//! Navigation and inference select the same source under one session lock.
use crate::materials::{MaterialEntry, MaterialEvidence};
use crate::{
    IpcFailure, PluginState, Session, lock_application_admission, lock_session,
    require_bound_store, workspace_owner,
};

#[derive(Debug, serde::Serialize)]
pub(super) struct Reference {
    project_id: String,
    session_id: String,
    material: Option<MaterialEntry>,
    evidence: Option<MaterialEvidence>,
}

fn resolve(
    session: &mut Session,
    project_id: &str,
    session_id: &str,
    reference: &str,
) -> Result<Reference, IpcFailure> {
    require_bound_store(session, project_id, session_id)?;
    let owner = session
        .workspace
        .as_ref()
        .ok_or_else(|| IpcFailure::new("workspace_not_open", "Open a workspace first.", false))?;
    let owner_project = owner.project_id.to_string();
    let owner_session = owner.session_id.to_string();
    let context = workspace_owner::read_context(
        session,
        project_id,
        session_id,
        &owner_project,
        &owner_session,
    )?;
    let selected = context.reference(reference)?;
    let owner_selected = selected.store.root() == context.materials.root()
        && selected.store.manifest().project_id == context.materials.manifest().project_id;
    Ok(Reference {
        project_id: if owner_selected {
            owner_project
        } else {
            project_id.into()
        },
        session_id: if owner_selected {
            owner_session
        } else {
            session_id.into()
        },
        material: selected.material,
        evidence: selected.evidence,
    })
}

#[tauri::command]
pub(super) async fn material_resolve_reference(
    project_id: String,
    session_id: String,
    reference: String,
    state: tauri::State<'_, PluginState>,
) -> Result<Reference, IpcFailure> {
    let _admission = lock_application_admission(&state, "source reference")?;
    resolve(
        &mut *lock_session(&state)?,
        &project_id,
        &session_id,
        &reference,
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{CommandId, ProjectStore, SessionPhase, context_attachments, materials};

    #[test]
    fn reference_scope_is_stable_for_owner_and_distinct_for_document_attachments() {
        let root = tempfile::tempdir().unwrap();
        let (mut owner, _) = ProjectStore::initialize(root.path().join("Owner"), "Owner").unwrap();
        let source_path = root.path().join("source.txt");
        std::fs::write(&source_path, "An exact retained source.").unwrap();
        let imported = context_attachments::import_path(owner.root(), &source_path).unwrap();
        let material =
            materials::bind_attachment(&mut owner, &imported.id, Some("Source")).unwrap();
        let project_id = owner.manifest().project_id.to_string();
        let active_session = CommandId::new().to_string();
        let mut session = Session::default();
        workspace_owner::establish(&mut session, &owner);
        let owner_session = session.workspace.as_ref().unwrap().session_id.to_string();
        session.store = Some(owner);
        session.active_session_id = Some(active_session.parse().unwrap());
        session.phase = SessionPhase::Open;
        let selected = resolve(&mut session, &project_id, &active_session, &material.id).unwrap();
        assert_eq!(selected.session_id, owner_session);
        assert_eq!(selected.material.unwrap().id, material.id);
        workspace_owner::park_active(&mut session);
        let (mut child, _) = ProjectStore::initialize(root.path().join("Child"), "Child").unwrap();
        std::fs::write(&source_path, "A different inline source.").unwrap();
        let inline = context_attachments::import_path(child.root(), &source_path).unwrap();
        let inline = materials::bind_attachment(&mut child, &inline.id, Some("Inline")).unwrap();
        let child_project = child.manifest().project_id.to_string();
        let child_session = CommandId::new().to_string();
        session.store = Some(child);
        session.active_session_id = Some(child_session.parse().unwrap());
        assert_eq!(
            resolve(&mut session, &child_project, &child_session, &material.id)
                .unwrap()
                .session_id,
            owner_session
        );
        assert_eq!(
            resolve(&mut session, &child_project, &child_session, &inline.id)
                .unwrap()
                .session_id,
            child_session
        );
        assert!(resolve(&mut session, &child_project, &active_session, &inline.id).is_err());
        // Identical source IDs in two distinct stores are not two interchangeable origins.
        std::fs::write(&source_path, "An exact retained source.").unwrap();
        let child = session.store.as_mut().unwrap();
        let duplicate = context_attachments::import_path(child.root(), &source_path).unwrap();
        let duplicate =
            materials::bind_attachment(child, &duplicate.id, Some("Duplicate")).unwrap();
        assert_eq!(duplicate.id, material.id);
        assert_eq!(
            resolve(&mut session, &child_project, &child_session, &material.id)
                .unwrap_err()
                .code,
            "material_context_invalid"
        );
    }
}
