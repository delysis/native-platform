//! Import an external-only file edit after the renderer has flushed its local
//! editor. Distinct journaled writing always keeps the reconciliation boundary.

use loom_store::ProjectStore;
use tauri::State;

use super::{
    DocumentActionIdentity, DocumentSummary, IpcFailure, PluginState, lock_application_admission,
    lock_session, open_document_from, parse_document_action_identity,
    registered_document_action_path, require_bound_store, stale_document_action_failure,
};

fn import_for_store(
    store: &mut ProjectStore,
    identity: DocumentActionIdentity,
) -> Result<Option<DocumentSummary>, IpcFailure> {
    let path = registered_document_action_path(store, identity)?;
    let before = store
        .reconciliation_snapshot(&path)
        .map_err(IpcFailure::store)?;
    if before.active_revision_id != identity.revision || before.active_blob_id != identity.blob {
        return Err(stale_document_action_failure());
    }
    let imported = store
        .import_external_changes_if_uncontested(&path, "Read external file changes")
        .map_err(IpcFailure::store)?;
    if imported.is_none() && !before.visible_matches_active {
        return Ok(None);
    }
    let loaded = store.read_document(&path).map_err(IpcFailure::store)?;
    let registered = store
        .registered_document(identity.document)
        .map_err(IpcFailure::store)?
        .ok_or_else(stale_document_action_failure)?;
    Ok(Some(
        open_document_from(loaded, None, registered.display_title).summary,
    ))
}

#[tauri::command]
pub(super) async fn document_import_external(
    project_id: String,
    session_id: String,
    document_id: String,
    expected_revision_id: String,
    expected_blob_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<DocumentSummary>, IpcFailure> {
    let _admission = lock_application_admission(&state, "an external file refresh")?;
    let identity =
        parse_document_action_identity(&document_id, &expected_revision_id, &expected_blob_id)?;
    let mut session = lock_session(&state)?;
    import_for_store(
        require_bound_store(&mut session, &project_id, &session_id)?,
        identity,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_document::DocumentContent;

    #[test]
    fn imports_external_only_bytes_and_rejects_stale_identity_or_distinct_draft() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        store
            .create_document_if_absent(
                "Draft.md",
                DocumentContent::Prose("Base\n".into()),
                "writing",
            )
            .unwrap();
        let base = store.read_document("Draft.md").unwrap();
        let identity = DocumentActionIdentity {
            document: base.document_id,
            revision: base.revision_id,
            blob: base.blob_id,
        };
        std::fs::write(store.root().join("Draft.md"), "External\r\n").unwrap();
        let imported = import_for_store(&mut store, identity).unwrap().unwrap();
        assert_ne!(imported.revision_id, Some(identity.revision.to_string()));
        assert_eq!(
            store.read_document("Draft.md").unwrap().text,
            "External\r\n"
        );
        assert!(import_for_store(&mut store, identity).is_err());
        let base = store.read_document("Draft.md").unwrap();
        let identity = DocumentActionIdentity {
            document: base.document_id,
            revision: base.revision_id,
            blob: base.blob_id,
        };
        store
            .upsert_transient_draft(
                "Draft.md",
                base.revision_id,
                0,
                DocumentContent::Prose("Local unsaved writing".into()),
            )
            .unwrap();
        std::fs::write(store.root().join("Draft.md"), "Another external edit").unwrap();
        assert!(import_for_store(&mut store, identity).unwrap().is_none());
        assert_eq!(
            store
                .load_transient_draft("Draft.md")
                .unwrap()
                .unwrap()
                .text,
            "Local unsaved writing"
        );
        assert_eq!(
            store
                .reconciliation_snapshot("Draft.md")
                .unwrap()
                .active_revision_id,
            base.revision_id
        );
    }
}
