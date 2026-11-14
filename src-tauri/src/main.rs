// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod ocr;
mod translate;
mod tts;

use config::{Config, ConfigPatch, Provider, Store};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use xcap::image::RgbaImage;

/// URL of the in-memory screenshot served by the `shot` protocol (Windows/WebView2 form).
const SHOT_URL: &str = "http://shot.localhost/frame";
const APP_ORIGIN: &str = "http://tauri.localhost";

pub struct AppState {
    pub config: Mutex<Config>,
    store: Store,
    ocr: ocr::Ocr,
    pub translator: translate::Translator,
    tts: tts::Tts,
    capturing: AtomicBool,
    shot: Mutex<Option<Arc<RgbaImage>>>,
    shot_seq: AtomicU64,
    last_shown: Mutex<Instant>,
    hotkey_error: Mutex<Option<String>>,
}

fn cfg(app: &AppHandle) -> Config {
    app.state::<AppState>().config.lock().unwrap().clone()
}

fn status(app: &AppHandle, msg: &str) {
    let _ = app.emit_to("main", "ocr:status", msg);
}

// ------------------------------------------------------------------ windows
fn create_main_window(app: &AppHandle) -> tauri::Result<()> {
    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("ReadChinese from trunghoa.net")
        .inner_size(440.0, 240.0)
        .min_inner_size(300.0, 120.0)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .background_color(tauri::window::Color(11, 31, 58, 255))
        .on_page_load(|w, p| {
            if p.event() == PageLoadEvent::Finished {
                // load the OCR models early, once the UI can show errors
                let app = w.app_handle().clone();
                if let Some(m) = app.state::<AppState>().hotkey_error.lock().unwrap().clone() {
                    status(&app, &m);
                }
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = ensure_ocr(&app).await {
                        let _ = app.emit_to("main", "ocr:error", e);
                    }
                });
            }
        })
        .build()?;
    let app2 = app.clone();
    let w2 = win.clone();
    win.on_window_event(move |ev| match ev {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = w2.hide();
        }
        // not pinned -> hide when the user clicks somewhere else
        WindowEvent::Focused(false) => {
            let st = app2.state::<AppState>();
            let pinned = st.config.lock().unwrap().pinned;
            let recent = st.last_shown.lock().unwrap().elapsed() < Duration::from_millis(400);
            if !pinned && !recent && !st.capturing.load(Ordering::SeqCst) {
                let _ = w2.hide();
            }
        }
        _ => {}
    });
    Ok(())
}

/// Turn off the DWM show/hide animation of a window. Without this Windows keeps the hidden overlay on
/// screen for ~350 ms and then fades it out, which looks like a flicker after the selection.
#[cfg(windows)]
fn disable_dwm_transitions(w: &tauri::WebviewWindow) {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: *mut core::ffi::c_void, attr: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }
    const DWMWA_TRANSITIONS_FORCEDISABLED: u32 = 3;
    if let Ok(hwnd) = w.hwnd() {
        let on: i32 = 1; // BOOL TRUE
        unsafe {
            DwmSetWindowAttribute(hwnd.0 as _, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const i32 as _, 4);
        }
    }
}
#[cfg(not(windows))]
fn disable_dwm_transitions(_w: &tauri::WebviewWindow) {}

fn create_capture_window(app: &AppHandle) -> tauri::Result<()> {
    let w = WebviewWindowBuilder::new(app, "capture", WebviewUrl::App("capture.html".into()))
        .title("ReadChinese capture")
        .inner_size(800.0, 600.0)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .focused(false)
        .visible(false)
        .background_color(tauri::window::Color(0, 0, 0, 255))
        .build()?;
    disable_dwm_transitions(&w);
    Ok(())
}

fn show_main(app: &AppHandle) {
    *app.state::<AppState>().last_shown.lock().unwrap() = Instant::now();
    let Some(w) = app.get_webview_window("main") else { return };
    if !w.is_visible().unwrap_or(false) {
        if let (Ok(c), Ok(size)) = (app.cursor_position(), w.outer_size()) {
            if let Ok(Some(m)) = app.monitor_from_point(c.x, c.y) {
                let wa = m.work_area();
                let (ww, wh) = (size.width as i32, size.height as i32);
                let x = (c.x as i32 + 10).min(wa.position.x + wa.size.width as i32 - ww).max(wa.position.x);
                let y = (c.y as i32 + 10).min(wa.position.y + wa.size.height as i32 - wh).max(wa.position.y);
                let _ = w.set_position(PhysicalPosition::new(x, y));
            }
        }
    }
    let _ = w.show();
    let _ = w.set_focus();
}

// ------------------------------------------------------------------ OCR
/// Folder with the PP-OCR ONNX models (bundled resource; the repository folder in dev runs).
fn models_dir(app: &AppHandle) -> PathBuf {
    let bundled = app.path().resolve("models", tauri::path::BaseDirectory::Resource).ok();
    match bundled {
        Some(p) if p.join(ocr::DET_MODEL).exists() => p,
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("models"),
    }
}

/// Load the OCR models once (on a blocking thread: ~0.3 s).
async fn ensure_ocr(app: &AppHandle) -> Result<(), String> {
    if app.state::<AppState>().ocr.is_loaded() {
        return Ok(());
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let dir = models_dir(&app);
        let threads = cfg(&app).ocr_threads;
        app.state::<AppState>().ocr.ensure(&dir, threads)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ------------------------------------------------------------------ screen capture
async fn do_capture(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.capturing.swap(true, Ordering::SeqCst) {
        return;
    }
    let t0 = Instant::now();
    let res: Result<(), String> = async {
        // hide the (unpinned) translator window so it is not in the screenshot
        if let Some(m) = app.get_webview_window("main") {
            if m.is_visible().unwrap_or(false) && !st.config.lock().unwrap().pinned {
                let _ = m.hide();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let c = app.cursor_position().map_err(|e| e.to_string())?;
        let mon = app.monitor_from_point(c.x, c.y).ok().flatten()
            .or_else(|| app.primary_monitor().ok().flatten())
            .ok_or("no monitor found")?;
        let (cx, cy) = (c.x as i32, c.y as i32);
        // capture on a worker thread (GDI / DXGI through xcap)
        let img = tauri::async_runtime::spawn_blocking(move || -> Result<RgbaImage, String> {
            let m = xcap::Monitor::from_point(cx, cy).map_err(|e| e.to_string())?;
            m.capture_image().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        let (w, h) = (img.width(), img.height());
        *st.shot.lock().unwrap() = Some(Arc::new(img));
        let win = app.get_webview_window("capture").ok_or("capture window missing")?;
        let _ = win.set_position(*mon.position());
        let _ = win.set_size(*mon.size());
        let n = st.shot_seq.fetch_add(1, Ordering::SeqCst);
        // the overlay fetches the raw pixels, paints them and then calls capture_ready -> no black flash
        let _ = app.emit_to("capture", "capture:image", json!({ "url": format!("{SHOT_URL}?n={n}"), "width": w, "height": h }));
        println!("[capture] xcap {w}x{h}: {} ms", t0.elapsed().as_millis());
        Ok(())
    }
    .await;
    if let Err(e) = res {
        st.capturing.store(false, Ordering::SeqCst);
        eprintln!("[capture] {e}");
        show_main(&app);
        let _ = app.emit_to("main", "ocr:error", format!("Screen capture failed: {e}"));
    }
}

fn end_capture(app: &AppHandle) {
    app.state::<AppState>().capturing.store(false, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("capture") {
        let _ = w.hide();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = app.emit_to("capture", "capture:reset", ());
        });
    }
}

#[derive(Deserialize, Clone, Copy)]
struct SelRect { x: f64, y: f64, width: f64, height: f64 }

/// Crop the selection (physical pixels) out of the screenshot.
fn crop(img: &RgbaImage, r: SelRect) -> RgbaImage {
    let (iw, ih) = (img.width(), img.height());
    let x = (r.x.max(0.0) as u32).min(iw.saturating_sub(1));
    let y = (r.y.max(0.0) as u32).min(ih.saturating_sub(1));
    let w = (r.width.round() as u32).clamp(1, iw - x);
    let h = (r.height.round() as u32).clamp(1, ih - y);
    xcap::image::imageops::crop_imm(img, x, y, w, h).to_image()
}

// ------------------------------------------------------------------ hotkeys & tray
fn clipboard_translate(app: &AppHandle) {
    let text = app.clipboard().read_text().unwrap_or_default().trim().to_string();
    show_main(app);
    if !text.is_empty() {
        let _ = app.emit_to("main", "input:set", &text);
        translate::translate_all(app, text);
    }
}

fn register_hotkeys(app: &AppHandle) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let c = cfg(app);
    let mut bad = Vec::new();
    for (key, action) in [(c.hotkey_ocr, 0u8), (c.hotkey_show, 1), (c.hotkey_clipboard, 2)] {
        if key.is_empty() {
            continue;
        }
        let res = key.parse::<Shortcut>().map_err(|e| e.to_string()).and_then(|sc| {
            gs.on_shortcut(sc, move |app, _sc, ev| {
                if ev.state != ShortcutState::Pressed {
                    return;
                }
                match action {
                    0 => { tauri::async_runtime::spawn(do_capture(app.clone())); }
                    1 => show_main(app),
                    _ => clipboard_translate(app),
                }
            })
            .map_err(|e| e.to_string())
        });
        if let Err(e) = res {
            eprintln!("[hotkey] {key}: {e}");
            bad.push(key);
        }
    }
    let msg = (!bad.is_empty()).then(|| format!("Could not register hotkey: {} (used by another app?)", bad.join(", ")));
    if let Some(m) = &msg {
        status(app, m);
    }
    // kept so it can be shown again once the page has loaded (the first emit may come too early)
    *app.state::<AppState>().hotkey_error.lock().unwrap() = msg;
}

fn tray_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let c = cfg(app);
    Menu::with_items(app, &[
        &MenuItem::with_id(app, "capture", format!("Capture & translate ({})", c.hotkey_ocr), true, None::<&str>)?,
        &MenuItem::with_id(app, "show", format!("Open window ({})", c.hotkey_show), true, None::<&str>)?,
        &PredefinedMenuItem::separator(app)?,
        &MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?,
    ])
}

fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let mut b = TrayIconBuilder::with_id("main")
        .tooltip("ReadChinese from trunghoa.net")
        .menu(&tray_menu(app)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "capture" => { tauri::async_runtime::spawn(do_capture(app.clone())); }
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}

// ------------------------------------------------------------------ commands (called from the UI)
#[tauri::command]
fn get_config(app: AppHandle) -> Config { cfg(&app) }

#[tauri::command]
async fn set_config(app: AppHandle, patch: Value) -> Result<Config, String> {
    let patch: ConfigPatch = serde_json::from_value(patch).map_err(|e| format!("Invalid settings: {e}"))?;
    let st = app.state::<AppState>();
    let (old, new) = {
        let mut c = st.config.lock().unwrap();
        let new = c.apply(patch)?;
        (std::mem::replace(&mut *c, new.clone()), new)
    };
    let saved = st.store.save(&new, Some(&old));
    if (old.hotkey_ocr, old.hotkey_clipboard, old.hotkey_show) != (new.hotkey_ocr.clone(), new.hotkey_clipboard.clone(), new.hotkey_show.clone()) {
        register_hotkeys(&app);
        if let (Some(tray), Ok(menu)) = (app.tray_by_id("main"), tray_menu(&app)) {
            let _ = tray.set_menu(Some(menu));
        }
    }
    if old.ocr_threads != new.ocr_threads {
        st.ocr.restart();
    }
    saved?; // settings are applied; only the key could not be stored -> tell the user
    Ok(new)
}

#[tauri::command]
fn get_langs() -> Value { config::langs_json() }

#[tauri::command]
fn ai_presets() -> Value { config::ai_presets() }

#[tauri::command]
fn ai_new_id() -> String { config::new_id() }

#[tauri::command]
async fn ai_test(app: AppHandle, provider: Provider) -> Value {
    app.state::<AppState>().translator.ai_test(&provider).await
}

#[tauri::command]
fn capture_backend() -> &'static str { "native (xcap, Rust)" }

#[tauri::command]
fn translate(app: AppHandle, text: String) {
    let t: String = text.trim().chars().take(20_000).collect();
    if !t.is_empty() {
        translate::translate_all(&app, t);
    }
}

#[tauri::command]
fn start_capture(app: AppHandle) {
    tauri::async_runtime::spawn(do_capture(app));
}

#[tauri::command]
fn hide(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") { let _ = w.hide(); }
}

#[tauri::command]
fn pin(app: AppHandle, v: bool) {
    let st = app.state::<AppState>();
    let c = { let mut c = st.config.lock().unwrap(); c.pinned = v; c.clone() };
    let _ = st.store.save(&c, None); // only the pin state changed; keys are unchanged
}

/// Auto-fit: the UI reports its content height (CSS px); keep the window inside the work area.
#[tauri::command]
fn fit(app: AppHandle, h: f64) {
    let Some(w) = app.get_webview_window("main") else { return };
    // inner_size, not outer_size: set_size() sets the inner size, and on a frameless window the outer size
    // includes the invisible resize border -> using outer_size grew the width on every call (feedback loop).
    let (Ok(sf), Ok(pos), Ok(size), Ok(Some(m))) = (w.scale_factor(), w.outer_position(), w.inner_size(), w.current_monitor()) else { return };
    let wa = m.work_area();
    let max_h = (wa.size.height as f64 * 0.9) as i32;
    let new_h = ((h * sf).round() as i32).clamp((120.0 * sf) as i32, max_h.max(1));
    if (new_h - size.height as i32).abs() < 2 {
        return;
    }
    let y = pos.y.min(wa.position.y + wa.size.height as i32 - new_h).max(wa.position.y);
    let _ = w.set_size(PhysicalSize::new(size.width, new_h as u32));
    let _ = w.set_position(PhysicalPosition::new(pos.x, y));
}

#[tauri::command]
fn copy(app: AppHandle, text: String) {
    let _ = app.clipboard().write_text(text);
}

#[tauri::command]
async fn tts_edge(app: AppHandle, text: String, voice: Option<String>, rate: Option<f64>) -> Value {
    let c = cfg(&app);
    if text.chars().count() > 5000 {
        return json!({ "ok": false, "error": "Text too long for Edge TTS (max 5000 characters)." });
    }
    let voice = voice.filter(|v| config::valid_edge_voice(v)).unwrap_or(c.tts_edge_voice.clone());
    let cache = match app.path().app_cache_dir() {
        Ok(d) => d.join("tts-cache"),
        Err(e) => return json!({ "ok": false, "error": e.to_string() }),
    };
    let st = app.state::<AppState>();
    match st.tts.speak(&cache, &text, &voice, rate.unwrap_or(c.tts_rate)).await {
        Ok(url) => json!({ "ok": true, "url": url }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
fn capture_ready(app: AppHandle) {
    if !app.state::<AppState>().capturing.load(Ordering::SeqCst) {
        return;
    }
    if let Some(w) = app.get_webview_window("capture") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[tauri::command]
async fn capture_done(app: AppHandle, rect: Option<SelRect>) {
    let st = app.state::<AppState>();
    let shot = st.shot.lock().unwrap().take(); // release the full-screen image early
    end_capture(&app);
    let (Some(img), Some(r)) = (shot, rect) else { return };
    if r.width < 4.0 || r.height < 4.0 {
        return;
    }
    tokio::time::sleep(Duration::from_millis(16)).await; // let Windows hide the overlay first
    show_main(&app);
    let _ = app.emit_to("main", "ocr:start", ());
    let c = cfg(&app);
    let res = async {
        ensure_ocr(&app).await?;
        let app2 = app.clone();
        let lines = tauri::async_runtime::spawn_blocking(move || {
            let sub = crop(&img, r);
            drop(img);
            app2.state::<AppState>().ocr.recognize(&sub)
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok::<String, String>(ocr::join_lines(&lines, c.merge_lines))
    }
    .await;
    match res {
        Ok(text) => {
            let _ = app.emit_to("main", "ocr:result", json!({ "text": text, "autoSpeak": c.auto_speak }));
            if !text.is_empty() {
                translate::translate_all(&app, text);
            }
        }
        Err(e) => { let _ = app.emit_to("main", "ocr:error", e); }
    }
}

// ------------------------------------------------------------------ app
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        // raw RGBA pixels of the current screenshot, kept in memory only (never written to disk)
        .register_uri_scheme_protocol("shot", |ctx, _req| {
            let shot = ctx.app_handle().state::<AppState>().shot.lock().unwrap().clone();
            let b = tauri::http::Response::builder()
                .header("Access-Control-Allow-Origin", APP_ORIGIN)
                .header("Cache-Control", "no-store");
            match shot {
                Some(img) => b.header("Content-Type", "application/octet-stream").body(img.as_raw().clone()),
                None => b.status(404).body(Vec::new()),
            }
            .unwrap()
        })
        .invoke_handler(tauri::generate_handler![
            get_config, set_config, get_langs, ai_presets, ai_new_id, ai_test, capture_backend, translate,
            start_capture, hide, pin, fit, copy, tts_edge, capture_ready, capture_done
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let store = Store { path: app.path().app_config_dir()?.join("config.json") };
            let config = store.load();
            app.manage(AppState {
                config: Mutex::new(config),
                store,
                ocr: ocr::Ocr::default(),
                translator: translate::Translator::default(),
                tts: tts::Tts::default(),
                capturing: AtomicBool::new(false),
                shot: Mutex::new(None),
                shot_seq: AtomicU64::new(0),
                last_shown: Mutex::new(Instant::now()),
                hotkey_error: Mutex::new(None),
            });
            create_main_window(&handle)?;
            create_capture_window(&handle)?; // pre-created hidden so the hotkey responds instantly
            create_tray(&handle)?;
            register_hotkeys(&handle);
            show_main(&handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ReadChinese");
}
