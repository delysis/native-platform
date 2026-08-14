#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use loom_types::BuildModelPolicy;
use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Runtime};

const EMBEDDED_BUILD_MODEL_POLICY: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/loom-build-model-policy.json"));
const EMBEDDED_BUILD_MODEL_POLICY_NAME: &str = env!("LOOM_BUILD_MODEL_POLICY_NAME");
const EMBEDDED_BUILD_MODEL_POLICY_SHA256: &str = env!("LOOM_BUILD_MODEL_POLICY_SHA256");
const APPLICATION_QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";
const ACCEPTANCE_DIRECTORY_ENV: &str = "DELYSIS_LOOM_ACCEPTANCE_DIR";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let Ok(build_model_policy) = embedded_build_model_policy() else {
        eprintln!("Loom's embedded model policy failed its integrity check");
        return;
    };
    let acceptance_app_local_data_root = match acceptance_app_local_data_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("Loom refused its acceptance directory: {error}");
            return;
        }
    };
    let loom_plugin = tauri_plugin_loom::Builder::new()
        .with_build_model_policy(build_model_policy)
        .with_app_local_data_root(acceptance_app_local_data_root);
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

fn acceptance_app_local_data_root() -> Result<Option<PathBuf>, String> {
    acceptance_app_local_data_root_from(
        std::env::var_os(ACCEPTANCE_DIRECTORY_ENV),
        &std::env::temp_dir(),
    )
}

fn acceptance_app_local_data_root_from(
    configured: Option<OsString>,
    temporary_directory: &Path,
) -> Result<Option<PathBuf>, String> {
    let Some(configured) = configured else {
        return Ok(None);
    };
    let configured = PathBuf::from(configured);
    if !configured.is_absolute() {
        return Err(format!(
            "{ACCEPTANCE_DIRECTORY_ENV} must name an absolute path"
        ));
    }
    let metadata = configured.symlink_metadata().map_err(|error| {
        format!("{ACCEPTANCE_DIRECTORY_ENV} must name an existing directory: {error}")
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{ACCEPTANCE_DIRECTORY_ENV} must not name a symbolic link"
        ));
    }
    if !metadata.is_dir() {
        return Err(format!("{ACCEPTANCE_DIRECTORY_ENV} must name a directory"));
    }

    let configured = configured.canonicalize().map_err(|error| {
        format!("{ACCEPTANCE_DIRECTORY_ENV} could not be resolved safely: {error}")
    })?;
    let temporary_directory = temporary_directory
        .canonicalize()
        .map_err(|error| format!("the operating-system temporary directory is invalid: {error}"))?;
    if configured == temporary_directory || !configured.starts_with(&temporary_directory) {
        return Err(format!(
            "{ACCEPTANCE_DIRECTORY_ENV} must be a child of {}",
            temporary_directory.display()
        ));
    }

    Ok(Some(configured))
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
    if policy.identity().canonical_sha256() != digest {
        return Err("embedded policy does not match its closed compile-time identity".to_owned());
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::tempdir;

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
    fn build_sources_have_no_writer_path_embedding_channel() {
        let build_source = include_str!("../build.rs");
        let runtime_source = include_str!("lib.rs");
        let removed_environment_variable = concat!("LOOM_BUILD_WRITER_", "MODEL_PATH");
        let removed_generated_file = concat!("loom-build-writer-", "model-path.txt");

        assert!(!build_source.contains(removed_environment_variable));
        assert!(!build_source.contains(removed_generated_file));
        assert!(!runtime_source.contains(removed_environment_variable));
        assert!(!runtime_source.contains(removed_generated_file));
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

    #[test]
    fn acceptance_directory_is_opt_in() {
        let temporary_directory = tempdir().expect("temporary directory");
        assert_eq!(
            acceptance_app_local_data_root_from(None, temporary_directory.path())
                .expect("unset override"),
            None
        );
    }

    #[test]
    fn acceptance_directory_accepts_an_existing_absolute_temp_child() {
        let temporary_directory = tempdir().expect("temporary directory");
        let acceptance_directory = temporary_directory.path().join("loom-acceptance");
        fs::create_dir(&acceptance_directory).expect("acceptance directory");

        let resolved = acceptance_app_local_data_root_from(
            Some(acceptance_directory.clone().into_os_string()),
            temporary_directory.path(),
        )
        .expect("valid acceptance directory");

        assert_eq!(
            resolved,
            Some(
                acceptance_directory
                    .canonicalize()
                    .expect("canonical acceptance directory")
            )
        );
    }

    #[test]
    fn acceptance_directory_rejects_relative_missing_and_non_directory_paths() {
        let temporary_directory = tempdir().expect("temporary directory");
        let missing = temporary_directory.path().join("missing");
        let file = temporary_directory.path().join("file");
        fs::write(&file, b"not a directory").expect("test file");

        for configured in [PathBuf::from("relative"), missing, file] {
            assert!(
                acceptance_app_local_data_root_from(
                    Some(configured.into_os_string()),
                    temporary_directory.path(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn acceptance_directory_rejects_the_temp_root_and_paths_outside_it() {
        let temporary_directory = tempdir().expect("temporary directory");
        let outside_directory = tempdir().expect("outside directory");

        for configured in [temporary_directory.path(), outside_directory.path()] {
            assert!(
                acceptance_app_local_data_root_from(
                    Some(configured.as_os_str().to_owned()),
                    temporary_directory.path(),
                )
                .is_err()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn acceptance_directory_rejects_a_symbolic_link_leaf() {
        use std::os::unix::fs::symlink;

        let temporary_directory = tempdir().expect("temporary directory");
        let target = temporary_directory.path().join("target");
        let link = temporary_directory.path().join("link");
        fs::create_dir(&target).expect("target directory");
        symlink(&target, &link).expect("symbolic link");

        assert!(
            acceptance_app_local_data_root_from(
                Some(link.into_os_string()),
                temporary_directory.path(),
            )
            .is_err()
        );
    }
}
