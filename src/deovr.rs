//! DeoVR JSON endpoints.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use serde::Serialize;

use crate::db::Video;
use crate::state::AppState;

const ROOT_SCENE: &str = "Library";

#[derive(Debug, Serialize)]
pub struct Library {
    pub scenes: Vec<Scene>,
    pub authorized: u8,
}

#[derive(Debug, Serialize)]
pub struct Scene {
    pub name: String,
    pub list: Vec<VideoLink>,
}

#[derive(Debug, Serialize)]
pub struct VideoLink {
    pub title: String,
    #[serde(rename = "thumbnailUrl")]
    pub thumbnail_url: String,
    #[serde(rename = "videoLength", skip_serializing_if = "Option::is_none")]
    pub video_length: Option<u64>,
    pub video_url: String,
}

#[derive(Debug, Serialize)]
pub struct VideoDetail {
    pub id: i64,
    pub title: String,
    pub authorized: u8,
    pub is3d: bool,
    #[serde(rename = "screenType")]
    pub screen_type: &'static str,
    #[serde(rename = "stereoMode")]
    pub stereo_mode: &'static str,
    #[serde(rename = "videoLength", skip_serializing_if = "Option::is_none")]
    pub video_length: Option<u64>,
    #[serde(rename = "thumbnailUrl")]
    pub thumbnail_url: String,
    pub encodings: Vec<Encoding>,
}

#[derive(Debug, Serialize)]
pub struct Encoding {
    pub name: String,
    #[serde(rename = "videoSources")]
    pub video_sources: Vec<VideoSource>,
}

#[derive(Debug, Serialize)]
pub struct VideoSource {
    pub resolution: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    pub url: String,
}

fn length(v: &Video) -> Option<u64> {
    v.duration
        .filter(|d| d.is_finite() && *d >= 0.0)
        .map(|d| d.round() as u64)
}

pub fn build_library(videos: &[Video], base: &str) -> Library {
    let mut scenes: BTreeMap<String, Vec<&Video>> = BTreeMap::new();
    for v in videos {
        let name = if v.folder.is_empty() {
            ROOT_SCENE.to_string()
        } else {
            v.folder.clone()
        };
        scenes.entry(name).or_default().push(v);
    }
    let scenes = scenes
        .into_iter()
        .map(|(name, mut vs)| {
            vs.sort_by(|a, b| {
                a.title
                    .to_lowercase()
                    .cmp(&b.title.to_lowercase())
                    .then(a.id.cmp(&b.id))
            });
            Scene {
                name,
                list: vs
                    .into_iter()
                    .map(|v| VideoLink {
                        title: v.title.clone(),
                        thumbnail_url: format!("{base}/thumb/{}", v.id),
                        video_length: length(v),
                        video_url: format!("{base}/deovr/video/{}", v.id),
                    })
                    .collect(),
            }
        })
        .collect();
    Library {
        scenes,
        authorized: 0,
    }
}

pub fn build_detail(v: &Video, base: &str) -> VideoDetail {
    VideoDetail {
        id: v.id,
        title: v.title.clone(),
        authorized: 0,
        is3d: false,
        screen_type: "flat",
        stereo_mode: "off",
        video_length: length(v),
        thumbnail_url: format!("{base}/thumb/{}", v.id),
        encodings: vec![Encoding {
            name: v.codec.clone().unwrap_or_else(|| "unknown".to_string()),
            video_sources: vec![VideoSource {
                resolution: v.height.unwrap_or(1080),
                height: v.height,
                width: v.width,
                url: format!("{base}/stream/{}", v.id),
            }],
        }],
    }
}

/// `http://<Host header>`; `None` if the header is missing or unusable.
fn base_url(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?.trim();
    if host.is_empty() || host.contains(['/', ' ', '\\', '@']) {
        return None;
    }
    Some(format!("http://{host}"))
}

pub async fn library(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Library>, StatusCode> {
    let base = base_url(&headers).ok_or(StatusCode::BAD_REQUEST)?;
    let snap = state.snapshot();
    Ok(Json(build_library(snap.videos(), &base)))
}

pub async fn video(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<VideoDetail>, StatusCode> {
    let id: i64 = id.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    let base = base_url(&headers).ok_or(StatusCode::BAD_REQUEST)?;
    let snap = state.snapshot();
    let v = snap.get(id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(build_detail(v, &base)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vid(id: i64, folder: &str, title: &str) -> Video {
        Video {
            id,
            library_root: "/r".into(),
            rel_path: format!("{folder}/{title}.mp4"),
            title: title.into(),
            folder: folder.into(),
            size: 1,
            mtime: 0,
            duration: Some(61.6),
            width: Some(1920),
            height: Some(800),
            codec: Some("h264".into()),
            quest_playable: Some(true),
        }
    }

    #[test]
    fn library_shape() {
        let mut unknown = vid(3, "", "Root");
        unknown.duration = None;
        let vids = vec![vid(1, "Movies/Action", "B"), vid(2, "A", "C"), unknown];
        let j = serde_json::to_value(build_library(&vids, "http://h:1")).unwrap();
        assert_eq!(j["authorized"], 0);
        let names: Vec<_> = j["scenes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["A", "Library", "Movies/Action"]);
        let root = &j["scenes"][1]["list"][0];
        assert!(root.get("videoLength").is_none());
        assert_eq!(root["video_url"], "http://h:1/deovr/video/3");
        let first = &j["scenes"][0]["list"][0];
        assert_eq!(first["videoLength"], 62);
        assert_eq!(first["thumbnailUrl"], "http://h:1/thumb/2");
    }

    #[test]
    fn detail_shape_and_defaults() {
        let j = serde_json::to_value(build_detail(&vid(7, "", "T"), "http://h")).unwrap();
        assert_eq!(j["id"], 7);
        assert_eq!(j["is3d"], false);
        assert_eq!(j["screenType"], "flat");
        assert_eq!(j["stereoMode"], "off");
        assert_eq!(j["encodings"][0]["name"], "h264");
        let src = &j["encodings"][0]["videoSources"][0];
        assert_eq!(src["resolution"], 800);
        assert_eq!(src["url"], "http://h/stream/7");

        let mut v = vid(8, "", "U");
        v.height = None;
        v.width = None;
        v.codec = None;
        v.duration = None;
        let j = serde_json::to_value(build_detail(&v, "http://h")).unwrap();
        assert_eq!(j["encodings"][0]["name"], "unknown");
        assert_eq!(j["encodings"][0]["videoSources"][0]["resolution"], 1080);
        assert!(j.get("videoLength").is_none());
    }
}
