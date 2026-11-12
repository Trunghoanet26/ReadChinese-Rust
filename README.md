# ReadChinese – Tauri 2 prototype

Same app as the Electron version in `../src`, rebuilt on **Tauri 2** (Rust + the system WebView2).
OCR still runs in Python (RapidOCR) as a **sidecar** process – phase 1 of the migration.

## Run

```powershell
.\run.ps1          # dev run (debug build)
.\run.ps1 build    # release build + installer
```

Needs Rust (rustup, MSVC toolchain) and Python with pip. No Node.js: the script uses the Rust Tauri CLI
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
| `src-tauri/src/ocr.rs` | Python OCR sidecar + reading-order/line joining |
| `src-tauri/src/translate.rs` | Google (free endpoints or official API) + AI providers with streaming |
| `src-tauri/src/tts.rs` | Edge TTS via the pinned `edge-tts` package, mp3 cache |
| `src-tauri/capabilities/` | Which window may call which command |
| `../python/ocr_server.py` | Shared with the Electron version |

## Security changes compared with the Electron version

- **API keys** are stored in Windows Credential Manager (`ReadChinese` entries), not in `config.json`.
  On first start the Electron settings (`%APPDATA%\ReadChinese\config.json`) are imported and their keys moved.
- **Settings are validated in Rust**: unknown fields are rejected; `pythonPath` may only be `python`, `python3`, `py`
  or an absolute path to an existing `python*.exe`; hotkeys, languages, voices and URLs are checked.
- **Capabilities**: the capture overlay window may only call `capture_ready` / `capture_done`.
- **Strict CSP**: no inline scripts (`script-src 'self'`).
- **No temp files with screen content**: the screenshot stays in memory (served to the overlay through the
  `shot` protocol) and the cropped image is sent to Python over stdin.
- **Pinned Python packages**: `rapidocr_onnxruntime==1.4.4`, `edge-tts==7.3.1`, installed without `-U`.
- **WebView2 is updated by Windows**, so the browser engine no longer goes out of support.
- The Google Cloud API key is sent in a header, not in the URL.
- DevTools exist only in debug builds.
