//! A workspace outlives the document root currently displayed in its window.
//! Its store moves between the active slot and this parked slot; it is never
//! reopened under a second lease when the writer switches back to it.
use crate::{CommandId, IpcFailure, PluginState, ProjectId, ProjectStore, Session};
use std::path::PathBuf;

#[derive(Debug)]
pub(super) struct Owner {
    pub project_id: ProjectId,
    pub session_id: CommandId,
    pub root: PathBuf,
    parked: Option<ProjectStore>,
    pub filesystem_watcher: Option<crate::DocumentFilesystemWatcher>,
}

pub(super) fn establish(session: &mut Session, store: &ProjectStore) {
    if session.workspace.is_none() {
        session.workspace = Some(Owner {
            project_id: store.manifest().project_id,
            session_id: CommandId::new(),
            root: store.root().to_owned(),
            parked: None,
            filesystem_watcher: None,
        });
    }
}

fn unavailable() -> IpcFailure {
    IpcFailure::new("workspace_not_open", "Open a workspace first.", false)
}

pub(super) fn is_bound(session: &Session, project_id: ProjectId, session_id: CommandId) -> bool {
    session
        .workspace
        .as_ref()
        .is_some_and(|owner| owner.project_id == project_id && owner.session_id == session_id)
}

pub(super) fn store(session: &Session) -> Result<&ProjectStore, IpcFailure> {
    let owner = session.workspace.as_ref().ok_or_else(unavailable)?;
    owner
        .parked
        .as_ref()
        .or_else(|| {
            session.store.as_ref().filter(|store| {
                store.manifest().project_id == owner.project_id && store.root() == owner.root
            })
        })
        .ok_or_else(unavailable)
}

pub(super) fn store_mut(session: &mut Session) -> Result<&mut ProjectStore, IpcFailure> {
    let owner = session.workspace.as_mut().ok_or_else(unavailable)?;
    if owner.parked.is_some() {
        return owner.parked.as_mut().ok_or_else(unavailable);
    }
    session
        .store
        .as_mut()
        .filter(|store| {
            store.manifest().project_id == owner.project_id && store.root() == owner.root
        })
        .ok_or_else(unavailable)
}

/// Both session tokens are captured at admission. A serialized source path can
/// select between these already-owned stores, but cannot grant filesystem access.
pub(super) fn read_context<'a>(
    session: &'a Session,
    project_id: &str,
    session_id: &str,
    owner_project_id: &str,
    owner_session_id: &str,
) -> Result<crate::material_context::ReadContext<'a>, IpcFailure> {
    if session.phase != crate::SessionPhase::Open
        || session
            .active_session_id
            .is_none_or(|id| id.to_string() != session_id)
    {
        return Err(IpcFailure::new(
            "stale_project_session",
            "The document session has ended.",
            false,
        ));
    }
    let documents = session
        .store
        .as_ref()
        .filter(|store| store.manifest().project_id.to_string() == project_id)
        .ok_or_else(|| {
            IpcFailure::new(
                "project_identity_mismatch",
                "The document belongs to a different project.",
                false,
            )
        })?;
    let owner = session.workspace.as_ref().ok_or_else(unavailable)?;
    if owner.project_id.to_string() != owner_project_id
        || owner.session_id.to_string() != owner_session_id
    {
        return Err(IpcFailure::new(
            "stale_workspace_session",
            "The source workspace session has ended.",
            false,
        ));
    }
    Ok(crate::material_context::ReadContext {
        documents,
        materials: store(session)?,
    })
}

/// Workspace work survives document-root transitions, but never an owner change.
pub(super) fn require_store_mut<'a>(
    session: &'a mut Session,
    project_id: &str,
    session_id: &str,
) -> Result<&'a mut ProjectStore, IpcFailure> {
    let owner = session.workspace.as_ref().ok_or_else(unavailable)?;
    if owner.project_id.to_string() != project_id || owner.session_id.to_string() != session_id {
        return Err(IpcFailure::new(
            "stale_workspace_session",
            "This operation belongs to a different workspace.",
            false,
        ));
    }
    store_mut(session)
}

pub(super) fn park_active(session: &mut Session) {
    if let Some(store) = session.store.take() {
        restore_parked(session, store);
    }
}

pub(super) fn restore_parked(session: &mut Session, store: ProjectStore) {
    if let Some(owner) = &mut session.workspace
        && store.manifest().project_id == owner.project_id
        && store.root() == owner.root
    {
        owner.parked = Some(store);
    }
}

pub(super) fn take_parked(session: &mut Session) -> Result<ProjectStore, IpcFailure> {
    session
        .workspace
        .as_mut()
        .and_then(|owner| owner.parked.take())
        .ok_or_else(unavailable)
}

pub(super) fn private_root(state: &PluginState) -> Result<PathBuf, IpcFailure> {
    let parent = state.app_local_data_root.as_deref().ok_or_else(|| {
        IpcFailure::new(
            "workspace_grants_unavailable",
            "The application data directory is unavailable.",
            true,
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        IpcFailure::new("workspace_grants_unavailable", error.to_string(), true)
    })?;
    Ok(parent.join("workspace-roots"))
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Snapshot {
    workspace_id: String,
    workspace_session_id: String,
    roots: Vec<crate::workspace_roots::RootDescriptor>,
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri owns deserialized command arguments.
pub(crate) fn workspace_roots_get(
    project_id: String,
    session_id: String,
    state: tauri::State<'_, PluginState>,
) -> Result<Snapshot, IpcFailure> {
    let mut session = crate::lock_session(&state)?;
    crate::require_bound_store(&mut session, &project_id, &session_id)?;
    let owner = session.workspace.as_ref().ok_or_else(unavailable)?;
    let workspace_id = owner.project_id.to_string();
    let workspace_session_id = owner.session_id.to_string();
    let owner_store = store_mut(&mut session)?;
    crate::workspace_template::load_template(owner_store)?;
    let roots = crate::workspace_roots::list(owner_store, &private_root(&state)?)?;
    Ok(Snapshot {
        workspace_id,
        workspace_session_id,
        roots,
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri owns deserialized command arguments.
pub(crate) fn workspace_root_prepare(
    project_id: String,
    session_id: String,
    root_id: String,
    state: tauri::State<'_, PluginState>,
) -> Result<Option<String>, IpcFailure> {
    let picker = crate::reserve_folder_picker(&state)?;
    let (owner_session, path) = {
        let mut session = crate::lock_session(&state)?;
        crate::require_bound_store(&mut session, &project_id, &session_id)?;
        let owner_session = session
            .workspace
            .as_ref()
            .ok_or_else(unavailable)?
            .session_id;
        let owner_store = store_mut(&mut session)?;
        crate::workspace_template::load_template(owner_store)?;
        (
            owner_session,
            crate::workspace_roots::resolve(owner_store, &root_id, &private_root(&state)?)?,
        )
    };
    let prepared_id = picker.finish(Some(path))?;
    if prepared_id.is_some() {
        let mut prepared = crate::lock_prepared_project(&state)?;
        let candidate = prepared.as_mut().ok_or_else(unavailable)?;
        if candidate.workspace_session != Some(owner_session) {
            prepared.take();
            return Err(unavailable());
        }
        candidate.granted_root = Some(root_id);
    }
    Ok(prepared_id)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri owns deserialized command arguments.
pub(crate) fn workspace_root_remove(
    project_id: String,
    session_id: String,
    root_id: String,
    state: tauri::State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let _admission = crate::lock_application_admission(&state, "removing a workspace folder")?;
    let mut session = crate::lock_session(&state)?;
    crate::require_bound_store(&mut session, &project_id, &session_id)?;
    crate::workspace_roots::remove(store_mut(&mut session)?, &root_id, &private_root(&state)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProjectSnapshot, SessionPhase};

    fn close(state: &PluginState, opened: &ProjectSnapshot) {
        crate::close_project_with_wait(
            state,
            opened.project_id.clone(),
            opened.session_id.clone(),
            CommandId::new(),
            std::time::Duration::from_secs(1),
        )
        .expect("close active root");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One lifecycle journey proves the single lease survives failures.
    fn workspace_owner_survives_root_switch_and_failed_return_without_a_second_lease() {
        let owner_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        std::fs::write(
            owner_dir
                .path()
                .join(crate::workspace_template::TEMPLATE_PATH),
            "```loom-workspace\n[functions]\nformat='raw'\n```\n",
        )
        .unwrap();
        std::fs::write(
            other_dir
                .path()
                .join(crate::workspace_template::TEMPLATE_PATH),
            "```loom-workspace\n[functions]\nformat='model'\n```\n",
        )
        .unwrap();
        let state = PluginState::with_app_local_data_root(
            Some(private.path().to_owned()),
            true,
            crate::BuildModelPolicy::default(),
        );
        let initial_watch_session = std::cell::Cell::new(None);
        let opened = crate::reserve_project_choice(&state)
            .unwrap()
            .finish_with_document_filesystem_watcher(
                ProjectStore::open_folder(owner_dir.path()).map_err(IpcFailure::store),
                |_, session_id| {
                    initial_watch_session.set(Some(session_id));
                    Ok(None)
                },
            )
            .unwrap();
        let owner_session = crate::lock_session(&state)
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .session_id;
        assert_eq!(initial_watch_session.get(), Some(owner_session));
        assert_ne!(owner_session.to_string(), opened.session_id);
        let prepared = crate::prepare_project_folder(&state, Some(other_dir.path().to_owned()))
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        close(&state, &opened);
        assert!(
            ProjectStore::open_folder(owner_dir.path()).is_err(),
            "owner lease must remain held"
        );
        let choice = crate::reserve_project_choice(&state).unwrap();
        let other = choice
            .finish_without_document_filesystem_watcher(crate::take_prepared_project(
                &state, prepared,
            ))
            .unwrap();
        {
            let mut session = crate::lock_session(&state).unwrap();
            assert_eq!(
                session.workspace.as_ref().unwrap().session_id,
                owner_session
            );
            assert_eq!(
                store(&session).unwrap().manifest().project_id.to_string(),
                opened.project_id
            );
            let roots = crate::workspace_roots::list(
                store(&session).unwrap(),
                &private_root(&state).unwrap(),
            )
            .unwrap();
            assert_eq!(roots.len(), 2);
            assert!(roots.iter().all(|root| root.available));
            let recipe =
                crate::workspace_template::function_recipe(store_mut(&mut session).unwrap())
                    .unwrap();
            assert_eq!(
                recipe.format,
                crate::workspace_template::FunctionFormat::Raw
            );
            let child = session.store.as_mut().unwrap();
            assert!(
                recipe.local_configuration_artifact(child).is_none(),
                "foreign recipe artifacts must never be attributed to the active root"
            );
            let evidence = serde_json::to_vec(&recipe).unwrap();
            let retained = child.store_provenance_blob(&evidence).unwrap();
            assert_eq!(child.read_blob(retained).unwrap(), evidence);
            let value: serde_json::Value = serde_json::from_slice(&evidence).unwrap();
            assert_eq!(value["origin"]["project_id"], opened.project_id);
            assert!(
                value["configuration"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("format='raw'")
            );
        }
        assert!(
            ProjectStore::open_folder(owner_dir.path()).is_err(),
            "switch must not release owner"
        );
        let prepared = crate::prepare_project_folder(&state, Some(owner_dir.path().to_owned()))
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        close(&state, &other);
        let choice = crate::reserve_project_choice(&state).unwrap();
        let selected = crate::take_prepared_project(&state, prepared).unwrap();
        // A running pane can publish after the owner return is prepared but
        // before the new active document session has been committed.
        {
            let mut session = crate::lock_session_internal(&state).unwrap();
            require_store_mut(&mut session, &opened.project_id, &owner_session.to_string())
                .unwrap()
                .create_document_if_absent(
                    "During return.md",
                    loom_document::DocumentContent::Prose("Retained while returning.".into()),
                    "retained experiment",
                )
                .unwrap();
        }
        let error = choice
            .finish_with_document_filesystem_watcher(Ok(selected), |_, _| {
                Err(IpcFailure::new("watcher_failed", "fixture failure", false))
            })
            .unwrap_err();
        assert_eq!(error.code, "watcher_failed");
        {
            let session = crate::lock_session_internal(&state).unwrap();
            assert_eq!(session.phase, SessionPhase::Closed);
            assert_eq!(
                store(&session).unwrap().manifest().project_id.to_string(),
                opened.project_id
            );
            assert_eq!(
                store(&session)
                    .unwrap()
                    .read_document("During return.md")
                    .unwrap()
                    .text,
                "Retained while returning."
            );
        }
        let reopened = crate::reserve_project_choice(&state)
            .unwrap()
            .finish_without_document_filesystem_watcher(Ok(crate::ProjectChoiceStore::Workspace))
            .unwrap();
        assert_eq!(reopened.project_id, opened.project_id);
        assert_ne!(reopened.session_id, opened.session_id);
        assert_eq!(
            crate::lock_session(&state)
                .unwrap()
                .workspace
                .as_ref()
                .unwrap()
                .session_id,
            owner_session
        );
    }
}
