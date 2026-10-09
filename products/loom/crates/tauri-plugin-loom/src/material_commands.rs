//! Product-owned material commands, bound to the live workspace session.

use super::*;
use crate::materials::{
    self, MaterialEntry, MaterialEvidence, MaterialRead, MaterialRetention, MaterialSearch,
    MetadataChange,
};

fn grant_root(state: &PluginState) -> Result<Option<PathBuf>, IpcFailure> {
    state
        .app_local_data_root
        .as_ref()
        .map(|root| {
            std::fs::create_dir_all(root).map_err(materials::MaterialError::from)?;
            Ok(std::fs::canonicalize(root)
                .map_err(materials::MaterialError::from)?
                .join("material-grants"))
        })
        .transpose()
}

pub(super) fn restore_grants(state: &PluginState, store: &ProjectStore) -> Result<(), IpcFailure> {
    let sources = materials::list(store)?;
    if !sources
        .iter()
        .any(|entry| entry.kind == materials::MaterialKind::Library && !entry.available)
    {
        return Ok(());
    }
    // Restore is opportunistic. A missing/corrupt private grant never blocks
    // ordinary writing or access to retained evidence. Using that unavailable
    // library will report a scoped re-selection error at its read boundary.
    if let Ok(Some(root)) = grant_root(state) {
        let _ = materials::restore_selected_grants(store, &root);
    }
    Ok(())
}

fn with_store<T>(
    state: &PluginState,
    project: &str,
    session_id: &str,
    action: impl FnOnce(&ProjectStore) -> Result<T, materials::MaterialError>,
) -> Result<T, IpcFailure> {
    let _admission = lock_application_admission(state, "material access")?;
    let mut session = lock_session(state)?;
    let store = require_bound_store(&mut session, project, session_id)?;
    action(store).map_err(Into::into)
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterialRenameReceipt {
    project_id: String,
    session_id: String,
    request_id: String,
    expected_metadata_revision: String,
    material: MaterialEntry,
}

fn rename_for_session(
    state: &PluginState,
    project_id: &str,
    session_id: &str,
    id: &str,
    expected_metadata_revision: &str,
    request_id: &str,
    name: &str,
) -> Result<MaterialRenameReceipt, IpcFailure> {
    // Correlates a response, not a durable command ledger or permission to retry
    // against a different metadata revision. A lost response requires a read.
    request_id.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_request_id",
            "Invalid material rename request.",
            false,
        )
    })?;
    let material = with_store(state, project_id, session_id, |store| {
        materials::change_metadata(
            store,
            id,
            expected_metadata_revision,
            MetadataChange::Rename(name),
        )?
        .ok_or_else(|| materials::MaterialError::Invalid("rename returned no material".into()))
    })?;
    Ok(MaterialRenameReceipt {
        project_id: project_id.into(),
        session_id: session_id.into(),
        request_id: request_id.into(),
        expected_metadata_revision: expected_metadata_revision.into(),
        material,
    })
}

#[tauri::command]
pub(super) async fn material_rename(
    project_id: String,
    session_id: String,
    id: String,
    expected_metadata_revision: String,
    request_id: String,
    name: String,
    state: State<'_, PluginState>,
) -> Result<MaterialRenameReceipt, IpcFailure> {
    rename_for_session(
        &state,
        &project_id,
        &session_id,
        &id,
        &expected_metadata_revision,
        &request_id,
        &name,
    )
}

fn observe_entry(
    store: &ProjectStore,
    item: Result<MaterialEntry, materials::MaterialError>,
) -> Result<MaterialEntry, materials::MaterialError> {
    materials::observed_entry(store, &item?.id)
}

#[tauri::command]
pub(super) async fn material_list(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<MaterialEntry>, IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        restore_grants(&state, store)
            .map_err(|error| materials::MaterialError::Invalid(error.message))?;
        materials::list(store)
    })
}

#[tauri::command]
pub(super) async fn material_read(
    project_id: String,
    session_id: String,
    id: String,
    state: State<'_, PluginState>,
) -> Result<MaterialRead, IpcFailure> {
    let read = with_store(&state, &project_id, &session_id, |store| {
        let mut read = materials::read(store, &id)?;
        read.material = materials::observed_entry(store, &id)?;
        Ok(read)
    })?;
    crate::material_media::bind_tokens(read, &project_id, &session_id)
}

#[tauri::command]
pub(super) async fn material_search(
    project_id: String,
    session_id: String,
    id: String,
    query: String,
    state: State<'_, PluginState>,
) -> Result<MaterialSearch, IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        restore_grants(&state, store)
            .map_err(|error| materials::MaterialError::Invalid(error.message))?;
        let mut result = materials::search(store, &id, &query)?;
        result.material = materials::observed_entry(store, &id)?;
        Ok(result)
    })
}

#[tauri::command]
pub(super) async fn material_read_evidence(
    project_id: String,
    session_id: String,
    id: String,
    evidence_id: String,
    state: State<'_, PluginState>,
) -> Result<MaterialEvidence, IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        materials::read_evidence(store, &id, &evidence_id)
    })
}

#[tauri::command]
pub(super) async fn material_bind_attachment(
    project_id: String,
    session_id: String,
    attachment_id: String,
    name: Option<String>,
    retention: Option<MaterialRetention>,
    state: State<'_, PluginState>,
) -> Result<MaterialEntry, IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        let item = match retention.unwrap_or_default() {
            MaterialRetention::Ordinary => {
                materials::bind_attachment(store, &attachment_id, name.as_deref())
            }
            protected @ MaterialRetention::Protected => materials::bind_attachment_with_retention(
                store,
                &attachment_id,
                name.as_deref(),
                protected,
            ),
        };
        observe_entry(store, item)
    })
}

#[tauri::command]
pub(super) async fn material_set_pinned(
    project_id: String,
    session_id: String,
    id: String,
    pinned: bool,
    expected_metadata_revision: String,
    state: State<'_, PluginState>,
) -> Result<MaterialEntry, IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        materials::change_metadata(
            store,
            &id,
            &expected_metadata_revision,
            MetadataChange::Pin(pinned),
        )?
        .ok_or_else(|| materials::MaterialError::Invalid("pin returned no material".into()))
    })
}

#[tauri::command]
pub(super) async fn material_remove(
    project_id: String,
    session_id: String,
    id: String,
    expected_metadata_revision: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        // Reject an obsolete remove before revoking any selected-source grant.
        materials::change_metadata(
            store,
            &id,
            &expected_metadata_revision,
            MetadataChange::Remove,
        )?;
        if let Some(root) =
            grant_root(&state).map_err(|error| materials::MaterialError::Invalid(error.message))?
        {
            materials::forget_selected_grant(store, &root, &id)?;
        }
        Ok(())
    })
}

#[tauri::command]
pub(super) async fn material_add_library<R: Runtime>(
    project_id: String,
    session_id: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<Option<MaterialEntry>, IpcFailure> {
    // Validate before showing the picker, then bind again after it returns.
    with_store(&state, &project_id, &session_id, |_| Ok(()))?;
    let Some(selected) = app
        .dialog()
        .file()
        .add_filter("SQLite library", &["sqlite", "sqlite3", "db"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = selected.into_path().map_err(|_| {
        IpcFailure::new(
            "material_path_invalid",
            "Choose a local library file.",
            false,
        )
    })?;
    with_store(&state, &project_id, &session_id, |store| {
        let root =
            grant_root(&state).map_err(|error| materials::MaterialError::Invalid(error.message))?;
        observe_entry(
            store,
            materials::add_library_persisted(store, &path, root.as_deref()),
        )
    })
    .map(Some)
}

#[tauri::command]
pub(super) async fn material_add_library_path(
    project_id: String,
    session_id: String,
    path: String,
    state: State<'_, PluginState>,
) -> Result<MaterialEntry, IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        let root =
            grant_root(&state).map_err(|error| materials::MaterialError::Invalid(error.message))?;
        observe_entry(
            store,
            materials::add_library_persisted(store, Path::new(&path), root.as_deref()),
        )
    })
}

#[cfg(all(test, unix))]
mod rename_tests {
    use super::*;

    #[test]
    fn rename_requires_live_project_session_and_rejects_replay_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let (store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        let root = store.root().to_path_buf();
        let source = root.join("source.txt");
        std::fs::write(&source, "retained original\r\n").unwrap();
        let attachment = crate::context_attachments::import_path(&root, &source).unwrap();
        let material =
            materials::bind_attachment(&store, &attachment.id, Some("Original")).unwrap();
        let material = materials::observed_entry(&store, &material.id).unwrap();
        let revision = material.metadata_revision.as_deref().unwrap();
        let project = store.manifest().project_id.to_string();
        let session_id = CommandId::new();
        let session_text = session_id.to_string();
        let request = CommandId::new().to_string();
        let state = PluginState::default();
        {
            let mut session = state.session.lock().unwrap();
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
        }
        let path = root.join(".loom/materials/bindings.json");
        let bytes = std::fs::read(&path).unwrap();
        for (project_id, session, request_id) in [
            (
                ProjectId::new().to_string(),
                session_text.clone(),
                request.clone(),
            ),
            (
                project.clone(),
                CommandId::new().to_string(),
                request.clone(),
            ),
            (
                project.clone(),
                session_text.clone(),
                "not-a-command-id".into(),
            ),
        ] {
            assert!(
                rename_for_session(
                    &state,
                    &project_id,
                    &session,
                    &material.id,
                    revision,
                    &request_id,
                    "New"
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let receipt = rename_for_session(
            &state,
            &project,
            &session_text,
            &material.id,
            revision,
            &request,
            "  新名 🦉  ",
        )
        .unwrap();
        assert_eq!(receipt.project_id, project);
        assert_eq!(receipt.session_id, session_text);
        assert_eq!(receipt.request_id, request);
        assert_eq!(receipt.expected_metadata_revision, revision);
        assert_eq!(receipt.material.id, material.id);
        assert_eq!(receipt.material.name, "  新名 🦉  ");
        let committed = std::fs::read(&path).unwrap();
        assert!(
            rename_for_session(
                &state,
                &project,
                &session_text,
                &material.id,
                revision,
                &request,
                "  新名 🦉  "
            )
            .is_err()
        );
        {
            let mut session = state.session.lock().unwrap();
            drop(session.store.take()); // release the real store lease before reopening
            session.store = Some(ProjectStore::open(&root).unwrap());
            session.active_session_id = Some(CommandId::new());
        }
        assert!(
            rename_for_session(
                &state,
                &project,
                &session_text,
                &material.id,
                receipt.material.metadata_revision.as_deref().unwrap(),
                &CommandId::new().to_string(),
                "Wrong session"
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), committed);
        assert_eq!(std::fs::read(&source).unwrap(), b"retained original\r\n");
    }
}
