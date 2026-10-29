use std::net::SocketAddr;
use std::path::Path;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use matinee::app::build_router;
use matinee::state::AppState;
use tower::ServiceExt;

const LAN: &str = "192.168.1.20:5555";

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn new_state(root: &Path, db_dir: &Path) -> AppState {
    AppState::open(vec![root.to_path_buf()], &db_dir.join("library.db")).unwrap()
}

/// Returns (tempdir, state, library root, movie bytes). The database lives beside the library.
fn setup_state() -> (tempfile::TempDir, AppState, std::path::PathBuf, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    write(&lib, "My_Movie.mp4", &data);
    write(&lib, "Action/Fight.2020.MP4", b"junk bytes");
    write(&lib, "Action/Fight.2020.png", b"png");
    let state = new_state(&lib, dir.path());
    state.rescan_blocking().unwrap();
    (dir, state, lib, data)
}

fn setup() -> (tempfile::TempDir, Router, Vec<u8>) {
    let (dir, state, _lib, data) = setup_state();
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
    let state = new_state(dir.path(), dir.path());
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

// ---------------------------------------------------------------- milestone 2

async fn send_body(
    router: &Router,
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut b = req(method, uri);
    if let Some(ct) = content_type {
        b = b.header(header::CONTENT_TYPE, ct);
    }
    let mut r = b.body(Body::from(body.to_string())).unwrap();
    r.extensions_mut()
        .insert(ConnectInfo(LAN.parse::<SocketAddr>().unwrap()));
    let resp = router.clone().oneshot(r).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (parts.status, parts.headers, bytes)
}

async fn put_json(router: &Router, uri: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let (s, _, bytes) = send_body(router, Method::PUT, uri, Some("application/json"), body).await;
    (s, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// A router over a single synthetic 90.5 s HEVC file; returns (dir, state, router, id).
async fn hevc_setup() -> (tempfile::TempDir, AppState, Router, i64) {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    write(&lib, "hevc.mp4", &synthetic_hvc1());
    let state = new_state(&lib, dir.path());
    state.rescan_blocking().unwrap();
    let router = build_router(state.clone());
    let list = get_json(&router, "/api/videos").await;
    let id = list[0]["id"].as_i64().unwrap();
    (dir, state, router, id)
}

#[tokio::test]
async fn videos_folders_and_single() {
    let (_d, router, _) = setup();
    let list = get_json(&router, "/api/videos").await;
    let titles: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["title"].as_str().unwrap())
        .collect();
    // Root folder sorts before "Action".
    assert_eq!(titles, ["My Movie", "Fight 2020"]);
    let first = &list[0];
    for key in [
        "id",
        "title",
        "folder",
        "duration",
        "width",
        "height",
        "codec",
        "quest_playable",
        "size",
        "position",
        "watched",
        "has_subs",
    ] {
        assert!(first.get(key).is_some(), "missing {key}");
    }
    assert_eq!(first["size"], 1000);
    assert_eq!(first["position"], 0.0);
    assert_eq!(first["watched"], false);
    assert_eq!(first["has_subs"], false);

    let in_action = get_json(&router, "/api/videos?folder=Action").await;
    assert_eq!(in_action[0]["title"], "Fight 2020");
    let partial = get_json(&router, "/api/videos?folder=Acti").await;
    assert_eq!(partial, serde_json::json!([]));
    for (q, n) in [("q=FIGHT", 1), ("q=action", 1), ("q=zzz", 0)] {
        let r = get_json(&router, &format!("/api/videos?{q}")).await;
        assert_eq!(r.as_array().unwrap().len(), n, "{q}");
    }

    let tree = get_json(&router, "/api/folders").await;
    assert_eq!(tree["path"], "");
    assert_eq!(tree["count"], 2);
    assert_eq!(tree["children"][0]["name"], "Action");
    assert_eq!(tree["children"][0]["path"], "Action");
    assert_eq!(tree["children"][0]["count"], 1);

    let id = first["id"].as_i64().unwrap();
    let one = get_json(&router, &format!("/api/videos/{id}")).await;
    assert_eq!(&one, first);
    for uri in ["/api/videos/99999", "/api/videos/abc"] {
        let (s, _, _) = send(&router, req(Method::GET, uri), LAN).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn progress_roundtrip_validation_and_auto_watched() {
    let (_d, _state, router, id) = hevc_setup().await;
    let uri = format!("/api/videos/{id}/progress");
    let single = format!("/api/videos/{id}");

    let (s, v) = put_json(&router, &uri, r#"{"position": 42.5}"#).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["position"], 42.5);
    assert_eq!(v["watched"], false);
    assert_eq!(get_json(&router, &single).await["position"], 42.5);
    assert_eq!(get_json(&router, "/api/videos").await[0]["position"], 42.5);

    // Bad bodies are 400: negative, non-finite or non-numeric.
    for body in [
        r#"{"position": -1}"#,
        r#"{"position": NaN}"#,
        r#"{"position": 1e999}"#,
        r#"{"position": null}"#,
        r#"{"position": "5"}"#,
        r#"{}"#,
        "not json",
    ] {
        let (s, _) = put_json(&router, &uri, body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(get_json(&router, &single).await["position"], 42.5);

    // Just under 90% of 90.5 s stays unwatched; at 90% it flips and resets.
    let (_, v) = put_json(&router, &uri, r#"{"position": 81.0}"#).await;
    assert_eq!(v["watched"], false);
    assert_eq!(v["position"], 81.0);
    let (s, v) = put_json(&router, &uri, r#"{"position": 82.0}"#).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["watched"], true);
    assert_eq!(v["position"], 0.0);

    // Wrong content type and unknown ids.
    let body = r#"{"position":1}"#;
    let (s, _, _) = send_body(&router, Method::PUT, &uri, Some("text/plain"), body).await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (s, _, _) = send_body(&router, Method::PUT, &uri, None, body).await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (s, _) = put_json(&router, "/api/videos/99999/progress", body).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn watched_put_sets_flag_and_resets_position() {
    let (_d, router, _) = setup();
    let id = get_json(&router, "/api/videos").await[0]["id"]
        .as_i64()
        .unwrap();
    // Duration is unknown for the fake file, so the position is stored as sent.
    let progress = format!("/api/videos/{id}/progress");
    let (_, v) = put_json(&router, &progress, r#"{"position": 30}"#).await;
    assert_eq!(v["position"], 30.0);
    let uri = format!("/api/videos/{id}/watched");
    let (s, v) = put_json(&router, &uri, r#"{"watched": true}"#).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["watched"], true);
    assert_eq!(v["position"], 0.0);
    let (_, v) = put_json(&router, &uri, r#"{"watched": false}"#).await;
    assert_eq!(v["watched"], false);
    let (s, _) = put_json(&router, &uri, r#"{"watched": "yes"}"#).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = put_json(&router, "/api/videos/99999/watched", r#"{"watched": true}"#).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rescan_is_async_and_requires_json() {
    let (_d, state, lib, _) = setup_state();
    let router = build_router(state);
    let status = get_json(&router, "/api/status").await;
    assert_eq!(status["scanning"], false);
    assert_eq!(status["video_count"], 2);
    assert!(status["last_scan"].is_number());
    assert!(status["last_error"].is_null());

    // Without a JSON content type: 415 and no scan.
    let post = |ct: Option<&'static str>| send_body(&router, Method::POST, "/api/rescan", ct, "{}");
    assert_eq!(post(None).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        post(Some("text/plain")).await.0,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    // No CORS headers are ever sent.
    let b = req(Method::OPTIONS, "/api/rescan")
        .header(header::ORIGIN, "http://evil.example")
        .header("access-control-request-method", "POST");
    let (_, h, _) = send(&router, b, LAN).await;
    assert!(h.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());

    write(&lib, "New/Added.mp4", b"x");
    assert_eq!(post(Some("application/json")).await.0, StatusCode::ACCEPTED);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let st = get_json(&router, "/api/status").await;
        if st["video_count"] == 3 && st["scanning"] == false {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "rescan never finished: {st}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let added = get_json(&router, "/api/videos?folder=New").await;
    assert_eq!(added[0]["title"], "Added");
}

#[tokio::test]
async fn only_one_scan_at_a_time() {
    let (_d, state, _lib, _) = setup_state();
    // Hold the scan connection so a started scan stays "running".
    let guard = state.lock_scan_db_for_test();
    assert!(state.trigger_rescan());
    assert!(state.status().scanning);
    assert!(!state.trigger_rescan(), "second scan must not start");
    drop(guard);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while state.status().scanning {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The flag was reset, so another scan can start.
    assert!(state.trigger_rescan());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn progress_write_succeeds_while_scan_connection_is_held() {
    let (_d, state, _lib, _) = setup_state();
    let router = build_router(state.clone());
    let id = get_json(&router, "/api/videos").await[0]["id"]
        .as_i64()
        .unwrap();
    let _scan = state.lock_scan_db_for_test();
    let work = async {
        let uri = format!("/api/videos/{id}/progress");
        let (s, v) = put_json(&router, &uri, r#"{"position": 7}"#).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["position"], 7.0);
        assert_eq!(get_json(&router, "/api/videos").await[0]["position"], 7.0);
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .expect("progress path blocked by the scan lock");
}

#[tokio::test]
async fn subtitles_list_and_conversion() {
    let (_d, state, lib, _) = setup_state();
    write(
        &lib,
        "Action/Fight.2020.srt",
        b"\xEF\xBB\xBF1\r\n00:00:01,500 --> 00:00:02,000\r\nHi, caf\xE9\r\n",
    );
    write(
        &lib,
        "Action/Fight.2020.EN.vtt",
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n",
    );
    write(&lib, "Action/Fight.2020.toolongtag.srt", b"x");
    state.rescan_blocking().unwrap();
    let router = build_router(state);
    let list = get_json(&router, "/api/videos?folder=Action").await;
    assert_eq!(list[0]["has_subs"], true);
    let id = list[0]["id"].as_i64().unwrap();
    let movie = get_json(&router, "/api/videos?q=my").await;
    assert_eq!(movie[0]["has_subs"], false);

    let subs = get_json(&router, &format!("/api/videos/{id}/subs")).await;
    let arr = subs.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Sorted by file name, case-insensitive: "Fight.2020.EN.vtt" before "Fight.2020.srt".
    assert_eq!(arr[0]["lang"], "en");
    assert_eq!(arr[0]["index"], 0);
    assert_eq!(arr[0]["url"], format!("/subs/{id}/0.vtt"));
    assert!(arr[1]["lang"].is_null());
    assert_eq!(arr[1]["label"], "Subtitles");
    assert_eq!(get_json(&router, &format!("/subs/{id}")).await, subs);

    let (s, h, body) = send(&router, req(Method::GET, &format!("/subs/{id}/1.vtt")), LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "text/vtt; charset=utf-8");
    assert_eq!(
        String::from_utf8(body).unwrap(),
        "WEBVTT\n\n1\n00:00:01.500 --> 00:00:02.000\nHi, caf\u{e9}\n"
    );
    let (s, _, body) = send(&router, req(Method::GET, &format!("/subs/{id}/0.vtt")), LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8(body).unwrap().starts_with("WEBVTT"));

    for uri in [
        format!("/subs/{id}/2.vtt"),
        format!("/subs/{id}/x.vtt"),
        format!("/subs/{id}/0.srt"),
        "/subs/99999/0.vtt".to_string(),
        "/api/videos/99999/subs".to_string(),
    ] {
        let (s, _, _) = send(&router, req(Method::GET, &uri), LAN).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn oversized_subtitle_is_413() {
    let (_d, state, lib, _) = setup_state();
    write(&lib, "My_Movie.srt", &vec![b'a'; 5 * 1024 * 1024 + 1]);
    let router = build_router(state);
    let id = get_json(&router, "/api/videos?q=my").await[0]["id"]
        .as_i64()
        .unwrap();
    let (s, _, _) = send(&router, req(Method::GET, &format!("/subs/{id}/0.vtt")), LAN).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn frontend_is_served() {
    let (_d, router, _) = setup();
    let (s, h, body) = send(&router, req(Method::GET, "/"), LAN).await;
    assert_eq!(s, StatusCode::OK);
    let ct = |h: &axum::http::HeaderMap| h[header::CONTENT_TYPE].to_str().unwrap().to_string();
    assert!(ct(&h).starts_with("text/html"));
    assert!(String::from_utf8(body).unwrap().contains("matinee"));
    let (s, h, _) = send(&router, req(Method::GET, "/assets/app.js"), LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert!(ct(&h).starts_with("text/javascript"));
    let (s, h, _) = send(&router, req(Method::GET, "/assets/app.css"), LAN).await;
    assert_eq!(s, StatusCode::OK);
    assert!(ct(&h).starts_with("text/css"));
    let (s, _, _) = send(&router, req(Method::GET, "/assets/missing.js"), LAN).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = send(&router, req(Method::GET, "/"), "8.8.8.8:1").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
