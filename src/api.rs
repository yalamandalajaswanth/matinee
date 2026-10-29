//! Portal JSON API: video listing, folders, progress, rescan, subtitles.
//!
//! State-changing endpoints require `Content-Type: application/json` (via the `Json`
//! extractor) and there is deliberately no CORS layer, so a page on another origin cannot
//! issue them: the preflight fails.

use std::collections::BTreeMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::db::{Db, Progress, Video};
use crate::state::{AppState, Status};
use crate::subs::{self, SubError};

/// Fraction of the duration after which a video counts as watched.
pub const WATCHED_THRESHOLD: f64 = 0.9;

pub enum ApiError {
    NotFound,
    BadRequest(String),
    Status(StatusCode),
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Status(s) => (s, s.to_string()),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into()),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        // Wrong content type stays 415; malformed or mistyped bodies are 400.
        match r.status() {
            StatusCode::UNPROCESSABLE_ENTITY => ApiError::BadRequest(r.body_text()),
            StatusCode::BAD_REQUEST => ApiError::BadRequest(r.body_text()),
            other => ApiError::Status(other),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("request failed: {e:#}");
        ApiError::Internal
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn parse_id(id: &str) -> ApiResult<i64> {
    id.parse().map_err(|_| ApiError::NotFound)
}

fn find_video(state: &AppState, id: &str) -> ApiResult<Video> {
    let id = parse_id(id)?;
    state.snapshot().get(id).cloned().ok_or(ApiError::NotFound)
}

/// Run blocking database work off the async runtime.
async fn blocking<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&AppState) -> anyhow::Result<T> + Send + 'static,
) -> ApiResult<T> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || f(&state))
        .await
        .map_err(|e| ApiError::from(anyhow::anyhow!("task failed: {e}")))?
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------- video objects

#[derive(Debug, Serialize, PartialEq)]
pub struct VideoJson {
    pub id: i64,
    pub title: String,
    pub folder: String,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
    pub quest_playable: Option<bool>,
    pub size: i64,
    pub position: f64,
    pub watched: bool,
    pub has_subs: bool,
}

fn to_json(v: &Video, p: Progress, has_subs: bool) -> VideoJson {
    VideoJson {
        id: v.id,
        title: v.title.clone(),
        folder: v.folder.clone(),
        duration: v.duration,
        width: v.width,
        height: v.height,
        codec: v.codec.clone(),
        quest_playable: v.quest_playable,
        size: v.size,
        position: p.position,
        watched: p.watched,
        has_subs,
    }
}

/// Whether `folder` is `filter` or lies below it. An empty filter matches everything.
pub fn folder_matches(folder: &str, filter: &str) -> bool {
    let f = filter.trim_matches('/');
    f.is_empty()
        || folder == f
        || folder
            .strip_prefix(f)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Filter by search text and folder, sorted by folder then title (case-insensitive).
pub fn filter_videos<'a>(videos: &'a [Video], q: &str, folder: &str) -> Vec<&'a Video> {
    let q = q.trim().to_lowercase();
    let mut out: Vec<&Video> = videos
        .iter()
        .filter(|v| folder_matches(&v.folder, folder))
        .filter(|v| {
            q.is_empty()
                || v.title.to_lowercase().contains(&q)
                || v.folder.to_lowercase().contains(&q)
        })
        .collect();
    out.sort_by(|a, b| {
        (a.folder.to_lowercase(), a.title.to_lowercase(), a.id).cmp(&(
            b.folder.to_lowercase(),
            b.title.to_lowercase(),
            b.id,
        ))
    });
    out
}

#[derive(Debug, Serialize, PartialEq)]
pub struct FolderNode {
    pub name: String,
    pub path: String,
    pub count: usize,
    pub children: Vec<FolderNode>,
}

/// Folder tree with the root at path `""`; counts include descendants.
pub fn build_tree(videos: &[Video]) -> FolderNode {
    #[derive(Default)]
    struct Node {
        count: usize,
        children: BTreeMap<String, Node>,
    }
    let mut root = Node::default();
    for v in videos {
        root.count += 1;
        let mut node = &mut root;
        for part in v.folder.split('/').filter(|p| !p.is_empty()) {
            node = node.children.entry(part.to_string()).or_default();
            node.count += 1;
        }
    }
    fn convert(name: &str, path: &str, node: Node) -> FolderNode {
        let mut children: Vec<FolderNode> = node
            .children
            .into_iter()
            .map(|(n, child)| {
                let p = if path.is_empty() {
                    n.clone()
                } else {
                    format!("{path}/{n}")
                };
                convert(&n, &p, child)
            })
            .collect();
        children.sort_by_key(|c| c.name.to_lowercase());
        FolderNode {
            name: name.to_string(),
            path: path.to_string(),
            count: node.count,
            children,
        }
    }
    convert("Library", "", root)
}

/// Apply a saved position. At or beyond 90% of a known duration the video becomes watched
/// and the position resets. Returns `(position_to_store, mark_watched)`.
pub fn evaluate_progress(position: f64, duration: Option<f64>) -> Option<(f64, bool)> {
    if !position.is_finite() || position < 0.0 {
        return None;
    }
    match duration {
        Some(d) if d.is_finite() && d > 0.0 && position >= WATCHED_THRESHOLD * d => {
            Some((0.0, true))
        }
        _ => Some((position, false)),
    }
}

// -------------------------------------------------------------------- handlers

#[derive(Debug, Deserialize)]
pub struct VideosQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    folder: String,
}

pub async fn list_videos(
    State(state): State<AppState>,
    Query(query): Query<VideosQuery>,
) -> ApiResult<Json<Vec<VideoJson>>> {
    let progress = blocking(&state, |s| s.all_progress()).await?;
    let snap = state.snapshot();
    let out = filter_videos(snap.videos(), &query.q, &query.folder)
        .into_iter()
        .map(|v| {
            let p = progress.get(&v.id).copied().unwrap_or_default();
            to_json(v, p, snap.has_subs(v.id))
        })
        .collect();
    Ok(Json(out))
}

pub async fn folders(State(state): State<AppState>) -> Json<FolderNode> {
    Json(build_tree(state.snapshot().videos()))
}

async fn video_json(state: &AppState, video: &Video) -> ApiResult<VideoJson> {
    let id = video.id;
    let p = blocking(state, move |s| s.with_req_db(|db| db.get_progress(id))).await?;
    Ok(to_json(video, p, state.snapshot().has_subs(id)))
}

pub async fn get_video(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<VideoJson>> {
    let video = find_video(&state, &id)?;
    Ok(Json(video_json(&state, &video).await?))
}

#[derive(Debug, Deserialize)]
pub struct ProgressBody {
    position: f64,
}

pub async fn put_progress(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<ProgressBody>, JsonRejection>,
) -> ApiResult<Json<VideoJson>> {
    let video = find_video(&state, &id)?;
    let Json(body) = body?;
    let (pos, watched) = evaluate_progress(body.position, video.duration).ok_or_else(|| {
        ApiError::BadRequest("position must be a finite, non-negative number".into())
    })?;
    let vid = video.id;
    blocking(&state, move |s| {
        s.with_req_db(|db: &Db| db.set_position(vid, pos, watched))
    })
    .await?;
    Ok(Json(video_json(&state, &video).await?))
}

#[derive(Debug, Deserialize)]
pub struct WatchedBody {
    watched: bool,
}

pub async fn put_watched(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<WatchedBody>, JsonRejection>,
) -> ApiResult<Json<VideoJson>> {
    let video = find_video(&state, &id)?;
    let Json(body) = body?;
    let vid = video.id;
    blocking(&state, move |s| {
        s.with_req_db(|db: &Db| db.set_watched(vid, body.watched))
    })
    .await?;
    Ok(Json(video_json(&state, &video).await?))
}

/// `POST /api/rescan`. The body must be JSON (send `{}`): requiring the content type forces
/// a CORS preflight for cross-origin callers, which this server never approves.
pub async fn rescan(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(_) = body?;
    if state.trigger_rescan() {
        tracing::info!("rescan requested");
    }
    Ok(StatusCode::ACCEPTED)
}

pub async fn status(State(state): State<AppState>) -> Json<Status> {
    Json(state.status())
}

// ------------------------------------------------------------------- subtitles

pub async fn list_subs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<subs::SubTrack>>> {
    let video = find_video(&state, &id)?;
    let vid = video.id;
    let files = tokio::task::spawn_blocking(move || subs::find_for(&video.abs_path()))
        .await
        .map_err(|e| ApiError::from(anyhow::anyhow!("task failed: {e}")))?;
    let files: Vec<_> = files.into_iter().map(|(f, _)| f).collect();
    Ok(Json(subs::tracks(vid, &files)))
}

pub async fn subtitle_vtt(
    State(state): State<AppState>,
    Path((id, file)): Path<(String, String)>,
) -> ApiResult<Response> {
    let video = find_video(&state, &id)?;
    let index: usize = file
        .strip_suffix(".vtt")
        .and_then(|s| s.parse().ok())
        .ok_or(ApiError::NotFound)?;
    let result = tokio::task::spawn_blocking(move || {
        let found = subs::find_for(&video.abs_path());
        let (_, path) = found.get(index)?;
        Some(subs::read_vtt(path))
    })
    .await
    .map_err(|e| ApiError::from(anyhow::anyhow!("task failed: {e}")))?;
    match result {
        None => Err(ApiError::NotFound),
        Some(Err(SubError::TooLarge)) => Err(ApiError::Status(StatusCode::PAYLOAD_TOO_LARGE)),
        Some(Err(SubError::Io(e))) => {
            tracing::warn!("subtitle read failed: {e}");
            Err(ApiError::NotFound)
        }
        Some(Ok(text)) => Ok((
            [
                (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            text,
        )
            .into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: i64, folder: &str, title: &str) -> Video {
        Video {
            id,
            library_root: "/r".into(),
            rel_path: format!("{folder}/{title}.mp4"),
            title: title.into(),
            folder: folder.into(),
            size: 1,
            mtime: 1,
            duration: None,
            width: None,
            height: None,
            codec: None,
            quest_playable: None,
        }
    }

    fn sample() -> Vec<Video> {
        vec![
            v(1, "", "zeta"),
            v(2, "Movies", "Beta"),
            v(3, "Movies/Action", "alpha"),
            v(4, "MoviesX", "Gamma"),
            v(5, "Movies", "alpha two"),
        ]
    }

    fn ids(v: &[&Video]) -> Vec<i64> {
        v.iter().map(|v| v.id).collect()
    }

    #[test]
    fn folder_filter_semantics() {
        assert!(folder_matches("Movies", "Movies"));
        assert!(folder_matches("Movies/Action", "Movies"));
        assert!(!folder_matches("MoviesX", "Movies"));
        assert!(folder_matches("anything", ""));
        assert!(folder_matches("Movies/Action", "/Movies/"));
        assert!(!folder_matches("Movies", "Movies/Action"));
        let s = sample();
        assert_eq!(ids(&filter_videos(&s, "", "Movies")), vec![5, 2, 3]);
    }

    #[test]
    fn search_and_sort() {
        let s = sample();
        // Sorted by folder then title, case-insensitive; root folder first.
        assert_eq!(ids(&filter_videos(&s, "", "")), vec![1, 5, 2, 3, 4]);
        assert_eq!(ids(&filter_videos(&s, "ALPHA", "")), vec![5, 3]);
        // Search also matches the folder.
        assert_eq!(ids(&filter_videos(&s, "action", "")), vec![3]);
        assert_eq!(ids(&filter_videos(&s, "alpha", "Movies/Action")), vec![3]);
        assert!(filter_videos(&s, "nomatch", "").is_empty());
    }

    #[test]
    fn tree_counts_include_descendants() {
        let t = build_tree(&sample());
        assert_eq!((t.path.as_str(), t.count), ("", 5));
        let names: Vec<&str> = t.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Movies", "MoviesX"]);
        let movies = &t.children[0];
        assert_eq!((movies.path.as_str(), movies.count), ("Movies", 3));
        assert_eq!(movies.children[0].path, "Movies/Action");
        assert_eq!(movies.children[0].count, 1);
        assert_eq!(t.children[1].count, 1);
    }

    #[test]
    fn progress_threshold() {
        assert_eq!(evaluate_progress(10.0, Some(100.0)), Some((10.0, false)));
        assert_eq!(evaluate_progress(89.9, Some(100.0)), Some((89.9, false)));
        assert_eq!(evaluate_progress(90.0, Some(100.0)), Some((0.0, true)));
        assert_eq!(evaluate_progress(500.0, Some(100.0)), Some((0.0, true)));
        assert_eq!(evaluate_progress(500.0, None), Some((500.0, false)));
        assert_eq!(evaluate_progress(5.0, Some(0.0)), Some((5.0, false)));
        assert_eq!(evaluate_progress(-1.0, None), None);
        assert_eq!(evaluate_progress(f64::NAN, None), None);
        assert_eq!(evaluate_progress(f64::INFINITY, Some(10.0)), None);
    }
}
