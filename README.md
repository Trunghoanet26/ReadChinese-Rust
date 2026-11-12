# ReadChinese – Tauri 2 prototype

Same app as the Electron version in `../src`, rebuilt on **Tauri 2** (Rust + the system WebView2).
OCR runs **in Rust**: the PP-OCRv4 ONNX models (`models/`) with ONNX Runtime via `ort` (statically linked).
Edge TTS also runs in Rust (websocket client ported from `edge-tts` 7.3.1). **No Python needed.**

## Run

```powershell
.\run.ps1          # dev run (debug build)
.\run.ps1 build    # release build + installer
```

Needs Rust (rustup, MSVC toolchain). No Node.js: the script uses the Rust Tauri CLI
(`cargo install tauri-cli`, installed automatically the first time) because an npm `node_modules` on
Google Drive got corrupted (0-byte files).
Build output goes to `%LOCALAPPDATA%\ReadChinese-tauri-target`, not into Google Drive; the installer ends up in
`%LOCALAPPDATA%\ReadChinese-tauri-target\release\bundle\nsis\`.

## Layout

| Path | What |
|---|---|
| `ui/` | Front end. `renderer.js` is the Electron one almost unchanged; `api.js` maps `window.api` to Tauri `invoke`/`listen`. |
| `ui/style.css` | Colour palette: `#0B1F3A` `#143A51` `#746C6B` `#C49A68` `#FFD66D` |
| `src-tauri/src/main.rs` | Windows, tray, hotkeys, screen capture (`xcap`), commands |
| `src-tauri/src/config.rs` | Settings + validation; API keys in Windows Credential Manager |
| `src-tauri/src/ocr.rs` | OCR: port of the RapidOCR pipeline (detection, 0°/180° classifier, recognition) + reading order |
| `src-tauri/examples/ocr_cli.rs` | Dev tool: run the OCR on image files (`cargo run --release --example ocr_cli -- ../models 2 a.png`) |
| `models/` | PP-OCRv4 det/rec + mobile cls ONNX models (Apache-2.0, from RapidOCR 1.4.4), bundled with the installer |
| `src-tauri/src/translate.rs` | Google (free endpoints or official API) + AI providers with streaming |
| `src-tauri/src/tts.rs` | Edge TTS over websocket (Sec-MS-GEC token, SSML), mp3 cache; `examples/tts_cli.rs` to try it |
| `src-tauri/capabilities/` | Which window may call which command |

## Security changes compared with the Electron version

- **API keys** are stored in Windows Credential Manager (`ReadChinese` entries), not in `config.json`.
  On first start the Electron settings (`%APPDATA%\ReadChinese\config.json`) are imported and their keys moved.
- **Settings are validated in Rust**: unknown fields are rejected; hotkeys, languages, voices and URLs are checked.
- **Capabilities**: the capture overlay window may only call `capture_ready` / `capture_done`.
- **Strict CSP**: no inline scripts (`script-src 'self'`).
- **No temp files with screen content**: the screenshot stays in memory (served to the overlay through the
  `shot` protocol) and the crop is recognized in-process.
- **No Python, no runtime package installs**: models and ONNX Runtime ship inside the installer; nothing is downloaded or executed besides the app itself.
- **WebView2 is updated by Windows**, so the browser engine no longer goes out of support.
- The Google Cloud API key is sent in a header, not in the URL.
- DevTools exist only in debug builds.
