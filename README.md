# matinee

matinee is a small Windows program that serves the movies on your PC to DeoVR on a Meta Quest over your home network. Point it at a folder, open the address it prints in DeoVR's built-in browser, and your library appears grouped by folder, ready to play. It streams the original files with seeking, so there is no conversion step and no upload.

matinee is not affiliated with DeoVR or Meta.

## Features

- Scans one or more folders (including subfolders) for MP4 and M4V files.
- Serves a DeoVR-compatible library, so videos are listed natively inside DeoVR.
- A web portal for any browser: browse, search, resume, mark watched.
- Byte-range streaming, so seeking is instant.
- Sidecar thumbnails and subtitles.
- Startup banner with the LAN address and a QR code.
- LAN-only: requests from outside your local network are rejected.
- Single executable; the web portal is embedded.

## Quick start

1. Get `matinee.exe` (build it from source, see below).
2. Run it, pointing at your movie folder:

   ```
   matinee.exe --library "D:\Movies"
   ```

3. It prints one or more addresses, such as `http://192.168.1.50:8080`, and a QR code.
4. On the Quest, open DeoVR, use its built-in browser, and go to that address. Your library appears.

The same address also works in the Quest Browser or any browser on your network. Your PC and Quest must be on the same network. Press Ctrl+C to stop.

Double-clicking `matinee.exe` without arguments shows the usage help and waits for Enter.

## Windows Firewall

The first time you run matinee, Windows Defender Firewall asks whether to allow it. Allow it on Private networks only. If you missed the prompt, or the Quest cannot connect, add a rule from an administrator terminal (change the port if you use `--port`):

```
netsh advfirewall firewall add rule name="matinee" dir=in action=allow protocol=TCP localport=8080 profile=private
```

To remove it later:

```
netsh advfirewall firewall delete rule name="matinee"
```

matinee never changes firewall settings itself.

## Network profile

Windows applies the Private firewall profile only if your network is set to Private. In Settings, go to Network & internet, open your Wi-Fi or Ethernet connection, and set Network profile type to Private. If it is Public, the Quest will not be able to connect even with the rule above.

## Subtitles and thumbnails

Put sidecar files next to the video, named after it:

- Thumbnail: `Movie.jpg` (also `.jpeg`, `.png`, `.webp`) for `Movie.mp4`. Without one, a placeholder is shown.
- Subtitles: `Movie.srt` or `Movie.vtt`, optionally with a language code such as `Movie.en.srt`. SRT is converted to WebVTT automatically.

## Supported formats

- Containers: MP4 and M4V.
- Quest 3 plays H.264, HEVC (H.265), AV1 and VP9. Videos with other codecs are still listed but may not play.
- MKV, AVI and other containers are not supported yet.

## Command-line options

| Option | Description | Default |
| --- | --- | --- |
| `--library <DIR>` | Folder to scan. Repeat for several folders. At least one is required. | none |
| `--port <PORT>` | TCP port to listen on. | `8080` |
| `--bind <ADDR>` | Address to bind to. A specific address is the only one shown in the banner. | `0.0.0.0` |
| `--data-dir <DIR>` | Directory for the database. | `%LOCALAPPDATA%\matinee` |
| `--no-qr` | Do not print the QR code. | off |

## Where data is stored

Watch progress and the library index live in `%LOCALAPPDATA%\matinee\library.db`. It is safe to delete; the library is rebuilt on the next start, but watch progress is reset. Your video files are never modified.

## Security

matinee has no login. It only accepts connections from loopback, private (RFC1918) and link-local addresses, and answers everything else with 403. Do not port-forward it to the internet or run it on an untrusted network. Anyone on your LAN can browse and stream the library.

## Limitations

- DeoVR's JSON format has no subtitle or progress fields, so resume, watched state and subtitles are available only in the web portal, not in DeoVR's native library view.
- No transcoding: the file must be playable by the Quest as is.

## Building from source

You need stable Rust and the MSVC Build Tools (Visual Studio Build Tools with the C++ workload).

```
cargo build --release
cargo test
```

The executable is `target\release\matinee.exe`.

## License

MIT. See [LICENSE](LICENSE).
