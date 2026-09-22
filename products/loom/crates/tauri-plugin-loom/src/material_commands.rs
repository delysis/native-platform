//! Product-owned material commands, bound to the live workspace session.

use super::*;
use crate::materials::{
    self, MaterialEntry, MaterialEvidence, MaterialRead, MaterialRetention, MaterialSearch,
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
        materials::read(store, &id)
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
        materials::search(store, &id, &query)
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
        match retention.unwrap_or_default() {
            MaterialRetention::Ordinary => {
                materials::bind_attachment(store, &attachment_id, name.as_deref())
            }
            protected @ MaterialRetention::Protected => materials::bind_attachment_with_retention(
                store,
                &attachment_id,
                name.as_deref(),
                protected,
            ),
        }
    })
}

#[tauri::command]
pub(super) async fn material_set_pinned(
    project_id: String,
    session_id: String,
    id: String,
    pinned: bool,
    state: State<'_, PluginState>,
) -> Result<MaterialEntry, IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        materials::set_pinned(store, &id, pinned)
    })
}

#[tauri::command]
pub(super) async fn material_remove(
    project_id: String,
    session_id: String,
    id: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    with_store(&state, &project_id, &session_id, |store| {
        if let Some(root) =
            grant_root(&state).map_err(|error| materials::MaterialError::Invalid(error.message))?
        {
            materials::forget_selected_grant(store, &root, &id)?;
        }
        materials::remove(store, &id)
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
        materials::add_library_persisted(store, &path, root.as_deref())
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
        materials::add_library_persisted(store, Path::new(&path), root.as_deref())
    })
}
