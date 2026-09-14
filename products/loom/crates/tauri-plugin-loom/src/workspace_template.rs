//! Mine settings are plain TOML, edited through the ordinary document store.
//! Reading them never starts a download, model, or tool.

use super::*;
pub(super) use loom_config::valid_pane_id;
use loom_config::{CONFIG_FILE, MineConfig, WorkspaceConfig};

const DEFAULT_TEMPLATE: &str = r#"# Mine settings. Omitted values inherit defaults.
version = 1

# [assistance]
# suggestions = true

# [workspace.panes.chat]
# visible = true
# context = ["@document"]

# [workspace.panes.terminal]
# visible = true

# [workspace.panes.browser]
# visible = true
# document = "@.browser"

# Reload model settings explicitly with Command/Ctrl-Shift-R.
# [model]
# path = "../models/writer.gguf"
# context_tokens = 8192
# batch_tokens = 512
# max_sequences = 4

# [generation]
# manual_writing = "my_writer"

# [profiles.my_writer]
# context_file = ".mine/personas/my_writer.md"
# [profiles.my_writer.sampling]
# temperature = 0.7
# top_p = 0.95
"#;

/// Answers to the optional setup flow. No model or download authority lives here.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetupChoices {
    chat: bool,
    suggestions: bool,
}

fn setup_source(choices: SetupChoices) -> String {
    format!(
        "{DEFAULT_TEMPLATE}\n[assistance]\nsuggestions = {}\n\n[workspace.panes.chat]\nvisible = {}\n",
        choices.suggestions, choices.chat
    )
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceTemplateSnapshot {
    enabled: bool,
    document_id: Option<String>,
    revision_id: Option<String>,
    source_sha256: Option<String>,
    suggestions: Option<bool>,
    model_path: Option<String>,
    config: WorkspaceConfig,
    error: Option<String>,
}

fn resolved_snapshot(
    parsed: Result<MineConfig, loom_config::ConfigError>,
    root: &std::path::Path,
    document_id: Option<String>,
    revision_id: Option<String>,
) -> WorkspaceTemplateSnapshot {
    match parsed {
        Ok(settings) => WorkspaceTemplateSnapshot {
            enabled: settings.source_sha256().is_some(),
            document_id,
            revision_id,
            source_sha256: settings.source_sha256().map(str::to_owned),
            suggestions: settings.assistance.suggestions,
            model_path: settings
                .model
                .model_path(root)
                .map(|path| path.to_string_lossy().into_owned()),
            // Parsing has already validated the same immutable workspace value.
            config: settings.workspace.resolve().expect("validated workspace"),
            error: None,
        },
        Err(error) => WorkspaceTemplateSnapshot {
            enabled: true,
            document_id,
            revision_id,
            source_sha256: None,
            suggestions: None,
            model_path: None,
            config: WorkspaceConfig::default(),
            error: Some(error.to_string()),
        },
    }
}

fn snapshot(store: &mut ProjectStore) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let document = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .find(|document| document.relative_path == CONFIG_FILE);
    let Some(document) = document else {
        // A hand-authored dotfile works without registering it as a document.
        // Reads create neither a file nor a document/revision record.
        return Ok(resolved_snapshot(
            MineConfig::read(store.root()),
            store.root(),
            None,
            None,
        ));
    };
    store
        .import_external_changes_if_uncontested(CONFIG_FILE, "Read external Mine settings")
        .map_err(IpcFailure::store)?;
    let loaded = store
        .read_document(CONFIG_FILE)
        .map_err(IpcFailure::store)?;
    Ok(resolved_snapshot(
        MineConfig::parse(&loaded.text),
        store.root(),
        Some(document.document_id.to_string()),
        Some(loaded.revision_id.to_string()),
    ))
}

fn enable(
    store: &mut ProjectStore,
    choices: Option<SetupChoices>,
) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let current = snapshot(store)?;
    if current.document_id.is_some() {
        return Ok(current);
    }
    match std::fs::symlink_metadata(store.root().join(CONFIG_FILE)) {
        Ok(_) => {
            store
                .adopt_visible_document_if_absent(
                    CONFIG_FILE,
                    DocumentKind::Prose,
                    "Open Mine settings",
                )
                .map_err(IpcFailure::store)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let source = choices.map_or_else(|| DEFAULT_TEMPLATE.into(), setup_source);
            // Validate generated settings before entering the store transaction.
            MineConfig::parse(&source).map_err(|error| {
                IpcFailure::new("workspace_settings_invalid", error.to_string(), false)
            })?;
            store
                .create_document_if_absent(
                    CONFIG_FILE,
                    DocumentContent::Prose(source),
                    "Configure Mine",
                )
                .map_err(IpcFailure::store)?;
        }
        Err(error) => {
            return Err(IpcFailure::new(
                "workspace_template_failed",
                error.to_string(),
                false,
            ));
        }
    }
    snapshot(store)
}

#[tauri::command]
pub(super) async fn workspace_template_get(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let _admission = lock_application_admission(&state, "workspace configuration")?;
    let mut session = lock_session(&state)?;
    snapshot(require_bound_store(&mut session, &project_id, &session_id)?)
}

#[tauri::command]
pub(super) async fn workspace_template_enable(
    project_id: String,
    session_id: String,
    choices: Option<SetupChoices>,
    state: State<'_, PluginState>,
) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let _admission = lock_application_admission(&state, "workspace configuration")?;
    let mut session = lock_session(&state)?;
    enable(
        require_bound_store(&mut session, &project_id, &session_id)?,
        choices,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_setup_answer_reaches_settings_without_enabling_other_panes() {
        for chat in [false, true] {
            for suggestions in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let (mut store, _) =
                    ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
                let result = enable(&mut store, Some(SetupChoices { chat, suggestions })).unwrap();
                assert_eq!(result.suggestions, Some(suggestions));
                assert_eq!(result.config.panes["chat"].visible, chat);
                assert!(!result.config.panes["terminal"].visible);
                let first = store.read_document(CONFIG_FILE).unwrap();
                let retried = enable(
                    &mut store,
                    Some(SetupChoices {
                        chat: !chat,
                        suggestions: !suggestions,
                    }),
                )
                .unwrap();
                assert_eq!(retried.revision_id, result.revision_id);
                assert_eq!(store.read_document(CONFIG_FILE).unwrap().text, first.text);
            }
        }
    }

    #[test]
    fn reading_is_nonmutating_and_editing_existing_content_preserves_exact_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        assert!(!snapshot(&mut store).unwrap().enabled);
        assert!(!store.root().join(CONFIG_FILE).exists());
        let document_count = store.list_documents().unwrap().len();
        let text = "# My choices\r\n[workspace.panes.chat]\r\nvisible=true\r\n";
        std::fs::write(store.root().join(CONFIG_FILE), text).unwrap();
        let result = snapshot(&mut store).unwrap();
        assert!(result.enabled && result.config.panes["chat"].visible);
        assert!(result.document_id.is_none());
        assert_eq!(store.list_documents().unwrap().len(), document_count);
        let adopted = enable(
            &mut store,
            Some(SetupChoices {
                chat: false,
                suggestions: false,
            }),
        )
        .unwrap();
        assert!(adopted.document_id.is_some());
        assert_eq!(store.read_document(CONFIG_FILE).unwrap().text, text);
        assert_eq!(
            std::fs::read_to_string(store.root().join(CONFIG_FILE)).unwrap(),
            text
        );
    }

    #[test]
    fn invalid_settings_remain_available_for_repair() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        let text = "# Mine\r\nunknown = true\r\n";
        std::fs::write(store.root().join(CONFIG_FILE), text).unwrap();
        assert!(snapshot(&mut store).unwrap().error.is_some());
        let result = enable(&mut store, None).unwrap();
        assert!(result.enabled && result.error.is_some() && result.document_id.is_some());
        assert_eq!(store.read_document(CONFIG_FILE).unwrap().text, text);
    }

    #[test]
    fn explicit_defaults_are_quiet_and_legacy_document_is_ordinary_prose() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        let legacy = "# An old workspace\n```loom-workspace\n[panes.chat]\nvisible=true\n```\n";
        std::fs::write(store.root().join(".loom.md"), legacy).unwrap();
        assert!(!snapshot(&mut store).unwrap().enabled);
        let result = enable(&mut store, None).unwrap();
        assert!(!result.config.panes["chat"].visible);
        assert_eq!(
            std::fs::read_to_string(store.root().join(".loom.md")).unwrap(),
            legacy
        );
    }

    #[test]
    fn external_settings_refresh_imports_only_uncontested_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        let original = enable(&mut store, None).unwrap();
        let external = "# My settings\r\n[workspace.panes.chat]\r\nvisible=true\r\n";
        std::fs::write(store.root().join(CONFIG_FILE), external).unwrap();
        let refreshed = snapshot(&mut store).unwrap();
        assert!(refreshed.config.panes["chat"].visible);
        assert_ne!(refreshed.revision_id, original.revision_id);
        assert_eq!(store.read_document(CONFIG_FILE).unwrap().text, external);
        assert_eq!(
            snapshot(&mut store).unwrap().revision_id,
            refreshed.revision_id
        );
        let base = store.read_document(CONFIG_FILE).unwrap();
        store
            .upsert_transient_draft(
                CONFIG_FILE,
                base.revision_id,
                0,
                DocumentContent::Prose("Distinct local writing".into()),
            )
            .unwrap();
        std::fs::write(store.root().join(CONFIG_FILE), "A conflicting disk edit").unwrap();
        assert!(snapshot(&mut store).is_err());
        assert_eq!(
            store
                .load_transient_draft(CONFIG_FILE)
                .unwrap()
                .unwrap()
                .text,
            "Distinct local writing"
        );
        assert_eq!(
            store
                .reconciliation_snapshot(CONFIG_FILE)
                .unwrap()
                .active_revision_id,
            base.revision_id
        );
    }
}
