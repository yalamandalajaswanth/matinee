//! Router construction.

use axum::middleware;
use axum::routing::get;
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::AppState;
use crate::{deovr, media, net};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/deovr", get(deovr::library))
        .route("/deovr/video/{id}", get(deovr::video))
        .route("/stream/{id}", get(media::stream))
        .route("/thumb/{id}", get(media::thumb))
        .layer(middleware::from_fn(net::lan_only))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
