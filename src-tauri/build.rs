// Declaring the app's commands lets each capability file allow only the commands a window needs
// (the capture overlay cannot call set_config, translate, ...).
const COMMANDS: &[&str] = &[
    "get_config", "set_config", "get_langs", "ai_presets", "ai_new_id", "ai_test",
    "capture_backend", "translate", "start_capture", "hide", "pin", "fit", "copy",
    "tts_edge", "capture_ready", "capture_done",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
