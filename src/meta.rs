//! Minimal MP4 header reader: duration, video dimensions and codec.
//!
//! Top-level boxes are walked with seeks (`mdat` is never read); only `moov` is loaded.
//! All parsing is bounds-checked so malformed input yields `None`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Refuse to load a `moov` box larger than this.
const MAX_MOOV: u64 = 64 * 1024 * 1024;

/// Metadata read from an MP4 file; every field is optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
}

/// Read metadata; any failure (including a panic) yields empty metadata.
pub fn read_meta(path: &Path) -> Meta {
    let path = path.to_path_buf();
    std::panic::catch_unwind(move || try_read(&path).unwrap_or_default()).unwrap_or_default()
}

fn try_read(path: &Path) -> Option<Meta> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut pos = 0u64;
    while pos.checked_add(8)? <= len {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        let size32 = u64::from(u32::from_be_bytes(head[0..4].try_into().ok()?));
        let typ: [u8; 4] = head[4..8].try_into().ok()?;
        let (size, header_len) = match size32 {
            1 => {
                let mut ext = [0u8; 8];
                file.read_exact(&mut ext).ok()?;
                (u64::from_be_bytes(ext), 16)
            }
            0 => (len - pos, 8),
            n => (n, 8),
        };
        if size < header_len || pos.checked_add(size)? > len {
            return None;
        }
        if &typ == b"moov" {
            let body_len = size - header_len;
            if body_len > MAX_MOOV {
                return None;
            }
            let mut body = vec![0u8; usize::try_from(body_len).ok()?];
            file.read_exact(&mut body).ok()?;
            return parse_moov(&body);
        }
        pos += size;
    }
    None
}

/// Iterate the child boxes of `data` as `(fourcc, body)`; stops at the first malformed box.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let size32 = u64::from(u32::from_be_bytes(rest.get(0..4)?.try_into().ok()?));
        let typ: [u8; 4] = rest.get(4..8)?.try_into().ok()?;
        let (size, header_len) = match size32 {
            1 => (u64::from_be_bytes(rest.get(8..16)?.try_into().ok()?), 16),
            0 => (rest.len() as u64, 8),
            n => (n, 8),
        };
        if size < header_len {
            rest = &[];
            return None;
        }
        let size = usize::try_from(size).ok()?;
        let body = rest.get(usize::try_from(header_len).ok()?..size)?;
        rest = &rest[size..];
        Some((typ, body))
    })
}

fn child<'a>(data: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(t, _)| t == typ).map(|(_, b)| b)
}

fn be32(d: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        d.get(off..off.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn be64(d: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        d.get(off..off.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn be16(d: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        d.get(off..off.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn parse_moov(moov: &[u8]) -> Option<Meta> {
    let mut meta = Meta::default();
    if let Some(mvhd) = child(moov, b"mvhd") {
        meta.duration = parse_mvhd(mvhd);
    }
    for (typ, trak) in boxes(moov) {
        if &typ != b"trak" {
            continue;
        }
        if let Some(v) = parse_video_trak(trak) {
            meta.width = v.width;
            meta.height = v.height;
            meta.codec = Some(v.codec);
            break;
        }
    }
    Some(meta)
}

fn parse_mvhd(d: &[u8]) -> Option<f64> {
    let (timescale, duration) = if *d.first()? == 1 {
        (be32(d, 20)?, be64(d, 24)?)
    } else {
        (be32(d, 12)?, u64::from(be32(d, 16)?))
    };
    (timescale > 0).then(|| duration as f64 / f64::from(timescale))
}

struct VideoTrack {
    width: Option<u32>,
    height: Option<u32>,
    codec: String,
}

fn parse_video_trak(trak: &[u8]) -> Option<VideoTrack> {
    let mdia = child(trak, b"mdia")?;
    let hdlr = child(mdia, b"hdlr")?;
    if hdlr.get(8..12)? != b"vide" {
        return None;
    }
    let (mut w, mut h) = (0u32, 0u32);
    if let Some(tkhd) = child(trak, b"tkhd") {
        let off = if *tkhd.first()? == 1 { 88 } else { 76 };
        w = be32(tkhd, off).unwrap_or(0) >> 16;
        h = be32(tkhd, off + 4).unwrap_or(0) >> 16;
    }
    let mut codec = "unknown".to_string();
    let stsd = child(mdia, b"minf")
        .and_then(|m| child(m, b"stbl"))
        .and_then(|s| child(s, b"stsd"));
    if let Some(stsd) = stsd {
        // version/flags (4) + entry_count (4), then sample entries.
        if let Some((fourcc, entry)) = stsd.get(8..).and_then(|e| boxes(e).next()) {
            codec = codec_name(&fourcc);
            if w == 0 || h == 0 {
                w = u32::from(be16(entry, 24).unwrap_or(0));
                h = u32::from(be16(entry, 26).unwrap_or(0));
            }
        }
    }
    Some(VideoTrack {
        width: (w > 0 && h > 0).then_some(w),
        height: (w > 0 && h > 0).then_some(h),
        codec,
    })
}

fn codec_name(fourcc: &[u8; 4]) -> String {
    match fourcc {
        b"avc1" | b"avc3" => "h264".to_string(),
        b"hvc1" | b"hev1" => "h265".to_string(),
        b"av01" => "av1".to_string(),
        b"vp09" => "vp9".to_string(),
        b"mp4v" => "mpeg4".to_string(),
        other => {
            let s: String = other
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() {
                        char::from(b).to_ascii_lowercase()
                    } else {
                        '?'
                    }
                })
                .collect();
            s
        }
    }
}

/// Whether the Quest 3 can hardware-decode the codec. `None` when the codec is unknown.
pub fn quest_playable(codec: Option<&str>) -> Option<bool> {
    match codec? {
        "unknown" | "" => None,
        "h264" | "h265" | "av1" | "vp9" => Some(true),
        _ => Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(body);
        v
    }

    fn bx64(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = 1u32.to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(&((body.len() + 16) as u64).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    fn mvhd(version: u8, timescale: u32, duration: u64) -> Vec<u8> {
        let mut b = vec![version, 0, 0, 0];
        if version == 1 {
            b.extend_from_slice(&[0; 16]);
            b.extend_from_slice(&timescale.to_be_bytes());
            b.extend_from_slice(&duration.to_be_bytes());
        } else {
            b.extend_from_slice(&[0; 8]);
            b.extend_from_slice(&timescale.to_be_bytes());
            b.extend_from_slice(&(duration as u32).to_be_bytes());
        }
        b.extend_from_slice(&[0; 80]);
        bx(b"mvhd", &b)
    }

    fn tkhd(version: u8, w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![version, 0, 0, 0];
        b.extend_from_slice(&vec![0; if version == 1 { 32 } else { 20 }]);
        b.extend_from_slice(&[0; 52]);
        b.extend_from_slice(&(w << 16).to_be_bytes());
        b.extend_from_slice(&(h << 16).to_be_bytes());
        bx(b"tkhd", &b)
    }

    fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
        let mut b = vec![0; 8];
        b.extend_from_slice(kind);
        b.extend_from_slice(&[0; 12]);
        bx(b"hdlr", &b)
    }

    fn trak(
        kind: &[u8; 4],
        fourcc: Option<&[u8; 4]>,
        tk: Vec<u8>,
        entry_wh: (u16, u16),
    ) -> Vec<u8> {
        let mut stbl = Vec::new();
        if let Some(f) = fourcc {
            let mut body = vec![0u8; 24];
            body.extend_from_slice(&entry_wh.0.to_be_bytes());
            body.extend_from_slice(&entry_wh.1.to_be_bytes());
            body.extend_from_slice(&[0; 50]);
            let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
            stsd.extend(bx(f, &body));
            stbl = bx(b"stsd", &stsd);
        }
        let minf = bx(b"minf", &bx(b"stbl", &stbl));
        let mut mdia = bx(b"mdhd", &[0; 24]);
        mdia.extend(hdlr(kind));
        mdia.extend(minf);
        let mut t = tk;
        t.extend(bx(b"mdia", &mdia));
        bx(b"trak", &t)
    }

    fn video_trak(fourcc: &[u8; 4]) -> Vec<u8> {
        trak(b"vide", Some(fourcc), tkhd(0, 1920, 1080), (0, 0))
    }

    fn write(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.mp4");
        std::fs::write(&p, bytes).unwrap();
        (dir, p)
    }

    fn moov(mvhd_box: Vec<u8>, traks: Vec<Vec<u8>>) -> Vec<u8> {
        let mut body = mvhd_box;
        for t in traks {
            body.extend(t);
        }
        bx(b"moov", &body)
    }

    #[test]
    fn codec_mapping() {
        for c in ["h264", "h265", "av1", "vp9"] {
            assert_eq!(quest_playable(Some(c)), Some(true), "{c}");
        }
        assert_eq!(quest_playable(Some("mpeg4")), Some(false));
        assert_eq!(quest_playable(Some("unknown")), None);
        assert_eq!(quest_playable(None), None);
    }

    #[test]
    fn moov_before_and_after_mdat() {
        let ftyp = bx(b"ftyp", b"isom\0\0\0\0isom");
        let mdat = bx(b"mdat", &[7u8; 5000]);
        let m = moov(mvhd(0, 1000, 90_000), vec![video_trak(b"avc1")]);
        for order in [[&ftyp, &m, &mdat], [&ftyp, &mdat, &m]] {
            let bytes: Vec<u8> = order.iter().flat_map(|b| b.iter().copied()).collect();
            let (_d, p) = write(&bytes);
            let meta = read_meta(&p);
            assert_eq!(meta.duration, Some(90.0));
            assert_eq!((meta.width, meta.height), (Some(1920), Some(1080)));
            assert_eq!(meta.codec.as_deref(), Some("h264"));
        }
    }

    #[test]
    fn largesize_and_size_zero_boxes() {
        let ftyp = bx(b"ftyp", b"isom");
        let m = moov(mvhd(0, 10, 25), vec![video_trak(b"av01")]);
        // 64-bit mdat before moov.
        let mut bytes = ftyp.clone();
        bytes.extend(bx64(b"mdat", &[1u8; 100]));
        bytes.extend(&m);
        let (_d, p) = write(&bytes);
        assert_eq!(read_meta(&p).codec.as_deref(), Some("av1"));
        // 64-bit moov itself.
        let body = &m[8..];
        let mut bytes = ftyp.clone();
        bytes.extend(bx64(b"moov", body));
        let (_d, p) = write(&bytes);
        assert_eq!(read_meta(&p).duration, Some(2.5));
        // size==0 moov extends to EOF.
        let mut bytes = ftyp;
        bytes.extend(0u32.to_be_bytes());
        bytes.extend(b"moov");
        bytes.extend(body);
        let (_d, p) = write(&bytes);
        assert_eq!(read_meta(&p).codec.as_deref(), Some("av1"));
    }

    #[test]
    fn mvhd_v1() {
        let m = moov(mvhd(1, 600, 6000 * 3), vec![video_trak(b"vp09")]);
        let (_d, p) = write(&m);
        let meta = read_meta(&p);
        assert_eq!(meta.duration, Some(30.0));
        assert_eq!(meta.codec.as_deref(), Some("vp9"));
        let m = moov(
            mvhd(0, 1000, 1000),
            vec![trak(b"vide", Some(b"hvc1"), tkhd(1, 3840, 2160), (0, 0))],
        );
        let (_d, p) = write(&m);
        let meta = read_meta(&p);
        assert_eq!((meta.width, meta.height), (Some(3840), Some(2160)));
    }

    #[test]
    fn fourcc_mapping() {
        let cases: [(&[u8; 4], &str, Option<bool>); 8] = [
            (b"avc1", "h264", Some(true)),
            (b"avc3", "h264", Some(true)),
            (b"hvc1", "h265", Some(true)),
            (b"hev1", "h265", Some(true)),
            (b"av01", "av1", Some(true)),
            (b"vp09", "vp9", Some(true)),
            (b"mp4v", "mpeg4", Some(false)),
            (b"XVID", "xvid", Some(false)),
        ];
        for (f, name, playable) in cases {
            let m = moov(mvhd(0, 1000, 1000), vec![video_trak(f)]);
            let (_d, p) = write(&m);
            let meta = read_meta(&p);
            assert_eq!(meta.codec.as_deref(), Some(name));
            assert_eq!(quest_playable(meta.codec.as_deref()), playable);
        }
    }

    #[test]
    fn no_stsd_is_unknown_and_entry_size_fallback() {
        let t = trak(b"vide", None, tkhd(0, 0, 0), (0, 0));
        let (_d, p) = write(&moov(mvhd(0, 1000, 1000), vec![t]));
        let meta = read_meta(&p);
        assert_eq!(meta.codec.as_deref(), Some("unknown"));
        assert_eq!(quest_playable(meta.codec.as_deref()), None);
        assert_eq!(meta.width, None);

        let t = trak(b"vide", Some(b"avc1"), tkhd(0, 0, 0), (1280, 720));
        let (_d, p) = write(&moov(mvhd(0, 1000, 1000), vec![t]));
        let meta = read_meta(&p);
        assert_eq!((meta.width, meta.height), (Some(1280), Some(720)));
    }

    #[test]
    fn audio_only_has_no_codec() {
        let t = trak(b"soun", Some(b"mp4a"), tkhd(0, 0, 0), (0, 0));
        let (_d, p) = write(&moov(mvhd(0, 1000, 5000), vec![t]));
        let meta = read_meta(&p);
        assert_eq!(meta.duration, Some(5.0));
        assert_eq!(meta.codec, None);
        assert_eq!(meta.width, None);
    }

    #[test]
    fn garbage_and_truncated_input() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_meta(&dir.path().join("missing.mp4")), Meta::default());
        let good = moov(mvhd(0, 1000, 1000), vec![video_trak(b"avc1")]);
        let mut inputs: Vec<Vec<u8>> = vec![
            b"not an mp4 at all".to_vec(),
            vec![],
            vec![0xff; 64],
            0xffff_ffffu32
                .to_be_bytes()
                .iter()
                .chain(b"moov")
                .copied()
                .collect(),
        ];
        for n in 0..good.len() {
            inputs.push(good[..n].to_vec());
        }
        for (i, bytes) in inputs.iter().enumerate() {
            let p = dir.path().join(format!("{i}.mp4"));
            std::fs::write(&p, bytes).unwrap();
            assert_eq!(
                try_read(&p).unwrap_or_default(),
                Meta::default(),
                "input {i}"
            );
        }
    }
}
