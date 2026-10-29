//! Sidecar subtitle discovery and SRT to WebVTT conversion.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Subtitle files larger than this are refused.
pub const MAX_SUB_BYTES: u64 = 5 * 1024 * 1024;

/// A subtitle sidecar found next to a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubFile {
    pub file_name: String,
    pub lang: Option<String>,
}

/// API shape of one subtitle track.
#[derive(Debug, Serialize)]
pub struct SubTrack {
    pub index: usize,
    pub lang: Option<String>,
    pub label: String,
    pub url: String,
}

#[derive(Debug)]
pub enum SubError {
    TooLarge,
    Io(std::io::Error),
}

fn is_lang_tag(s: &str) -> bool {
    let mut parts = s.split('-');
    let primary_ok = parts
        .next()
        .is_some_and(|p| (2..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphabetic()));
    let rest_ok =
        parts.all(|p| (2..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()));
    primary_ok && rest_ok
}

/// Pick the subtitle files for `stem` out of a directory listing, sorted by file name.
/// Accepts `<stem>.vtt|srt` and `<stem>.<lang>.vtt|srt`; extensions and the stem match
/// case-insensitively.
pub fn discover(stem: &str, file_names: &[String]) -> Vec<SubFile> {
    let mut out: Vec<SubFile> = file_names
        .iter()
        .filter_map(|name| {
            let (base, ext) = name.rsplit_once('.')?;
            if !(ext.eq_ignore_ascii_case("vtt") || ext.eq_ignore_ascii_case("srt")) {
                return None;
            }
            if base.eq_ignore_ascii_case(stem) {
                return Some(SubFile {
                    file_name: name.clone(),
                    lang: None,
                });
            }
            // `<stem>.<lang>`: split at the last dot and compare the stem part.
            let (head, lang) = base.rsplit_once('.')?;
            if head.eq_ignore_ascii_case(stem) && is_lang_tag(lang) {
                Some(SubFile {
                    file_name: name.clone(),
                    lang: Some(lang.to_lowercase()),
                })
            } else {
                None
            }
        })
        .collect();
    out.sort_by_key(|s| (s.file_name.to_lowercase(), s.file_name.clone()));
    out
}

/// Names of the regular files in `dir` (empty on error).
pub fn dir_file_names(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

/// Subtitle files next to a video, in stable order, with their absolute paths.
pub fn find_for(video_path: &Path) -> Vec<(SubFile, PathBuf)> {
    let (Some(stem), Some(dir)) = (video_path.file_stem(), video_path.parent()) else {
        return Vec::new();
    };
    discover(&stem.to_string_lossy(), &dir_file_names(dir))
        .into_iter()
        .map(|s| {
            let p = dir.join(&s.file_name);
            (s, p)
        })
        .collect()
}

/// API tracks for a video id.
pub fn tracks(id: i64, files: &[SubFile]) -> Vec<SubTrack> {
    files
        .iter()
        .enumerate()
        .map(|(index, f)| SubTrack {
            index,
            lang: f.lang.clone(),
            label: f.lang.clone().unwrap_or_else(|| "Subtitles".to_string()),
            url: format!("/subs/{id}/{index}.vtt"),
        })
        .collect()
}

/// Windows-1252 code points for bytes 0x80..=0x9F.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}', '\u{017D}', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{9D}', '\u{017E}', '\u{0178}',
];

fn decode_1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => CP1252_HIGH[usize::from(b - 0x80)],
            _ => char::from(b),
        })
        .collect()
}

/// Decode as UTF-8 (BOM stripped), falling back to Windows-1252.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => decode_1252(bytes),
    }
}

/// Convert SRT text bytes to WebVTT.
pub fn srt_to_vtt(bytes: &[u8]) -> String {
    let text = decode_text(bytes).replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(text.len() + 16);
    out.push_str("WEBVTT\n\n");
    for line in text.trim_start_matches('\n').lines() {
        if line.contains("-->") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Read a subtitle file and return WebVTT text.
pub fn read_vtt(path: &Path) -> Result<String, SubError> {
    let md = std::fs::metadata(path).map_err(SubError::Io)?;
    if md.len() > MAX_SUB_BYTES {
        return Err(SubError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(SubError::Io)?;
    let is_srt = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("srt"));
    if is_srt {
        Ok(srt_to_vtt(&bytes))
    } else {
        Ok(decode_text(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn discovery_patterns_and_order() {
        let files = names(&[
            "Movie.mp4",
            "movie.SRT",
            "Movie.en.vtt",
            "Movie.en-US.srt",
            "Movie.eng.srt",
            "Movie.english.srt",
            "Movie.x.srt",
            "Movie2.srt",
            "Other.vtt",
            "Movie.vtt",
            "Movie.txt",
        ]);
        let found = discover("Movie", &files);
        let got: Vec<(&str, Option<&str>)> = found
            .iter()
            .map(|s| (s.file_name.as_str(), s.lang.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Movie.en-US.srt", Some("en-us")),
                ("Movie.en.vtt", Some("en")),
                ("Movie.eng.srt", Some("eng")),
                ("movie.SRT", None),
                ("Movie.vtt", None),
            ]
        );
    }

    #[test]
    fn stems_with_dots_work() {
        let files = names(&["My.Movie.2020.en.srt", "My.Movie.2020.srt", "My.Movie.srt"]);
        let found = discover("My.Movie.2020", &files);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].lang.as_deref(), Some("en"));
    }

    #[test]
    fn tracks_have_urls_and_labels() {
        let files = discover("a", &names(&["a.vtt", "a.fr.srt"]));
        let t = tracks(9, &files);
        assert_eq!(t[0].url, "/subs/9/0.vtt");
        assert_eq!(t[0].lang.as_deref(), Some("fr"));
        assert_eq!(t[1].label, "Subtitles");
        assert_eq!(t[1].index, 1);
    }

    #[test]
    fn srt_bom_crlf_and_timestamps() {
        let srt = b"\xEF\xBB\xBF1\r\n00:00:01,000 --> 00:00:02,500\r\nHello, world\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000 line:90%\r\n1,5 apples\r\n";
        let vtt = srt_to_vtt(srt);
        assert_eq!(
            vtt,
            "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello, world\n\n2\n00:00:03.000 --> 00:00:04.000 line:90%\n1,5 apples\n"
        );
        assert!(!vtt.contains('\r'));
        assert!(!vtt.contains('\u{FEFF}'));
    }

    #[test]
    fn srt_windows_1252_fallback() {
        let vtt = srt_to_vtt(b"1\n00:00:01,000 --> 00:00:02,000\ncaf\xE9 \x93hi\x94\n");
        assert!(vtt.contains("caf\u{e9} \u{201C}hi\u{201D}"), "{vtt}");
    }

    #[test]
    fn vtt_passthrough_strips_bom() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.vtt");
        std::fs::write(
            &p,
            b"\xEF\xBB\xBFWEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhi\n",
        )
        .unwrap();
        let out = read_vtt(&p).unwrap();
        assert!(out.starts_with("WEBVTT"));
    }

    #[test]
    fn oversize_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.srt");
        std::fs::write(&p, vec![b'x'; (MAX_SUB_BYTES + 1) as usize]).unwrap();
        assert!(matches!(read_vtt(&p), Err(SubError::TooLarge)));
    }
}
