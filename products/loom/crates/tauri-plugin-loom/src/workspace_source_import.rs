//! Explicit named-source imports retain originals in the workspace owner.
//! They never insert into a document or borrow the active folder's configuration.
use crate::context_attachments::{
    PreparedAttachment, StoredAttachment, prepare_path_bounded, prepare_provided,
    record_import_origin,
};
use crate::import_batch::ImportFailure;
use crate::import_jobs::ImportOperation;
use crate::materials::{self, MaterialEntry};
use crate::{IpcFailure, PluginState, lock_application_admission, lock_session};
use attachment_native_host::ProvidedAttachment;
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt as _;

const MAX_FILES: usize = 16;
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const TIME_LIMIT: Duration = Duration::from_mins(3);

#[derive(Debug, Serialize)]
pub(crate) struct ImportedSource {
    pub attachment: StoredAttachment,
    /// None means the original was retained, but its named binding failed.
    pub material: Option<MaterialEntry>,
}

#[derive(Serialize)]
pub(crate) struct ImportReport {
    pub workspace_id: String,
    pub workspace_session_id: String,
    pub operation_id: String,
    pub imported: Vec<ImportedSource>,
    pub failures: Vec<ImportFailure>,
    pub cancelled: bool,
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("workspace_source_import_failed", message, false)
}

#[tauri::command]
pub(crate) async fn workspace_source_import_paths(
    project_id: String,
    session_id: String,
    operation_id: String,
    paths: Vec<String>,
    state: State<'_, PluginState>,
) -> Result<ImportReport, IpcFailure> {
    let operation =
        ImportOperation::reserve_workspace(&state, &project_id, &session_id, &operation_id)?;
    import_sources(
        &operation,
        &state,
        &operation_id,
        paths.into_iter().map(PathBuf::from).collect(),
        |_, _| {},
    )
    .await
}

#[tauri::command]
pub(crate) async fn workspace_source_import_choose<R: Runtime>(
    project_id: String,
    session_id: String,
    operation_id: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ImportReport, IpcFailure> {
    // Reserve before the picker: late selections retain their original owner.
    let operation =
        ImportOperation::reserve_workspace(&state, &project_id, &session_id, &operation_id)?;
    let paths = app
        .dialog()
        .file()
        .blocking_pick_files()
        .unwrap_or_default()
        .into_iter()
        .map(|selected| {
            selected
                .into_path()
                .map_err(|_| failure("Choose local source files."))
        })
        .collect::<Result<Vec<_>, _>>()?;
    import_sources(&operation, &state, &operation_id, paths, |_, _| {}).await
}

#[tauri::command]
pub(crate) async fn workspace_source_import_paste(
    project_id: String,
    session_id: String,
    operation_id: String,
    text: String,
    separator: String,
    state: State<'_, PluginState>,
) -> Result<ImportReport, IpcFailure> {
    let inputs = pasted_inputs(&text, &separator)?;
    let operation =
        ImportOperation::reserve_workspace(&state, &project_id, &session_id, &operation_id)?;
    import_inputs(&operation, &state, &operation_id, inputs, |_, _| {}).await
}

fn pasted_inputs(text: &str, separator: &str) -> Result<Vec<SourceInput>, IpcFailure> {
    if text.len() > 1024 * 1024 || separator.len() > 128 {
        return Err(failure(
            "Paste at most 1 MiB with a separator of at most 128 bytes.",
        ));
    }
    let chunks: Vec<&str> = if separator.is_empty() {
        vec![text]
    } else {
        text.split(separator).take(MAX_FILES + 1).collect()
    };
    if chunks.len() > MAX_FILES {
        return Err(failure("Split pasted sources into batches of at most 16."));
    }
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let title: String = text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Pasted source")
                .chars()
                .filter(|ch| !ch.is_control())
                .take(60)
                .collect();
            SourceInput::Text {
                name: format!("{} - {}.txt", index + 1, title.replace(['/', '\\'], "-")),
                text: text.to_owned(),
            }
        })
        .collect())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri owns deserialized command arguments.
pub(crate) fn workspace_source_import_cancel(
    project_id: String,
    session_id: String,
    operation_id: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let _admission = lock_application_admission(&state, "stopping a workspace import")?;
    let mut session = lock_session(&state)?;
    crate::workspace_owner::require_store_mut(&mut session, &project_id, &session_id)?;
    state.imports.cancel(&session_id, &operation_id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Boundary {
    Prepared,
    Published,
}

fn prepare_file(
    root: &Path,
    path: &Path,
    remaining: u64,
) -> Result<(u64, Result<PreparedAttachment, IpcFailure>), IpcFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|error| failure(error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(failure(
            "Choose an ordinary file. Folders belong in the workspace folder list.",
        ));
    }
    if metadata.len() > remaining {
        return Err(failure("The import reached its 128 MiB source-byte limit."));
    }
    let charged = metadata.len();
    let prepared = prepare_path_bounded(root, path, charged)
        .map_err(|error| IpcFailure::context_attachment(&error));
    Ok((charged, prepared))
}

enum SourceInput {
    File(PathBuf),
    Text { name: String, text: String },
}
impl SourceInput {
    fn name(&self) -> String {
        match self {
            Self::File(path) => path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            Self::Text { name, .. } => name.clone(),
        }
    }
    fn prepare(
        self,
        root: &Path,
        remaining: u64,
    ) -> Result<(u64, Result<PreparedAttachment, IpcFailure>), IpcFailure> {
        match self {
            Self::File(path) => prepare_file(root, &path, remaining),
            Self::Text { name, text } => {
                let charged =
                    u64::try_from(text.len()).map_err(|error| failure(error.to_string()))?;
                if charged > remaining {
                    return Err(failure("The import reached its source-byte limit."));
                }
                let prepared = prepare_provided(root, ProvidedAttachment::from_bytes(&name, Some("text/plain".into()), text.into_bytes())).map_err(|error| IpcFailure::context_attachment(&error)).and_then(|prepared| {
                    record_import_origin(root, &serde_json::json!({ "schema":"loom.paste-import.v1", "source_sha256":prepared.attachment.id, "source_bytes":charged, "network_used":false, "human_reviewed":false })).map_err(|error| IpcFailure::context_attachment(&error))?;
                    Ok(prepared)
                });
                Ok((charged, prepared))
            }
        }
    }
}

pub(super) fn publish(
    operation: &ImportOperation,
    state: &PluginState,
    prepared: PreparedAttachment,
) -> Result<(ImportedSource, Option<String>), IpcFailure> {
    operation.publish_to_store(state, |store| {
        let mut attachment = prepared
            .publish()
            .map_err(|error| IpcFailure::context_attachment(&error))?;
        // Named sources expose explicit references, not document-local insertion links.
        attachment.inline_markdown.clear();
        attachment.editable_markdown = None;
        attachment.media_markdown = None;
        match materials::bind_attachment(store, &attachment.id, None) {
            Ok(material) => Ok((
                ImportedSource {
                    attachment,
                    material: Some(material),
                },
                None,
            )),
            Err(error) => Ok((
                ImportedSource {
                    attachment,
                    material: None,
                },
                Some(format!(
                    "The original was retained, but its name could not be added: {error}"
                )),
            )),
        }
    })
}

/// The callback exposes the actual preparation/publication boundaries to tests;
/// production callers pass a no-op. No callback owns conversion or publication.
async fn import_sources(
    operation: &ImportOperation,
    state: &PluginState,
    operation_id: &str,
    paths: Vec<PathBuf>,
    boundary: impl FnMut(Boundary, usize),
) -> Result<ImportReport, IpcFailure> {
    if paths.len() > MAX_FILES
        || paths
            .iter()
            .any(|path| !path.is_absolute() || path.as_os_str().len() > 4096)
    {
        return Err(failure(
            "Choose at most 16 local source files with absolute paths within 4 KiB.",
        ));
    }
    import_inputs(
        operation,
        state,
        operation_id,
        paths.into_iter().map(SourceInput::File).collect(),
        boundary,
    )
    .await
}

async fn import_inputs(
    operation: &ImportOperation,
    state: &PluginState,
    operation_id: &str,
    inputs: Vec<SourceInput>,
    mut boundary: impl FnMut(Boundary, usize),
) -> Result<ImportReport, IpcFailure> {
    let mut report = ImportReport {
        workspace_id: operation.project_id().into(),
        workspace_session_id: operation.session_id().into(),
        operation_id: operation_id.into(),
        imported: Vec::new(),
        failures: Vec::new(),
        cancelled: false,
    };
    let started = Instant::now();
    let mut remaining = MAX_BYTES;
    let count = inputs.len();
    for (index, input) in inputs.into_iter().enumerate() {
        let stopped = operation.check().err();
        if stopped.is_some() || started.elapsed() >= TIME_LIMIT {
            report.cancelled = stopped.is_some();
            report.failures.push(ImportFailure {
                name: "Remaining sources".into(),
                message: format!(
                    "{} {} selected files were not attempted; retained sources remain available.",
                    stopped.map_or(
                        "The import reached its three-minute work limit.".into(),
                        |error| error.message
                    ),
                    count - index
                ),
            });
            break;
        }
        let root = operation.root.clone();
        let name = input.name();
        let attempt = operation
            .compute(move || input.prepare(&root, remaining))
            .await;
        let prepared = match attempt {
            Ok((charged, result)) => {
                remaining = remaining.saturating_sub(charged);
                result
            }
            Err(error) => Err(error),
        };
        let result = prepared.and_then(|prepared| {
            boundary(Boundary::Prepared, index);
            publish(operation, state, prepared)
        });
        match result {
            Ok((source, binding_error)) => {
                report.imported.push(source);
                if let Some(message) = binding_error {
                    report.failures.push(ImportFailure { name, message });
                }
                boundary(Boundary::Published, index);
            }
            Err(error) => report.failures.push(ImportFailure {
                name,
                message: error.message,
            }),
        }
    }
    report.cancelled |= operation.check().is_err();
    Ok(report)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{CommandId, ProjectStore, SessionPhase};
    use std::sync::Arc;

    fn opened(path: &Path) -> (Arc<PluginState>, String, String) {
        let state = Arc::new(PluginState::default());
        let store = crate::initialize_project(path, "Owner".into()).unwrap();
        crate::reserve_project_choice(&state)
            .unwrap()
            .finish_without_document_filesystem_watcher(Ok(store))
            .unwrap();
        let ids = {
            let session = lock_session(&state).unwrap();
            let owner = session.workspace.as_ref().unwrap();
            (owner.project_id.to_string(), owner.session_id.to_string())
        };
        (state, ids.0, ids.1)
    }
    fn sources() -> (tempfile::TempDir, Vec<PathBuf>) {
        let directory = tempfile::tempdir().unwrap();
        let paths = ["first.txt", "second.txt"]
            .into_iter()
            .map(|name| {
                let path = directory.path().join(name);
                fs::write(&path, format!("Original {name}\n")).unwrap();
                fs::canonicalize(path).unwrap()
            })
            .collect();
        (directory, paths)
    }
    fn owner_materials(state: &PluginState) -> Vec<MaterialEntry> {
        let session = lock_session(state).unwrap();
        materials::list(crate::workspace_owner::store(&session).unwrap()).unwrap()
    }

    #[test]
    fn root_switch_between_preparation_and_publication_keeps_source_and_binding_in_owner() {
        let owner_root = tempfile::tempdir().unwrap();
        let mounted_root = tempfile::tempdir().unwrap();
        let (state, project, session_id) = opened(owner_root.path());
        let (_sources, paths) = sources();
        let originals: Vec<_> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
        let id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve_workspace(&state, &project, &session_id, &id).unwrap();
        let report = tauri::async_runtime::block_on(import_sources(
            &operation,
            &state,
            &id,
            paths.clone(),
            |stage, index| {
                if stage == Boundary::Prepared && index == 0 {
                    let (mounted, _) =
                        ProjectStore::initialize(mounted_root.path(), "Mounted").unwrap();
                    let mut session = lock_session(&state).unwrap();
                    crate::workspace_owner::park_active(&mut session);
                    session.store = Some(mounted);
                    session.active_session_id = Some(CommandId::new());
                    session.phase = SessionPhase::Open;
                }
            },
        ))
        .unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.imported.len(), 2);
        assert_eq!(owner_materials(&state).len(), 2);
        assert!(!mounted_root.path().join(".loom.md").exists());
        assert!(
            !mounted_root
                .path()
                .join(".loom/attachments/manifests")
                .exists()
        );
        for (index, source) in report.imported.iter().enumerate() {
            assert!(source.material.is_some());
            assert_eq!(fs::read(&paths[index]).unwrap(), originals[index]);
            let retained =
                crate::context_attachments::original_path(owner_root.path(), &source.attachment.id)
                    .unwrap();
            assert_eq!(fs::read(retained).unwrap(), originals[index]);
            assert!(source.attachment.inline_markdown.is_empty());
        }
    }

    #[test]
    fn stop_after_one_publication_retains_that_source_and_never_publishes_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let (state, project, session_id) = opened(root.path());
        let (_sources, paths) = sources();
        let id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve_workspace(&state, &project, &session_id, &id).unwrap();
        let report = tauri::async_runtime::block_on(import_sources(
            &operation,
            &state,
            &id,
            paths,
            |stage, index| {
                if stage == Boundary::Published && index == 0 {
                    state.imports.cancel(&session_id, &id).unwrap();
                }
            },
        ))
        .unwrap();
        assert!(report.cancelled);
        assert_eq!(report.imported.len(), 1);
        assert_eq!(owner_materials(&state).len(), 1);
    }

    #[test]
    fn stop_after_preparation_prevents_source_and_material_publication() {
        let root = tempfile::tempdir().unwrap();
        let (state, project, session_id) = opened(root.path());
        let (_sources, paths) = sources();
        let id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve_workspace(&state, &project, &session_id, &id).unwrap();
        let report = tauri::async_runtime::block_on(import_sources(
            &operation,
            &state,
            &id,
            vec![paths[0].clone()],
            |stage, _| {
                if stage == Boundary::Prepared {
                    state.imports.cancel(&session_id, &id).unwrap();
                }
            },
        ))
        .unwrap();
        assert!(report.cancelled);
        assert!(report.imported.is_empty());
        assert!(owner_materials(&state).is_empty());
        assert!(!root.path().join(".loom.md").exists());
    }

    #[test]
    fn pasted_chunks_retain_exact_bytes_and_respect_batch_limits() {
        let root = tempfile::tempdir().unwrap();
        let (state, project, session_id) = opened(root.path());
        let text = "  First\r\n---\nSecond\t\u{263e}\n";
        let chunks: Vec<_> = text.split("---\n").map(str::as_bytes).collect();
        let id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve_workspace(&state, &project, &session_id, &id).unwrap();
        let report = tauri::async_runtime::block_on(import_inputs(
            &operation,
            &state,
            &id,
            pasted_inputs(text, "---\n").unwrap(),
            |_, _| {},
        ))
        .unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.imported.len(), chunks.len());
        for (source, bytes) in report.imported.iter().zip(chunks) {
            assert!(source.material.is_some());
            assert_eq!(
                fs::read(
                    crate::context_attachments::original_path(root.path(), &source.attachment.id)
                        .unwrap()
                )
                .unwrap(),
                bytes
            );
        }
        assert!(pasted_inputs(&"x".repeat(1024 * 1024 + 1), "").is_err());
        assert!(pasted_inputs("x", &"|".repeat(129)).is_err());
        assert!(pasted_inputs(&vec!["x"; 17].join("|"), "|").is_err());
    }

    #[test]
    fn opaque_files_are_retained_and_symlinks_are_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let (state, project, session_id) = opened(root.path());
        let sources = tempfile::tempdir().unwrap();
        let path = sources.path().join("opaque.payload");
        let bytes = [0u8, 255, 0, 17, 4, 0, 128, 3];
        fs::write(&path, bytes).unwrap();
        let linked = sources.path().join("linked.payload");
        std::os::unix::fs::symlink(&path, &linked).unwrap();
        let id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve_workspace(&state, &project, &session_id, &id).unwrap();
        let report = tauri::async_runtime::block_on(import_sources(
            &operation,
            &state,
            &id,
            vec![path, linked],
            |_, _| {},
        ))
        .unwrap();
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.failures.len(), 1);
        assert!(report.imported[0].material.is_some());
        assert_eq!(
            fs::read(
                crate::context_attachments::original_path(
                    root.path(),
                    &report.imported[0].attachment.id
                )
                .unwrap()
            )
            .unwrap(),
            bytes
        );
    }

    #[test]
    fn failed_binding_reports_retained_original_without_overwriting_pending_configuration() {
        let root = tempfile::tempdir().unwrap();
        let (state, project, session_id) = opened(root.path());
        {
            let mut session = lock_session(&state).unwrap();
            let owner = crate::workspace_owner::store_mut(&mut session).unwrap();
            crate::workspace_template::enable(owner).unwrap();
            let loaded = owner.read_document(".loom.md").unwrap();
            owner
                .upsert_transient_draft(
                    ".loom.md",
                    loaded.revision_id,
                    0,
                    loom_document::DocumentContent::Prose("Unsaved settings".into()),
                )
                .unwrap();
        }
        let (_sources, paths) = sources();
        let original = fs::read(&paths[0]).unwrap();
        let id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve_workspace(&state, &project, &session_id, &id).unwrap();
        let report = tauri::async_runtime::block_on(import_sources(
            &operation,
            &state,
            &id,
            vec![paths[0].clone()],
            |_, _| {},
        ))
        .unwrap();
        assert_eq!(report.imported.len(), 1);
        assert!(report.imported[0].material.is_none());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(
            fs::read(
                crate::context_attachments::original_path(
                    root.path(),
                    &report.imported[0].attachment.id
                )
                .unwrap()
            )
            .unwrap(),
            original
        );
        let session = lock_session(&state).unwrap();
        assert_eq!(
            crate::workspace_owner::store(&session)
                .unwrap()
                .load_transient_draft(".loom.md")
                .unwrap()
                .unwrap()
                .text,
            "Unsaved settings"
        );
    }
}
