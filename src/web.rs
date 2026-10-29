//! Embedded frontend served at `/` and `/assets/*`.

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn serve(name: &str) -> Response {
    match Assets::get(name) {
        Some(file) => (
            [
                (header::CONTENT_TYPE, content_type(name)),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /`
pub async fn index() -> Response {
    serve("index.html")
}

/// `GET /assets/{name}`: only top-level embedded files; no path traversal possible.
pub async fn asset(Path(name): Path<String>) -> Response {
    if name.contains('/') || name.contains('\\') || name == "index.html" {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve(&name)
}
