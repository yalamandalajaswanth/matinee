//! SQLite storage for the video library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// A video known to the library.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    pub id: i64,
    pub library_root: String,
    pub rel_path: String,
    pub title: String,
    /// Relative parent folder with `/` separators; empty for files in the root.
    pub folder: String,
    pub size: i64,
    pub mtime: i64,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
    pub quest_playable: Option<bool>,
}

impl Video {
    /// Absolute path on disk. Only ever derived from a database row, never from request input.
    pub fn abs_path(&self) -> PathBuf {
        Path::new(&self.library_root).join(&self.rel_path)
    }
}

/// Size/mtime of an already-stored row, used to decide whether to re-read metadata.
#[derive(Debug, Clone, Copy)]
pub struct Existing {
    pub size: i64,
    pub mtime: i64,
}

/// Watch progress for one video.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Progress {
    pub position: f64,
    pub watched: bool,
}

pub struct Db {
    conn: Connection,
}

/// Current schema version stored in `PRAGMA user_version`.
/// 0 = M1 (videos table only), 1 = adds the `progress` table.
const SCHEMA_VERSION: i64 = 1;

const MIGRATION_1: &str = "
CREATE TABLE IF NOT EXISTS progress (
    video_id INTEGER PRIMARY KEY,
    position REAL NOT NULL DEFAULT 0,
    watched INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER
);";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS videos (
    id INTEGER PRIMARY KEY,
    library_root TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    title TEXT NOT NULL,
    folder TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    duration REAL,
    width INTEGER,
    height INTEGER,
    codec TEXT,
    quest_playable INTEGER,
    missing INTEGER NOT NULL DEFAULT 0,
    UNIQUE (library_root, rel_path)
);";

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        // WAL lets the request connection read and write while a scan transaction is open.
        // (In-memory databases silently stay in "memory" mode, which is fine.)
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    fn migrate(conn: &Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(MIGRATION_1)?;
        }
        if version < SCHEMA_VERSION {
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Progress for every video that has any, keyed by video id.
    pub fn all_progress(&self) -> Result<HashMap<i64, Progress>> {
        let mut stmt = self
            .conn
            .prepare("SELECT video_id, position, watched FROM progress")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                Progress {
                    position: r.get(1)?,
                    watched: r.get::<_, i64>(2)? != 0,
                },
            ))
        })?;
        Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
    }

    pub fn get_progress(&self, id: i64) -> Result<Progress> {
        Ok(self
            .conn
            .query_row(
                "SELECT position, watched FROM progress WHERE video_id = ?1",
                params![id],
                |r| {
                    Ok(Progress {
                        position: r.get(0)?,
                        watched: r.get::<_, i64>(1)? != 0,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Save a position. `mark_watched` sets the watched flag; otherwise it is left as is.
    pub fn set_position(&self, id: i64, position: f64, mark_watched: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO progress (video_id, position, watched, updated_at)
             VALUES (?1, ?2, ?3, strftime('%s','now'))
             ON CONFLICT (video_id) DO UPDATE SET
                position = excluded.position,
                watched = MAX(watched, excluded.watched),
                updated_at = excluded.updated_at",
            params![id, position, mark_watched],
        )?;
        Ok(())
    }

    /// Set the watched flag explicitly; the position is reset to 0.
    pub fn set_watched(&self, id: i64, watched: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO progress (video_id, position, watched, updated_at)
             VALUES (?1, 0, ?2, strftime('%s','now'))
             ON CONFLICT (video_id) DO UPDATE SET
                position = 0, watched = excluded.watched, updated_at = excluded.updated_at",
            params![id, watched],
        )?;
        Ok(())
    }

    /// Ids and (root, rel) of all rows not currently marked missing.
    pub fn present_keys(&self) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, library_root, rel_path FROM videos WHERE missing = 0")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn mark_missing(&self, id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE videos SET missing = 1 WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback(&self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }

    pub fn find(&self, root: &str, rel: &str) -> Result<Option<Existing>> {
        Ok(self
            .conn
            .query_row(
                "SELECT size, mtime FROM videos WHERE library_root = ?1 AND rel_path = ?2",
                params![root, rel],
                |r| {
                    Ok(Existing {
                        size: r.get(0)?,
                        mtime: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Insert or update a row (keeping its id) and clear the missing flag.
    pub fn upsert(&self, v: &Video) -> Result<()> {
        self.conn.execute(
            "INSERT INTO videos (library_root, rel_path, title, folder, size, mtime, duration,
                                 width, height, codec, quest_playable, missing)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)
             ON CONFLICT (library_root, rel_path) DO UPDATE SET
                title = excluded.title, folder = excluded.folder, size = excluded.size,
                mtime = excluded.mtime, duration = excluded.duration, width = excluded.width,
                height = excluded.height, codec = excluded.codec,
                quest_playable = excluded.quest_playable, missing = 0",
            params![
                v.library_root,
                v.rel_path,
                v.title,
                v.folder,
                v.size,
                v.mtime,
                v.duration,
                v.width,
                v.height,
                v.codec,
                v.quest_playable
            ],
        )?;
        Ok(())
    }

    /// Mark one row as present without touching its metadata.
    pub fn touch_present(&self, root: &str, rel: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE videos SET missing = 0 WHERE library_root = ?1 AND rel_path = ?2",
            params![root, rel],
        )?;
        Ok(())
    }

    /// All non-missing videos ordered by id.
    pub fn list_present(&self) -> Result<Vec<Video>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, library_root, rel_path, title, folder, size, mtime, duration, width,
                    height, codec, quest_playable
             FROM videos WHERE missing = 0 ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Video {
                id: r.get(0)?,
                library_root: r.get(1)?,
                rel_path: r.get(2)?,
                title: r.get(3)?,
                folder: r.get(4)?,
                size: r.get(5)?,
                mtime: r.get(6)?,
                duration: r.get(7)?,
                width: r.get(8)?,
                height: r.get(9)?,
                codec: r.get(10)?,
                quest_playable: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Count of all rows including missing ones.
    pub fn total_rows(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM videos", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_progress(7).unwrap(), Progress::default());
        db.set_position(7, 12.5, false).unwrap();
        assert_eq!(
            db.get_progress(7).unwrap(),
            Progress {
                position: 12.5,
                watched: false
            }
        );
        db.set_position(7, 0.0, true).unwrap();
        // A later plain save keeps the watched flag.
        db.set_position(7, 3.0, false).unwrap();
        let p = db.get_progress(7).unwrap();
        assert!(p.watched);
        assert_eq!(p.position, 3.0);
        db.set_watched(7, false).unwrap();
        assert_eq!(db.get_progress(7).unwrap(), Progress::default());
        assert_eq!(db.all_progress().unwrap().len(), 1);
    }

    #[test]
    fn migrates_m1_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        {
            // Exactly the M1 shape: videos table, user_version 0, no progress table.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA).unwrap();
            c.execute(
                "INSERT INTO videos (library_root, rel_path, title, folder, size, mtime)
                 VALUES ('/r', 'a.mp4', 'a', '', 1, 1)",
                [],
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(db.list_present().unwrap().len(), 1);
        db.set_position(1, 5.0, false).unwrap();
        assert_eq!(db.get_progress(1).unwrap().position, 5.0);
        drop(db);
        // Reopening is idempotent and keeps data.
        let db = Db::open(&path).unwrap();
        assert_eq!(db.get_progress(1).unwrap().position, 5.0);
    }
}
