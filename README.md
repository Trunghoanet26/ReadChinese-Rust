# ReadChinese

**[English](#english)** · **[Tiếng Việt](#tiếng-việt)**

---

## English

Select an area of the screen with Chinese text → text recognition (OCR) → pinyin → translation (Google, AI models) → read aloud.
A Windows app written in **Rust + Tauri 2**; the UI runs on the WebView2 engine built into Windows.
**No Python or other runtime needed**: OCR and text-to-speech run inside the app.

### Install

1. Open the **Releases** page of this repo, download `ReadChinese_x.y.z_x64-setup.exe` and run it.
2. Windows SmartScreen may warn because the installer is not code-signed: click **More info → Run anyway**.
3. The app lives in the system tray (bottom-right of the taskbar).

Requires Windows 10/11 64-bit. WebView2 ships with Windows 11; if it is missing, the installer downloads it.

### Usage

| Hotkey (default) | Action |
|---|---|
| **Alt+Q** | Capture: drag to select the text area (Esc or right-click to cancel) |
| **Alt+E** | Translate the text currently in the clipboard |
| **Alt+W** | Show the window |

- Click 🔊 to read aloud. Choose the **Edge** voices (need Internet) or a Windows voice in ⚙ Settings.
- **Google** translation works out of the box, no key needed.
- AI translation (DeepSeek, OpenAI, Claude, Gemini, …): everyone enters their own API key in ⚙ Settings.
  Keys are stored in Windows Credential Manager, not in the settings file.

### Build from source

Needs Rust (rustup + MSVC toolchain). No Node.js.

```powershell
.\run.ps1          # dev run (debug build)
.\run.ps1 build    # release build + NSIS installer
```

The first run installs the Tauri CLI (`cargo install tauri-cli`). Build output goes to
`%LOCALAPPDATA%\ReadChinese-tauri-target` (kept out of synced folders such as Google Drive); the installer ends up in
`%LOCALAPPDATA%\ReadChinese-tauri-target\release\bundle\nsis\`.

### Layout

| Path | What |
|---|---|
| `ui/` | Front end (HTML/CSS/JS). `api.js` maps `window.api` to Tauri `invoke`/`listen` |
| `ui/style.css` | Colour palette: `#0B1F3A` `#143A51` `#746C6B` `#C49A68` `#FFD66D` |
| `src-tauri/src/main.rs` | Windows, tray, hotkeys, screen capture (`xcap`), commands |
| `src-tauri/src/config.rs` | Settings + validation; API keys in Windows Credential Manager |
| `src-tauri/src/ocr.rs` | OCR: port of the RapidOCR pipeline (text detection, 0°/180° classifier, recognition) on ONNX Runtime (`ort`) + reading order |
| `src-tauri/src/translate.rs` | Google (free endpoints or official API) + AI providers with streaming |
| `src-tauri/src/tts.rs` | Edge TTS over websocket (Sec-MS-GEC token, SSML), mp3 cache |
| `src-tauri/examples/` | `ocr_cli` (OCR image files) and `tts_cli` (synthesize speech) for testing without the UI |
| `src-tauri/capabilities/` | Which window may call which command |
| `models/` | PP-OCRv4 det/rec + mobile cls ONNX models (Apache-2.0, from RapidOCR 1.4.4), bundled with the installer |

```powershell
cd src-tauri
cargo run --release --example ocr_cli -- ..\models 2 image.png
cargo run --release --example tts_cli -- zh-CN-XiaoxiaoNeural +0% out.mp3 "你好"
```

### Security

- API keys are kept in Windows Credential Manager (`ReadChinese` entries), never written to `config.json`.
- Settings are validated in Rust: unknown fields are rejected; hotkeys, languages, voices and URLs are checked.
- The capture overlay window may only call `capture_ready` / `capture_done`.
- Strict CSP: no inline scripts (`script-src 'self'`).
- Screenshots stay in memory; nothing is written to temp files.
- No Python, no package installs at runtime: the models and ONNX Runtime ship inside the installer.
- The Google Cloud key is sent in a header, not in the URL. DevTools exist only in debug builds.

### Third-party licenses

- PP-OCR models (PaddleOCR) — Apache-2.0.
- `ui/vendor/pinyin-pro.js` — MIT (`ui/vendor/pinyin-pro.LICENSE`).
- ONNX Runtime — MIT.

---

## Tiếng Việt

Chụp một vùng màn hình có chữ Trung Quốc → nhận dạng chữ (OCR) → hiện pinyin → dịch (Google, các AI) → đọc to.
Ứng dụng Windows viết bằng **Rust + Tauri 2**, giao diện chạy trên WebView2 có sẵn của Windows.
**Không cần cài Python** hay thư viện nào khác: OCR và giọng đọc đều chạy trong app.

### Cài đặt

1. Vào trang **Releases** của repo, tải `ReadChinese_x.y.z_x64-setup.exe` rồi chạy.
2. Windows SmartScreen có thể cảnh báo vì file cài chưa được ký số: chọn **More info → Run anyway**.
3. App chạy ở khay hệ thống (góc phải thanh taskbar).

Yêu cầu: Windows 10/11 64-bit. WebView2 có sẵn trên Windows 11; nếu máy thiếu, bộ cài sẽ tự tải về.

### Sử dụng

| Phím tắt (mặc định) | Việc |
|---|---|
| **Alt+Q** | Chụp: kéo chuột chọn vùng có chữ (Esc hoặc chuột phải để hủy) |
| **Alt+E** | Dịch chữ đang có trong clipboard |
| **Alt+W** | Hiện cửa sổ |

- Bấm 🔊 để đọc to. Chọn giọng **Edge** (cần Internet) hoặc giọng Windows trong ⚙ Settings.
- Dịch bằng **Google** dùng được ngay, không cần key.
- Dịch bằng AI (DeepSeek, OpenAI, Claude, Gemini, …): mỗi người tự nhập API key của mình trong ⚙ Settings.
  Key được lưu trong Windows Credential Manager, không nằm trong file cấu hình.

### Build từ mã nguồn

Cần Rust (rustup + MSVC toolchain). Không cần Node.js.

```powershell
.\run.ps1          # chạy bản dev (debug)
.\run.ps1 build    # bản release + bộ cài NSIS
```

Lần đầu script tự cài Tauri CLI (`cargo install tauri-cli`). Output build nằm ở
`%LOCALAPPDATA%\ReadChinese-tauri-target` (để không đồng bộ hàng GB lên Google Drive); bộ cài ở
`%LOCALAPPDATA%\ReadChinese-tauri-target\release\bundle\nsis\`.

### Cấu trúc

| Đường dẫn | Nội dung |
|---|---|
| `ui/` | Giao diện (HTML/CSS/JS). `api.js` nối `window.api` với `invoke`/`listen` của Tauri |
| `ui/style.css` | Bảng màu: `#0B1F3A` `#143A51` `#746C6B` `#C49A68` `#FFD66D` |
| `src-tauri/src/main.rs` | Cửa sổ, khay hệ thống, phím tắt, chụp màn hình (`xcap`), các command |
| `src-tauri/src/config.rs` | Cài đặt + kiểm tra hợp lệ; API key trong Windows Credential Manager |
| `src-tauri/src/ocr.rs` | OCR: port pipeline RapidOCR (dò vùng chữ, xoay 0°/180°, nhận dạng) chạy bằng ONNX Runtime (`ort`) + sắp xếp dòng |
| `src-tauri/src/translate.rs` | Google (endpoint miễn phí hoặc API chính thức) + các AI, trả kết quả dạng stream |
| `src-tauri/src/tts.rs` | Edge TTS qua websocket (token Sec-MS-GEC, SSML), cache mp3 |
| `src-tauri/examples/` | `ocr_cli` (OCR file ảnh) và `tts_cli` (đọc thử) để kiểm tra không cần mở app |
| `src-tauri/capabilities/` | Cửa sổ nào được gọi command nào |
| `models/` | Mô hình ONNX PP-OCRv4 det/rec + mobile cls (Apache-2.0, từ RapidOCR 1.4.4), đóng gói kèm bộ cài |

```powershell
cd src-tauri
cargo run --release --example ocr_cli -- ..\models 2 anh.png
cargo run --release --example tts_cli -- zh-CN-XiaoxiaoNeural +0% out.mp3 "你好"
```

### Bảo mật

- API key nằm trong Windows Credential Manager (mục `ReadChinese`), không ghi vào `config.json`.
- Cài đặt được kiểm tra trong Rust: trường lạ bị từ chối; phím tắt, ngôn ngữ, giọng đọc, URL đều được kiểm tra.
- Cửa sổ chụp màn hình chỉ được gọi `capture_ready` / `capture_done`.
- CSP chặt: không có script inline (`script-src 'self'`).
- Ảnh chụp màn hình chỉ ở trong bộ nhớ, không ghi ra file tạm.
- Không Python, không cài gói lúc chạy: mô hình và ONNX Runtime nằm sẵn trong bộ cài.
- Key Google Cloud gửi trong header, không nằm trong URL. DevTools chỉ có ở bản debug.

### Giấy phép bên thứ ba

- Mô hình PP-OCR (PaddleOCR) — Apache-2.0.
- `ui/vendor/pinyin-pro.js` — MIT (`ui/vendor/pinyin-pro.LICENSE`).
- ONNX Runtime — MIT.
