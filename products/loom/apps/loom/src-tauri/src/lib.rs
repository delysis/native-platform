#![forbid(unsafe_code)]

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_loom::Builder::new().build())
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| eprintln!("Loom could not start: {error}"));
}
