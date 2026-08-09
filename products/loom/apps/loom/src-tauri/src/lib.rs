#![forbid(unsafe_code)]

use loom_types::BuildModelPolicy;
use std::path::PathBuf;
use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Runtime};

const EMBEDDED_BUILD_MODEL_POLICY: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/loom-build-model-policy.json"));
const EMBEDDED_BUILD_MODEL_POLICY_NAME: &str = env!("LOOM_BUILD_MODEL_POLICY_NAME");
const EMBEDDED_BUILD_MODEL_POLICY_SHA256: &str = env!("LOOM_BUILD_MODEL_POLICY_SHA256");
const EMBEDDED_BUILD_WRITER_MODEL_PATH: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/loom-build-writer-model-path.txt"
));
const APPLICATION_QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let Ok(build_model_policy) = embedded_build_model_policy() else {
        eprintln!("Loom's embedded model policy failed its integrity check");
        return;
    };
    let Ok(build_writer_model_path) = embedded_build_writer_model_path() else {
        eprintln!("Loom's embedded writer model path failed its integrity check");
        return;
    };
    let mut loom_plugin =
        tauri_plugin_loom::Builder::new().with_build_model_policy(build_model_policy);
    if let Some(model_path) = build_writer_model_path {
        loom_plugin = loom_plugin.with_additional_policy_model_path(model_path);
    }
    tauri::Builder::default()
        // Tauri's stock macOS Quit item calls AppKit `terminate:` directly and
        // bypasses RunEvent::ExitRequested. Loom owns a regular Cmd+Q menu item
        // so every graceful quit enters the joined-worker close coordinator.
        .enable_macos_default_menu(false)
        .menu(build_desktop_menu)
        .plugin(tauri_plugin_dialog::init())
        .plugin(loom_plugin.build())
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| eprintln!("Loom could not start: {error}"));
}

fn build_desktop_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let package = app.package_info();
    let about = AboutMetadata {
        name: Some(package.name.clone()),
        version: Some(package.version.to_string()),
        copyright: app.config().bundle.copyright.clone(),
        authors: app
            .config()
            .bundle
            .publisher
            .clone()
            .map(|value| vec![value]),
        ..AboutMetadata::default()
    };
    let quit = MenuItem::with_id(
        app,
        tauri_plugin_loom::APPLICATION_QUIT_MENU_ID,
        format!("Quit {}", package.name),
        true,
        Some(APPLICATION_QUIT_ACCELERATOR),
    )?;
    let window = Submenu::with_id_and_items(
        app,
        WINDOW_SUBMENU_ID,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    let help = Submenu::with_id_and_items(
        app,
        HELP_SUBMENU_ID,
        "Help",
        true,
        &[
            #[cfg(not(target_os = "macos"))]
            &PredefinedMenuItem::about(app, None, Some(about.clone()))?,
        ],
    )?;

    Menu::with_items(
        app,
        &[
            #[cfg(target_os = "macos")]
            &Submenu::with_items(
                app,
                package.name.clone(),
                true,
                &[
                    &PredefinedMenuItem::about(app, None, Some(about))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ],
            )?,
            &Submenu::with_items(
                app,
                "File",
                true,
                &[
                    &PredefinedMenuItem::close_window(app, None)?,
                    #[cfg(not(target_os = "macos"))]
                    &quit,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            #[cfg(target_os = "macos")]
            &Submenu::with_items(
                app,
                "View",
                true,
                &[&PredefinedMenuItem::fullscreen(app, None)?],
            )?,
            &window,
            &help,
        ],
    )
}

fn embedded_build_writer_model_path() -> Result<Option<PathBuf>, String> {
    if EMBEDDED_BUILD_WRITER_MODEL_PATH.is_empty() {
        return Ok(None);
    }
    let value =
        std::str::from_utf8(EMBEDDED_BUILD_WRITER_MODEL_PATH).map_err(|error| error.to_string())?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("embedded writer model path is not absolute".to_owned());
    }
    Ok(Some(path))
}

fn embedded_build_model_policy() -> Result<BuildModelPolicy, String> {
    let policy = BuildModelPolicy::from_json_slice(EMBEDDED_BUILD_MODEL_POLICY)
        .map_err(|error| error.to_string())?;
    if policy.name().as_str() != EMBEDDED_BUILD_MODEL_POLICY_NAME {
        return Err("embedded policy name does not match its build identity".to_owned());
    }
    let digest = policy
        .canonical_digest()
        .map_err(|error| error.to_string())?;
    if digest.to_string() != EMBEDDED_BUILD_MODEL_POLICY_SHA256 {
        return Err("embedded policy digest does not match its build identity".to_owned());
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_policy_is_canonical_and_bound_to_its_build_identity() {
        let policy = embedded_build_model_policy().expect("valid embedded model policy");
        assert_eq!(policy.name().as_str(), EMBEDDED_BUILD_MODEL_POLICY_NAME);
        assert_eq!(
            policy.canonical_json().expect("canonical policy"),
            EMBEDDED_BUILD_MODEL_POLICY
        );
    }

    #[test]
    fn optional_embedded_writer_path_is_absent_or_absolute() {
        assert!(
            embedded_build_writer_model_path()
                .expect("valid embedded writer model path")
                .is_none_or(|path| path.is_absolute())
        );
    }

    #[test]
    fn macos_quit_source_cannot_reintroduce_appkit_terminate_bypass() {
        let source = include_str!("lib.rs");
        let predefined_quit = concat!("PredefinedMenuItem::", "quit");
        let predefined_quit_with_text = concat!("PredefinedMenuItem::", "quit_with_text");
        let default_menu = concat!("Menu::", "default");

        assert!(source.contains("enable_macos_default_menu(false)"));
        assert!(source.contains("APPLICATION_QUIT_MENU_ID"));
        assert!(!source.contains(predefined_quit));
        assert!(!source.contains(predefined_quit_with_text));
        assert!(!source.contains(default_menu));
    }
}
