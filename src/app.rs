//! Router construction.

use axum::middleware;
use axum::routing::{get, post, put};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::AppState;
use crate::{api, deovr, media, net, web};

/// No CORS layer is added on purpose: state-changing endpoints need a JSON content type,
/// so cross-origin browsers are stopped by the failed preflight.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(web::index))
        .route("/assets/{name}", get(web::asset))
        .route("/deovr", get(deovr::library))
        .route("/deovr/video/{id}", get(deovr::video))
        .route("/stream/{id}", get(media::stream))
        .route("/thumb/{id}", get(media::thumb))
        .route("/subs/{id}", get(api::list_subs))
        .route("/subs/{id}/{file}", get(api::subtitle_vtt))
        .route("/api/videos", get(api::list_videos))
        .route("/api/videos/{id}", get(api::get_video))
        .route("/api/videos/{id}/subs", get(api::list_subs))
        .route("/api/videos/{id}/progress", put(api::put_progress))
        .route("/api/videos/{id}/watched", put(api::put_watched))
        .route("/api/folders", get(api::folders))
        .route("/api/rescan", post(api::rescan))
        .route("/api/status", get(api::status))
        .layer(middleware::from_fn(net::lan_only))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
