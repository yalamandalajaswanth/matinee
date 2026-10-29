//! Streaming and thumbnail endpoints. Files are only ever resolved via numeric id.

use std::path::{Path as FsPath, PathBuf};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::db::Video;
use crate::state::AppState;

const THUMB_EXTS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

const PLACEHOLDER_SVG: &str = concat!(
    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180" width="320" height="180">"##,
    r##"<rect width="320" height="180" fill="#1e1e26"/>"##,
    r##"<circle cx="160" cy="90" r="34" fill="none" stroke="#6b6b80" stroke-width="4"/>"##,
    r##"<path d="M150 72 L150 108 L180 90 Z" fill="#6b6b80"/></svg>"##
);

fn lookup(state: &AppState, id: &str) -> Option<Video> {
    let id: i64 = id.parse().ok()?;
    state.snapshot().get(id).cloned()
}

async fn serve_file(path: PathBuf, req: Request) -> Response {
    match ServeFile::new(path).oneshot(req).await {
        Ok(resp) => resp.map(Body::new),
        Err(never) => match never {},
    }
}

/// `GET|HEAD /stream/{id}` with Range support.
pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let Some(video) = lookup(&state, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut resp = serve_file(video.abs_path(), req).await;
    if resp.status().is_success() {
        resp.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    }
    resp
}

/// Find a sidecar image next to the video: `<stem>.jpg|.jpeg|.png|.webp`.
pub fn find_sidecar_thumb(video_path: &FsPath) -> Option<PathBuf> {
    let stem = video_path.file_stem()?;
    let dir = video_path.parent()?;
    THUMB_EXTS.iter().find_map(|ext| {
        let mut name = stem.to_os_string();
        name.push(".");
        name.push(ext);
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

/// `GET /thumb/{id}`: sidecar image or built-in SVG placeholder.
pub async fn thumb(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let Some(video) = lookup(&state, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(path) = find_sidecar_thumb(&video.abs_path()) {
        let resp = serve_file(path, req).await;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_MODIFIED {
            return resp;
        }
    }
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        PLACEHOLDER_SVG,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path().join("movie.mp4");
        std::fs::write(&v, b"x").unwrap();
        assert!(find_sidecar_thumb(&v).is_none());
        std::fs::write(dir.path().join("movie.png"), b"x").unwrap();
        assert_eq!(find_sidecar_thumb(&v), Some(dir.path().join("movie.png")));
        std::fs::write(dir.path().join("movie.jpg"), b"x").unwrap();
        assert_eq!(find_sidecar_thumb(&v), Some(dir.path().join("movie.jpg")));
    }
}
