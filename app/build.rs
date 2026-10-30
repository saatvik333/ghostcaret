/// Every command the web UI may call. Tauri generates an `allow-<name>`
/// permission for each; `capabilities/main.json` grants them to the
/// settings window only, and anything not listed is refused.
const COMMANDS: &[&str] = &[
    "overview",
    "save_settings",
    "check_keys",
    "set_api_key",
    "remove_provider",
    "test_provider",
    "provider_models",
    "sent_log",
    "clear_sent_log",
    "usage",
    "clear_usage",
    "seen_apps",
    "style_samples",
    "delete_style",
    "set_setup_step",
    "pause",
    "resume",
    "timings",
    "local_models",
    "download_model",
    "cancel_download",
    "delete_model",
    "check_model_file",
];

fn main() {
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    tauri_build::try_build(attributes).expect("tauri build step");
}
