#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use loom_types::BuildModelPolicy;
use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
};
use tauri::utils::config::WindowConfig;
use tauri::{AppHandle, Runtime};

const EMBEDDED_BUILD_MODEL_POLICY: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/loom-build-model-policy.json"));
const EMBEDDED_BUILD_MODEL_POLICY_NAME: &str = env!("LOOM_BUILD_MODEL_POLICY_NAME");
const EMBEDDED_BUILD_MODEL_POLICY_SHA256: &str = env!("LOOM_BUILD_MODEL_POLICY_SHA256");
const APPLICATION_QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";
const FILE_NEW_DOCUMENT_ACCELERATOR: &str = "CmdOrCtrl+N";
const FILE_OPEN_PROJECT_ACCELERATOR: &str = "CmdOrCtrl+O";
const FILE_SAVE_ACCELERATOR: &str = "CmdOrCtrl+S";
const FILE_EXPORT_COPY_ACCELERATOR: &str = "CmdOrCtrl+Shift+S";
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
    let isolate_model_discovery = acceptance_app_local_data_root.is_some();
    let acceptance_data_store = acceptance_app_local_data_root
        .as_deref()
        .map(acceptance_data_store_identifier);
    let loom_plugin = tauri_plugin_loom::Builder::new()
        .with_build_model_policy(build_model_policy)
        .with_app_local_data_root(acceptance_app_local_data_root)
        .with_isolated_model_discovery(isolate_model_discovery);
    let (context, acceptance_windows) = application_context(acceptance_data_store);
    tauri::Builder::default()
        // Tauri's stock macOS Quit item calls AppKit `terminate:` directly and
        // bypasses RunEvent::ExitRequested. Loom owns a regular Cmd+Q menu item
        // so every graceful quit enters the joined-worker close coordinator.
        .enable_macos_default_menu(false)
        .menu(build_desktop_menu)
        .plugin(tauri_plugin_dialog::init())
        .plugin(loom_plugin.build())
        .setup(move |app| {
            if let Some(identifier) = acceptance_data_store {
                for window in &acceptance_windows {
                    // Tauri 2.11.5 drops this field in WebviewAttributes::from
                    // WindowConfig. Use the explicit runtime builder setter.
                    tauri::WebviewWindowBuilder::from_config(app, window)?
                        .data_store_identifier(identifier)
                        .build()?;
                }
            }
            Ok(())
        })
        .run(context)
        .unwrap_or_else(|error| eprintln!("Loom could not start: {error}"));
}

// Native project storage and renderer storage are independent. Acceptance
// defers only the automatically created windows, then creates them once in
// setup with the explicit persistent-store setter. Normal startup is unchanged.
fn application_context(
    acceptance_data_store: Option<[u8; 16]>,
) -> (tauri::Context<tauri::Wry>, Vec<WindowConfig>) {
    let mut context = tauri::generate_context!();
    let windows = if acceptance_data_store.is_some() {
        defer_acceptance_windows(&mut context.config_mut().app.windows)
    } else {
        Vec::new()
    };
    (context, windows)
}

fn defer_acceptance_windows(windows: &mut [WindowConfig]) -> Vec<WindowConfig> {
    let mut deferred = Vec::new();
    for window in windows.iter_mut().filter(|window| window.create) {
        deferred.push(window.clone());
        window.create = false;
    }
    deferred
}

fn acceptance_data_store_identifier(root: &Path) -> [u8; 16] {
    let mut digest = Sha256::new();
    digest.update(b"delysis-loom-acceptance-webview-v1\0");
    digest.update(root.to_string_lossy().as_bytes());
    digest.finalize()[..16]
        .try_into()
        .expect("SHA-256 prefix has a fixed length")
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
    let file = build_file_menu(app, &quit)?;
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
            &file,
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

fn build_file_menu<R: Runtime>(
    app: &AppHandle<R>,
    quit: &MenuItem<R>,
) -> tauri::Result<Submenu<R>> {
    #[cfg(target_os = "macos")]
    let _ = quit;
    let new_document = MenuItem::with_id(
        app,
        tauri_plugin_loom::FILE_NEW_DOCUMENT_MENU_ID,
        "New Document",
        true,
        Some(FILE_NEW_DOCUMENT_ACCELERATOR),
    )?;
    let open_project = MenuItem::with_id(
        app,
        tauri_plugin_loom::FILE_OPEN_PROJECT_MENU_ID,
        "Open Folder…",
        true,
        Some(FILE_OPEN_PROJECT_ACCELERATOR),
    )?;
    let save = MenuItem::with_id(
        app,
        tauri_plugin_loom::FILE_SAVE_MENU_ID,
        "Save",
        true,
        Some(FILE_SAVE_ACCELERATOR),
    )?;
    let export_copy = MenuItem::with_id(
        app,
        tauri_plugin_loom::FILE_EXPORT_COPY_MENU_ID,
        "Export Text…",
        true,
        Some(FILE_EXPORT_COPY_ACCELERATOR),
    )?;
    Submenu::with_items(
        app,
        "File",
        true,
        &[
            &new_document,
            &open_project,
            &PredefinedMenuItem::separator(app)?,
            &save,
            &export_copy,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
            #[cfg(not(target_os = "macos"))]
            quit,
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
    fn native_file_menu_exposes_standard_document_accelerators() {
        assert_eq!(FILE_NEW_DOCUMENT_ACCELERATOR, "CmdOrCtrl+N");
        assert_eq!(FILE_OPEN_PROJECT_ACCELERATOR, "CmdOrCtrl+O");
        assert_eq!(FILE_SAVE_ACCELERATOR, "CmdOrCtrl+S");
        assert_eq!(FILE_EXPORT_COPY_ACCELERATOR, "CmdOrCtrl+Shift+S");

        let source = include_str!("lib.rs");
        for menu_id in [
            "FILE_NEW_DOCUMENT_MENU_ID",
            "FILE_OPEN_PROJECT_MENU_ID",
            "FILE_SAVE_MENU_ID",
            "FILE_EXPORT_COPY_MENU_ID",
        ] {
            assert!(
                source.contains(menu_id),
                "missing native file menu item {menu_id}"
            );
        }
        for label in ["New Document", "Open Folder…", "Save", "Export Text…"] {
            assert!(
                source.contains(label),
                "missing native file menu label {label}"
            );
        }
    }

    // These tests exercise window ownership, not WebKit persistence. The
    // native two-directory/relaunch check must verify actual stored values.
    #[test]
    fn acceptance_windows_are_deferred_once_without_changing_their_configuration() {
        let identifier = acceptance_data_store_identifier(Path::new("/tmp/loom-acceptance"));
        let (normal, normal_deferred) = application_context(None);
        let (isolated, deferred) = application_context(Some(identifier));
        assert!(normal_deferred.is_empty());
        let automatic: Vec<_> = normal
            .config()
            .app
            .windows
            .iter()
            .filter(|window| window.create)
            .collect();
        assert!(!automatic.is_empty(), "the real app must create a window");
        assert_eq!(automatic.len(), deferred.len());
        assert!(
            isolated
                .config()
                .app
                .windows
                .iter()
                .all(|window| !window.create)
        );
        for (normal, deferred) in automatic.into_iter().zip(&deferred) {
            assert!(deferred.create);
            assert!(!deferred.incognito, "acceptance must preserve preferences");
            assert_eq!(normal.label, deferred.label);
            assert_eq!(normal.url, deferred.url);
            assert_eq!(normal.title, deferred.title);
            assert_eq!(normal.width.to_bits(), deferred.width.to_bits());
            assert_eq!(normal.height.to_bits(), deferred.height.to_bits());
        }
    }

    #[test]
    fn deferral_does_not_create_template_windows_or_duplicate_ownership() {
        let mut windows = [
            WindowConfig::default(),
            WindowConfig {
                label: "template".to_owned(),
                create: false,
                ..WindowConfig::default()
            },
        ];
        let deferred = defer_acceptance_windows(&mut windows);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].label, windows[0].label);
        assert!(windows.iter().all(|window| !window.create));
        assert!(defer_acceptance_windows(&mut windows).is_empty());
    }

    #[test]
    fn acceptance_context_preserves_application_identity() {
        let (normal, _) = application_context(None);
        let (isolated, _) = application_context(Some([7; 16]));
        assert_eq!(normal.config().identifier, isolated.config().identifier);
        assert_eq!(normal.config().product_name, isolated.config().product_name);
        assert_eq!(normal.package_info().name, isolated.package_info().name);
        assert_eq!(
            normal.package_info().version,
            isolated.package_info().version
        );
    }

    #[test]
    fn acceptance_data_store_identity_is_stable_and_directory_scoped() {
        let first = acceptance_data_store_identifier(Path::new("/tmp/loom-a"));
        assert_eq!(
            first,
            acceptance_data_store_identifier(Path::new("/tmp/loom-a"))
        );
        assert_ne!(
            first,
            acceptance_data_store_identifier(Path::new("/tmp/loom-b"))
        );
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
