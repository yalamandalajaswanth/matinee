use std::net::SocketAddr;
use std::path::Path;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use deovr_server::app::build_router;
use deovr_server::db::Db;
use deovr_server::state::AppState;
use http_body_util::BodyExt;
use tower::ServiceExt;

const LAN: &str = "192.168.1.20:5555";

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn setup() -> (tempfile::TempDir, Router, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    write(dir.path(), "My_Movie.mp4", &data);
    write(dir.path(), "Action/Fight.2020.MP4", b"junk bytes");
    write(dir.path(), "Action/Fight.2020.png", b"png");
    let state = AppState::new(
        vec![dir.path().to_path_buf()],
        Db::open_in_memory().unwrap(),
    );
    state.rescan_blocking().unwrap();
    (dir, build_router(state), data)
}

fn req(method: Method, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "192.168.1.5:8080")
}

async fn send(
    router: &Router,
    b: axum::http::request::Builder,
    peer: &str,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut r = b.body(Body::empty()).unwrap();
    let addr: SocketAddr = peer.parse().unwrap();
    r.extensions_mut().insert(ConnectInfo(addr));
    let resp = router.clone().oneshot(r).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (parts.status, parts.headers, bytes)
}

async fn get_json(router: &Router, uri: &str) -> serde_json::Value {
    let (s, _, body) = send(router, req(Method::GET, uri), LAN).await;
    assert_eq!(s, StatusCode::OK, "{uri}");
    serde_json::from_slice(&body).unwrap()
}

/// Returns (movie_id, fight_id) via the library listing.
async fn ids(router: &Router) -> (String, String) {
    let lib = get_json(router, "/deovr").await;
    let mut movie = String::new();
    let mut fight = String::new();
    for scene in lib["scenes"].as_array().unwrap() {
        for item in scene["list"].as_array().unwrap() {
            let id = item["video_url"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string();
            match item["title"].as_str().unwrap() {
                "My Movie" => movie = id,
                "Fight 2020" => fight = id,
                t => panic!("unexpected title {t}"),
            }
        }
    }
    (movie, fight)
}

#[tokio::test]
async fn deovr_urls_use_host_header_and_junk_files_listed() {
    let (_d, router, _) = setup();
    let lib = get_json(&router, "/deovr").await;
    assert_eq!(lib["authorized"], 0);
    let names: Vec<&str> = lib["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Action", "Library"]);
    let item = &lib["scenes"][1]["list"][0];
    assert_eq!(item["title"], "My Movie");
    assert!(item.get("videoLength").is_none());
    let url = item["video_url"].as_str().unwrap();
    assert!(
        url.starts_with("http://192.168.1.5:8080/deovr/video/"),
        "{url}"
    );
    assert!(item["thumbnailUrl"]
        .as_str()
        .unwrap()
        .starts_with("http://192.168.1.5:8080/thumb/"));

    // A different Host header changes every URL.
    let b = Request::builder()
        .uri("/deovr")
        .header(header::HOST, "quest.local:9000");
    let (s, _, body) = send(&router, b, LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("http://quest.local:9000/deovr/video/"));
}

#[tokio::test]
async fn video_detail_fields() {
    let (_d, router, _) = setup();
    let (movie, _) = ids(&router).await;
    let v = get_json(&router, &format!("/deovr/video/{movie}")).await;
    assert_eq!(v["id"].as_i64().unwrap().to_string(), movie);
    assert_eq!(v["title"], "My Movie");
    assert_eq!(v["is3d"], false);
    assert_eq!(v["screenType"], "flat");
    assert_eq!(v["stereoMode"], "off");
    assert_eq!(v["encodings"][0]["name"], "unknown");
    let src = &v["encodings"][0]["videoSources"][0];
    assert_eq!(src["resolution"], 1080);
    assert_eq!(
        src["url"],
        format!("http://192.168.1.5:8080/stream/{movie}")
    );
    assert_eq!(
        v["thumbnailUrl"],
        format!("http://192.168.1.5:8080/thumb/{movie}")
    );
}

#[tokio::test]
async fn range_request_returns_206() {
    let (_d, router, data) = setup();
    let (movie, _) = ids(&router).await;
    let b = req(Method::GET, &format!("/stream/{movie}")).header(header::RANGE, "bytes=10-19");
    let (s, h, body) = send(&router, b, LAN).await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(h[header::CONTENT_RANGE], "bytes 10-19/1000");
    assert_eq!(h[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(h[header::ACCEPT_RANGES], "bytes");
    assert_eq!(body, &data[10..20]);

    // Full GET and HEAD.
    let (s, h, body) = send(&router, req(Method::GET, &format!("/stream/{movie}")), LAN).await;
    assert_eq!((s, body.len()), (StatusCode::OK, 1000));
    assert_eq!(h[header::CONTENT_TYPE], "video/mp4");
    let (s, h, body) = send(&router, req(Method::HEAD, &format!("/stream/{movie}")), LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_LENGTH], "1000");
    assert!(body.is_empty());
}

#[tokio::test]
async fn unknown_ids_are_404() {
    let (_d, router, _) = setup();
    for uri in [
        "/stream/99999",
        "/thumb/99999",
        "/deovr/video/99999",
        "/stream/abc",
        "/deovr/video/..%2Fx",
    ] {
        let (s, _, _) = send(&router, req(Method::GET, uri), LAN).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn thumbnails_sidecar_and_placeholder() {
    let (_d, router, _) = setup();
    let (movie, fight) = ids(&router).await;
    let (s, h, body) = send(&router, req(Method::GET, &format!("/thumb/{movie}")), LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/svg+xml");
    assert!(String::from_utf8(body).unwrap().starts_with("<svg"));
    let (s, h, body) = send(&router, req(Method::GET, &format!("/thumb/{fight}")), LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");
    assert_eq!(body, b"png");
}

#[tokio::test]
async fn non_private_peer_is_forbidden() {
    let (_d, router, _) = setup();
    for peer in ["8.8.8.8:1234", "[2001:4860:4860::8888]:80"] {
        let (s, _, _) = send(&router, req(Method::GET, "/deovr"), peer).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{peer}");
    }
    // Missing connect info fails closed.
    let r = req(Method::GET, "/deovr").body(Body::empty()).unwrap();
    let resp = router.clone().oneshot(r).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    // Loopback is fine.
    let (s, _, _) = send(&router, req(Method::GET, "/deovr"), "127.0.0.1:1").await;
    assert_eq!(s, StatusCode::OK);
}

fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(typ);
    v.extend_from_slice(body);
    v
}

/// Minimal MP4 with one hvc1 video track, 1280x720, 90.5 s.
fn synthetic_hvc1() -> Vec<u8> {
    let mut mvhd = vec![0u8; 12];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&90_500u32.to_be_bytes());
    mvhd.extend_from_slice(&[0; 80]);
    let mut tkhd = vec![0u8; 76];
    tkhd.extend_from_slice(&(1280u32 << 16).to_be_bytes());
    tkhd.extend_from_slice(&(720u32 << 16).to_be_bytes());
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0; 12]);
    let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend(bx(b"hvc1", &[0u8; 78]));
    let minf = bx(b"minf", &bx(b"stbl", &bx(b"stsd", &stsd)));
    let mut mdia = bx(b"mdhd", &[0; 24]);
    mdia.extend(bx(b"hdlr", &hdlr));
    mdia.extend(minf);
    let mut trak = bx(b"tkhd", &tkhd);
    trak.extend(bx(b"mdia", &mdia));
    let mut moov = bx(b"mvhd", &mvhd);
    moov.extend(bx(b"trak", &trak));
    let mut file = bx(b"ftyp", b"isom\0\0\0\0isom");
    file.extend(bx(b"mdat", &[0u8; 64]));
    file.extend(bx(b"moov", &moov));
    file
}

#[tokio::test]
async fn hvc1_metadata_reported() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "hevc.mp4", &synthetic_hvc1());
    let state = AppState::new(
        vec![dir.path().to_path_buf()],
        Db::open_in_memory().unwrap(),
    );
    state.rescan_blocking().unwrap();
    let router = build_router(state);
    let lib = get_json(&router, "/deovr").await;
    let item = &lib["scenes"][0]["list"][0];
    assert_eq!(item["videoLength"], 91);
    let id = item["video_url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap();
    let v = get_json(&router, &format!("/deovr/video/{id}")).await;
    assert_eq!(v["videoLength"], 91);
    assert_eq!(v["encodings"][0]["name"], "h265");
    let src = &v["encodings"][0]["videoSources"][0];
    assert_eq!(src["resolution"], 720);
    assert_eq!(src["height"], 720);
    assert_eq!(src["width"], 1280);
}
