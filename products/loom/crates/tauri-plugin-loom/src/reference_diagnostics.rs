//! Read-only feedback from the same reference parser and resolver as inference.
use std::collections::HashMap;

use serde::Serialize;
use tauri::State;

use super::{
    IpcFailure, PluginState, lock_application_admission, lock_session, require_bound_store,
};
use crate::material_context::{self, Value};

#[derive(Debug, Serialize)]
pub(super) struct ReferenceDiagnostic {
    start: usize,
    end: usize,
    message: String,
}

fn diagnose(
    context: material_context::ReadContext<'_>,
    text: &str,
) -> Result<Vec<ReferenceDiagnostic>, IpcFailure> {
    let references = loom_document::document_references(text)
        .map_err(|error| IpcFailure::new("document_reference_invalid", error.to_string(), false))?;
    let mut checked = HashMap::new();
    let mut diagnostics = Vec::new();
    for reference in references {
        let message =
            checked.entry(reference.name.clone()).or_insert_with(
                || match context.resolve(&reference.name) {
                    Ok(value) if matches!(value.unscoped(), Value::Material { material } if !material.material.available) => {
                        Some(format!(
                            "Source @{} is unavailable. Open it to reconnect.",
                            reference.name
                        ))
                    }
                    Ok(_) => None,
                    Err(error) => Some(error.message),
                },
            );
        if let Some(message) = message {
            diagnostics.push(ReferenceDiagnostic {
                start: reference.range.start,
                end: reference.range.end,
                message: message.clone(),
            });
        }
    }
    Ok(diagnostics)
}

#[tauri::command]
pub(super) async fn document_reference_diagnostics(
    project_id: String,
    session_id: String,
    text: String,
    state: State<'_, PluginState>,
) -> Result<Vec<ReferenceDiagnostic>, IpcFailure> {
    let _admission = lock_application_admission(&state, "reference diagnostics")?;
    let mut session = lock_session(&state)?;
    require_bound_store(&mut session, &project_id, &session_id)?;
    let owner = session
        .workspace
        .as_ref()
        .ok_or_else(|| IpcFailure::new("workspace_not_open", "Open a workspace first.", false))?;
    let references = loom_document::document_references(&text)
        .map_err(|error| IpcFailure::new("document_reference_invalid", error.to_string(), false))?;
    let mut mounted = crate::workspace_references::Snapshots::default();
    mounted.admit(
        &state,
        &session,
        references.iter().map(|reference| reference.name.as_str()),
    )?;
    let context = crate::workspace_owner::read_context(
        &session,
        &project_id,
        &session_id,
        &owner.project_id.to_string(),
        &owner.session_id.to_string(),
    )?
    .with_mounted(&mounted);
    diagnose(context, &text)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn marks_exact_missing_and_ambiguous_references_but_not_quotes_or_code() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("a")).unwrap();
        std::fs::create_dir(root.path().join("b")).unwrap();
        std::fs::write(root.path().join("a/Notes.md"), "One").unwrap();
        std::fs::write(root.path().join("b/Notes.md"), "Two").unwrap();
        let store = loom_store::ProjectStore::open_folder(root.path()).unwrap();
        let text = "🌒 @Missing @Notes @\"a/Notes.md\"\n\n> @Quoted\n\n`@Code`\n\n@Missing";
        let diagnostics = diagnose((&store).into(), text).unwrap();
        let marked: Vec<_> = diagnostics
            .iter()
            .map(|item| &text[item.start..item.end])
            .collect();
        assert_eq!(marked, ["@Missing", "@Notes", "@Missing"]);
        assert_eq!(diagnostics[0].message, diagnostics[2].message);
        assert!(diagnostics[1].message.contains("ambiguous"));
        assert_eq!(
            std::fs::read(root.path().join("a/Notes.md")).unwrap(),
            b"One"
        );
    }
}
