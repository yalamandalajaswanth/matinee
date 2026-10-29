//! Library scanning.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Result;
use walkdir::{DirEntry, WalkDir};

use crate::db::{Db, Video};
use crate::meta::{self, quest_playable};

/// An immutable view of the non-missing videos, indexed by id.
#[derive(Debug, Default)]
pub struct Snapshot {
    videos: Vec<Video>,
    index: HashMap<i64, usize>,
}

impl Snapshot {
    pub fn new(videos: Vec<Video>) -> Self {
        let index = videos.iter().enumerate().map(|(i, v)| (v.id, i)).collect();
        Self { videos, index }
    }

    pub fn videos(&self) -> &[Video] {
        &self.videos
    }

    pub fn get(&self, id: i64) -> Option<&Video> {
        self.index.get(&id).map(|&i| &self.videos[i])
    }

    pub fn len(&self) -> usize {
        self.videos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.videos.is_empty()
    }
}

/// Title from a file stem: `.` and `_` become spaces.
pub fn derive_title(stem: &str) -> String {
    stem.replace(['.', '_'], " ")
}

/// Names skipped at any depth: dot-prefixed, `$`-prefixed (e.g. `$RECYCLE.BIN`) and
/// `System Volume Information`.
fn is_hidden_name(name: &str) -> bool {
    name.starts_with(['.', '$']) || name.eq_ignore_ascii_case("System Volume Information")
}

#[cfg(windows)]
fn has_hidden_attrs(entry: &DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
    entry
        .metadata()
        .is_ok_and(|m| m.file_attributes() & HIDDEN_OR_SYSTEM != 0)
}

#[cfg(not(windows))]
fn has_hidden_attrs(_entry: &DirEntry) -> bool {
    false
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry.depth() > 0
        && (is_hidden_name(&entry.file_name().to_string_lossy()) || has_hidden_attrs(entry))
}

fn is_video_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4") || e.eq_ignore_ascii_case("m4v"))
}

fn mtime_millis(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Scan all libraries, update the database, and return a fresh snapshot.
pub fn scan(db: &Db, libraries: &[PathBuf]) -> Result<Snapshot> {
    db.begin()?;
    match scan_inner(db, libraries) {
        Ok(()) => db.commit()?,
        Err(e) => {
            db.rollback();
            return Err(e);
        }
    }
    Ok(Snapshot::new(db.list_present()?))
}

fn scan_inner(db: &Db, libraries: &[PathBuf]) -> Result<()> {
    db.mark_all_missing()?;
    for root in libraries {
        let root_str = root.to_string_lossy().into_owned();
        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_hidden(e));
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("scan error: {e}");
                    continue;
                }
            };
            if !entry.file_type().is_file() || !is_video_ext(entry.path()) {
                continue;
            }
            if let Err(e) = index_file(db, root, &root_str, &entry) {
                tracing::warn!("failed to index {}: {e}", entry.path().display());
            }
        }
    }
    Ok(())
}

fn index_file(db: &Db, root: &Path, root_str: &str, entry: &DirEntry) -> Result<()> {
    let Ok(rel) = entry.path().strip_prefix(root) else {
        return Ok(());
    };
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some((_, dirs)) = parts.split_last() else {
        return Ok(());
    };
    let rel_path = parts.join("/");
    let folder = dirs.join("/");

    let md = entry.metadata()?;
    let size = i64::try_from(md.len()).unwrap_or(i64::MAX);
    let mtime = mtime_millis(&md);

    if let Some(existing) = db.find(root_str, &rel_path)? {
        if existing.size == size && existing.mtime == mtime {
            return db.touch_present(root_str, &rel_path);
        }
    }

    let stem = entry
        .path()
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let m = meta::read_meta(entry.path());
    let playable = quest_playable(m.codec.as_deref());
    db.upsert(&Video {
        id: 0,
        library_root: root_str.to_string(),
        rel_path,
        title: derive_title(&stem),
        folder,
        size,
        mtime,
        duration: m.duration,
        width: m.width,
        height: m.height,
        codec: m.codec,
        quest_playable: playable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn rels(s: &Snapshot) -> Vec<String> {
        let mut v: Vec<String> = s.videos().iter().map(|v| v.rel_path.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn title_derivation() {
        assert_eq!(derive_title("My_Movie.2020.1080p"), "My Movie 2020 1080p");
        assert_eq!(derive_title("plain"), "plain");
    }

    #[test]
    fn filters_extensions_hidden_and_nesting() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        touch(r, "a.mp4", b"1");
        touch(r, "B.MP4", b"1");
        touch(r, "c.M4v", b"1");
        touch(r, "d.mkv", b"1");
        touch(r, "e.txt", b"1");
        touch(r, ".hidden.mp4", b"1");
        touch(r, ".secret/x.mp4", b"1");
        touch(r, "Movies/Action/deep.mp4", b"1");
        touch(r, "Movies/.cache/y.mp4", b"1");
        let db = Db::open_in_memory().unwrap();
        let snap = scan(&db, &[r.to_path_buf()]).unwrap();
        assert_eq!(
            rels(&snap),
            vec!["B.MP4", "Movies/Action/deep.mp4", "a.mp4", "c.M4v"]
        );
        let deep = snap.videos().iter().find(|v| v.title == "deep").unwrap();
        assert_eq!(deep.folder, "Movies/Action");
        let a = snap
            .videos()
            .iter()
            .find(|v| v.rel_path == "a.mp4")
            .unwrap();
        assert_eq!(a.folder, "");
        assert_eq!(a.codec, None);
        assert_eq!(a.quest_playable, None);
        assert_eq!(a.duration, None);
    }

    #[test]
    fn skips_recycle_bin_and_system_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        touch(r, "ok.mp4", b"1");
        touch(r, "$RECYCLE.BIN/S-1-5/deleted.mp4", b"1");
        touch(r, "System Volume Information/x.mp4", b"1");
        touch(r, "$weird.mp4", b"1");
        let db = Db::open_in_memory().unwrap();
        let snap = scan(&db, &[r.to_path_buf()]).unwrap();
        assert_eq!(rels(&snap), vec!["ok.mp4"]);
    }

    #[cfg(windows)]
    #[test]
    fn skips_windows_hidden_attribute() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        touch(r, "ok.mp4", b"1");
        touch(r, "secret/inner.mp4", b"1");
        touch(r, "hid.mp4", b"1");
        for p in ["secret", "hid.mp4"] {
            let status = std::process::Command::new("attrib")
                .arg("+h")
                .arg(r.join(p))
                .status()
                .unwrap();
            assert!(status.success());
        }
        let db = Db::open_in_memory().unwrap();
        let snap = scan(&db, &[r.to_path_buf()]).unwrap();
        assert_eq!(rels(&snap), vec!["ok.mp4"]);
    }
    #[test]
    fn ids_stable_and_missing_handled() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        touch(r, "one.mp4", b"1");
        touch(r, "two.mp4", b"22");
        let db = Db::open_in_memory().unwrap();
        let libs = [r.to_path_buf()];
        let s1 = scan(&db, &libs).unwrap();
        let id_of =
            |s: &Snapshot, n: &str| s.videos().iter().find(|v| v.rel_path == n).map(|v| v.id);
        let (one, two) = (
            id_of(&s1, "one.mp4").unwrap(),
            id_of(&s1, "two.mp4").unwrap(),
        );
        assert_ne!(one, two);

        // Rescan unchanged and with a modified file: ids stay.
        touch(r, "two.mp4", b"2222");
        touch(r, "three.mp4", b"3");
        let s2 = scan(&db, &libs).unwrap();
        assert_eq!(id_of(&s2, "one.mp4"), Some(one));
        assert_eq!(id_of(&s2, "two.mp4"), Some(two));
        assert_eq!(s2.get(two).unwrap().size, 4);
        let three = id_of(&s2, "three.mp4").unwrap();

        // Remove: hidden from snapshot but row kept.
        fs::remove_file(r.join("one.mp4")).unwrap();
        let s3 = scan(&db, &libs).unwrap();
        assert!(s3.get(one).is_none());
        assert_eq!(s3.len(), 2);
        assert_eq!(db.total_rows().unwrap(), 3);

        // Comes back with the same id.
        touch(r, "one.mp4", b"1");
        let s4 = scan(&db, &libs).unwrap();
        assert_eq!(id_of(&s4, "one.mp4"), Some(one));
        assert_eq!(id_of(&s4, "three.mp4"), Some(three));
    }

    #[test]
    fn ids_stable_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        touch(&lib, "x.mp4", b"1");
        let dbp = dir.path().join("library.db");
        let id1 = {
            let db = Db::open(&dbp).unwrap();
            scan(&db, std::slice::from_ref(&lib)).unwrap().videos()[0].id
        };
        let db = Db::open(&dbp).unwrap();
        let id2 = scan(&db, std::slice::from_ref(&lib)).unwrap().videos()[0].id;
        assert_eq!(id1, id2);
    }
}
