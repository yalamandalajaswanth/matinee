# matinee — MVP Spec

A single Windows executable, written in Rust, that serves the 2D MP4 movies on a PC to **DeoVR on a Meta Quest 3** over the local network. It also includes a web portal for browsing and playing them in any browser.

Pitch: *point it at a folder, open the printed address in DeoVR, and watch. No installs, no Docker, no accounts.*

## Goals / non-goals

**In scope (MVP)**
- MP4 / M4V files only. Folders are scanned recursively (nested folders are supported), and no paths are hardcoded.
- DeoVR library integration through its JSON format: every video is played as a **flat 2D screen**.
- Web portal: thumbnail grid, folder navigation, search, resume position, watched markers, sidecar subtitles.
- Local network only. No authentication.

**Out of scope (later)**
- ffmpeg (generated thumbnails, previews, transcoding), other containers (MKV, AVI), file-system watching, remote access, users and logins, TMDB posters, 3D/VR formats.

## Platform facts that drive the design
- Quest 3 can hardware-decode H.264, HEVC (H.265, including 10-bit), VP9 and AV1. Any other codec is flagged as "may not play".
- DeoVR reads a library from `GET /deovr` (opened from DeoVR's built-in browser). Each entry links to a per-video JSON file. **`video_url`, `thumbnailUrl` and stream URLs must be absolute.**
- DeoVR's JSON has **no subtitles field and no progress callback**. Resume and watched tracking therefore apply only to the web portal, and DeoVR keeps its own local history. This is documented in the README, not worked around.
- Video IDs must be **numeric** (DeoVR `id: number`).

## Tech stack
| Concern | Choice |
|---|---|
| HTTP | `axum` + `tokio`; `tower-http` (`ServeFile` for streaming with Range/206 support, plus CORS and tracing) |
| Scan | `walkdir` |
| MP4 metadata | `mp4` crate: duration, width/height, video codec. Parse failures are non-fatal (fields become `null`) |
| Storage | `rusqlite` with the `bundled` feature (no system SQLite needed) |
| Frontend | Plain HTML/CSS/JS with no build step, embedded in the binary with `rust-embed` |
| CLI/config | `clap` |
| Startup UX | `local-ip-address` to find LAN IPv4 addresses, `qrcode` to print a QR code in the terminal |
| Data dir | `directories`: `%LOCALAPPDATA%\matinee\` holds `library.db` |
| Logging | `tracing` + `tracing-subscriber` |

## CLI
```
matinee --library <DIR> [--library <DIR2> ...] [--port 8080] [--bind 0.0.0.0] [--data-dir <DIR>]
```
- At least one `--library` is required. If none is given, print help and exit with a non-zero code.
- If a library directory doesn't exist, log a warning and skip it. If none are valid, exit with an error.

## Library scanning
- Walk each library root recursively. Don't follow symlinks, and skip hidden or dot-prefixed directories and files.
- Include extensions `.mp4` and `.m4v`, matched case-insensitively.
- Every video is keyed in SQLite by `(library_root, relative_path)`, with an `INTEGER PRIMARY KEY` id. **IDs stay stable across rescans and restarts.** Files that disappear are marked missing (hidden from listings), not deleted, so their progress survives if they come back.
- Per file, store: title (filename stem, with `.`/`_` replaced by spaces), relative folder, size, mtime, duration, width, height, codec, and `quest_playable` (bool or null).
- Metadata is re-read only when the size or mtime changes.
- Scans run at startup and through `POST /api/rescan`. A scan must not block requests: the API keeps serving the previous snapshot until the new one is ready.

## Sidecar files (same folder, same stem)
- Thumbnail: `<stem>.jpg|.jpeg|.png|.webp`. If there isn't one, serve a built-in SVG placeholder.
- Subtitles: `<stem>.vtt` or `<stem>.srt`, optionally with a language tag such as `<stem>.en.srt`. SRT is converted to WebVTT on the fly.

## HTTP endpoints
All absolute URLs are built from the request's `Host` header (scheme `http`), so they match whatever address DeoVR used.

**DeoVR**
- `GET /deovr` → `{ "scenes": [ { "name": "<folder path or 'Library'>", "list": [VideoLink...] } ], "authorized": 0 }`. There is one scene per folder (relative path, e.g. `Movies/Action`; root files go in `Library`), sorted by name. Missing files are excluded.
  - VideoLink: `{ "title", "thumbnailUrl", "videoLength" (seconds, int, omitted if unknown), "video_url": "http://host/deovr/video/{id}" }`
- `GET /deovr/video/{id}` → `{ "id", "title", "is3d": false, "screenType": "flat", "stereoMode": "off", "videoLength", "thumbnailUrl", "encodings": [ { "name": "h264|h265|av1|vp9|unknown", "videoSources": [ { "resolution": <height or 1080>, "height", "width", "url": "http://host/stream/{id}" } ] } ] }`

**Media**
- `GET /stream/{id}` → the file bytes with `Content-Type: video/mp4`, full Range support (`206 Partial Content`, `Accept-Ranges: bytes`), and HEAD support.
- `GET /thumb/{id}` → the sidecar image or the SVG placeholder.
- `GET /subs/{id}` → JSON list of subtitle tracks `[ { "lang", "label", "url" } ]`; `GET /subs/{id}/{index}.vtt` → WebVTT.

**Portal API (JSON)**
- `GET /api/videos?q=&folder=` → list of videos (id, title, folder, duration, width, height, codec, quest_playable, position, watched, has_subs). `q` is a case-insensitive substring search on title and folder. `folder` filters to that folder and everything below it.
- `GET /api/folders` → folder tree with video counts.
- `GET /api/videos/{id}` → a single video.
- `PUT /api/videos/{id}/progress` body `{ "position": f64 }` → saves the position. If `position >= 0.9 * duration`, the video is marked watched and its position reset to 0.
- `PUT /api/videos/{id}/watched` body `{ "watched": bool }`.
- `POST /api/rescan` → `202`, scanning in the background. `GET /api/status` → `{ "scanning", "video_count", "last_scan" }`.

**Portal UI**
- `GET /` and static assets, served from the embedded frontend.

## Web portal UX
- Header: app name, search box, rescan button, and the LAN address to type into DeoVR.
- Left: a collapsible folder tree. Main area: a responsive thumbnail grid. Each card shows the title, duration, a progress bar, a watched check, and a "may not play on Quest" badge when `quest_playable == false`.
- Clicking a card opens an overlay player (it expands from the card) with `<video controls>`, starting at the saved position. Subtitle tracks are added as `<track>` elements. Progress is saved every 10 s and on pause or close. There is a "Mark watched/unwatched" toggle. Esc closes the overlay.
- It must be usable in the Meta Quest Browser too: large hit targets and no hover-only controls.

## Security (local-only by design)
- File access only ever goes through numeric IDs looked up in the DB. **Raw paths are never taken from requests.**
- Middleware rejects any request whose peer address is not loopback, RFC1918 private, link-local, or IPv6 ULA (403).
- No auth. The README states this clearly.

## Startup output
```
matinee v0.1.0 — 42 videos in 2 libraries
Open in DeoVR (browser) or any browser on your network:
  http://192.168.1.50:8080
[QR code]
```
If the port is busy, exit with a clear message. The README documents the Windows Firewall rule (`netsh advfirewall firewall add rule ...`). The app never changes firewall settings itself.

## Testing
- Unit tests: scan filtering (extensions, hidden dirs, nesting), ID stability across rescans, missing-file handling, title derivation, SRT→VTT conversion, codec → `quest_playable` mapping, DeoVR JSON shape (snapshot-style asserts on field names and values).
- Integration tests (spin up the router against a temp dir): `/deovr` absolute URLs use the Host header, a Range request on `/stream/{id}` returns 206 with the correct `Content-Range`, unknown IDs return 404, progress round-trips, and a non-private peer gets 403.
- Test fixtures are generated in temp dirs. Fake `.mp4` files with invalid contents must still be listed (metadata `null`).

## Milestones
1. **Core server:** CLI, scanning, SQLite, DeoVR endpoints, streaming, LAN-only middleware, and tests.
2. **Portal:** API endpoints, embedded frontend, progress/watched, thumbnails and subtitles, and tests.
3. **Polish:** startup banner with QR code, README (setup, firewall, DeoVR steps, limitations), and a release build.
