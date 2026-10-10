//! Retained pane output belongs to the workspace that admitted its run.
//! Reading it must not open a document or borrow the active manuscript session.
use super::*;

const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

fn resolve_document(
    store: &ProjectStore,
    reference: &str,
) -> Result<Option<OpenDocument>, IpcFailure> {
    if reference.ends_with('/') {
        return Err(IpcFailure::new(
            "workspace_document_ambiguous",
            "A pane must name one document.",
            false,
        ));
    }
    match document_bindings::resolve_document_id(store, reference) {
        Ok(document_id) => read_output(store, document_id).map(Some),
        Err(error) if error.code == "document_reference_missing" => Ok(None),
        Err(error) => Err(error),
    }
}

#[tauri::command]
pub(super) async fn workspace_document_resolve(
    project_id: String,
    session_id: String,
    reference: String,
    state: State<'_, PluginState>,
) -> Result<Option<OpenDocument>, IpcFailure> {
    let _admission = lock_application_admission(&state, "reading a workspace pane document")?;
    let mut session = lock_session(&state)?;
    let store = workspace_owner::require_store_mut(&mut session, &project_id, &session_id)?;
    resolve_document(store, &reference)
}

fn read_output(store: &ProjectStore, document_id: DocumentId) -> Result<OpenDocument, IpcFailure> {
    let summary = store
        .registered_document(document_id)
        .map_err(IpcFailure::store)?
        .ok_or_else(stale_document_action_failure)?;
    let loaded = store
        .read_document(&summary.relative_path)
        .map_err(IpcFailure::store)?;
    if loaded.document_id != document_id || loaded.text.len() > MAX_OUTPUT_BYTES {
        return Err(IpcFailure::new(
            "workspace_output_unavailable",
            "The retained output is unavailable or too large to display.",
            false,
        ));
    }
    Ok(open_document_from(loaded, None, summary.display_title))
}

#[tauri::command]
pub(super) async fn workspace_pane_output(
    project_id: String,
    session_id: String,
    run_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<OpenDocument>, IpcFailure> {
    let _admission = lock_application_admission(&state, "reading workspace output")?;
    let mut session = lock_session(&state)?;
    let store = workspace_owner::require_store_mut(&mut session, &project_id, &session_id)?;
    terminal::output_document(store, &run_id)?
        .map(|document_id| read_output(store, document_id))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_read_preserves_exact_bytes_without_modifying_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("writing"), "Writing").unwrap();
        let text = "First line.\r\n\r\nSecond line.  \r\n";
        store
            .create_document_if_absent("Reply.md", DocumentContent::Prose(text.into()), "reply")
            .unwrap();
        let loaded = store.read_document("Reply.md").unwrap();
        let result = read_output(&store, loaded.document_id).unwrap();
        assert_eq!(result.text, text);
        assert_eq!(result.visible_blob_id, loaded.blob_id.to_string());
        assert_eq!(
            result.summary.revision_id,
            Some(loaded.revision_id.to_string())
        );
        assert!(result.transient_draft.is_none());
        assert_eq!(
            resolve_document(&store, "Reply").unwrap().unwrap().text,
            text
        );
        assert!(resolve_document(&store, "Missing").unwrap().is_none());
        assert!(resolve_document(&store, "../Reply").unwrap().is_none());
        assert!(read_output(&store, DocumentId::new()).is_err());
        assert_eq!(
            std::fs::read(store.root().join("Reply.md")).unwrap(),
            text.as_bytes()
        );
        let long_text = "a".repeat(65_537);
        store
            .create_document_if_absent(
                "Long.md",
                DocumentContent::Prose(long_text.clone()),
                "writing",
            )
            .unwrap();
        assert_eq!(
            resolve_document(&store, "Long").unwrap().unwrap().text,
            long_text
        );
    }
}
