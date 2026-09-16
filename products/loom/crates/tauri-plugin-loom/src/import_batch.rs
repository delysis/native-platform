//! Explicit local file/folder grants. Entries are bounded and symlinks are
//! never followed; one bad file cannot hide the results of earlier files.
use super::{IpcFailure, PluginState};
use crate::context_attachments::{StoredAttachment, prepare_path_bounded, prepare_provided};
use crate::import_jobs::ImportOperation;
use attachment_native_host::ProvidedAttachment;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt as _;

#[derive(Debug, Serialize)]
pub(crate) struct ImportFailure {
    pub(crate) name: String,
    pub(crate) message: String,
}
#[derive(Debug, Serialize)]
pub(crate) struct ImportBatch {
    pub(crate) imported: Vec<StoredAttachment>,
    pub(crate) failures: Vec<ImportFailure>,
    pub(crate) next_page_token: Option<String>,
}

#[tauri::command]
pub(crate) async fn attachment_import_batch_choose<R: Runtime>(
    project_id: String,
    session_id: String,
    operation_id: String,
    folder: bool,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ImportBatch, IpcFailure> {
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    let selected = if folder {
        app.dialog()
            .file()
            .blocking_pick_folder()
            .into_iter()
            .collect()
    } else {
        app.dialog()
            .file()
            .blocking_pick_files()
            .unwrap_or_default()
    };
    let roots: Vec<PathBuf> = selected
        .into_iter()
        .map(|path| {
            path.into_path().map_err(|_| {
                IpcFailure::new(
                    "import_path_invalid",
                    "The selected source is not a local path.",
                    false,
                )
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(import_local_batch(&operation, &state, roots, || {}).await)
}

/// A direct editor drop returns every completed item, even if a later file fails.
/// Conversion stays on registered workers; each publication revalidates authority.
pub(super) async fn import_paths(
    operation: &ImportOperation,
    state: &PluginState,
    paths: Vec<String>,
) -> Result<ImportBatch, IpcFailure> {
    if paths.len() > 16 {
        return Err(IpcFailure::new(
            "attachment_transfer_limit",
            "Attach at most 16 files at once.",
            false,
        ));
    }
    let mut report = ImportBatch {
        imported: Vec::new(),
        failures: Vec::new(),
        next_page_token: None,
    };
    for path in paths {
        let name = path.clone();
        let root = operation.root.clone();
        let result = operation
            .compute(move || {
                prepare_path_bounded(&root, std::path::Path::new(&path), 128 * 1024 * 1024)
                    .map_err(|error| IpcFailure::context_attachment(&error))
            })
            .await
            .and_then(|prepared| operation.publish(state, prepared));
        match result {
            Ok(item) => report.imported.push(item),
            Err(error) => report.failures.push(ImportFailure {
                name,
                message: error.message,
            }),
        }
    }
    Ok(report)
}

struct LocalSource {
    path: PathBuf,
    byte_limit: u64,
}

struct LocalBatch {
    sources: Vec<LocalSource>,
    failures: Vec<ImportFailure>,
}

const LOCAL_BATCH_TIME_LIMIT: std::time::Duration = std::time::Duration::from_mins(3);

/// Publication follows each successful conversion. The callback is a narrow
/// lifecycle boundary used to exercise Stop deterministically in tests.
async fn import_local_batch(
    operation: &ImportOperation,
    state: &PluginState,
    roots: Vec<PathBuf>,
    mut after_publication: impl FnMut(),
) -> ImportBatch {
    let started = std::time::Instant::now();
    let cancel = operation.stop_flag();
    let discovered = operation
        .compute(move || Ok(discover_local_batch(roots, &cancel, started)))
        .await;
    let mut report = ImportBatch {
        imported: Vec::new(),
        failures: Vec::new(),
        next_page_token: None,
    };
    let batch = match discovered {
        Ok(batch) => batch,
        Err(error) => {
            report.failures.push(ImportFailure {
                name: "Remaining sources".into(),
                message: format!(
                    "Folder discovery did not complete: {} No sources were prepared.",
                    error.message
                ),
            });
            return report;
        }
    };
    report.failures = batch.failures;
    let count = batch.sources.len();
    let mut seen = BTreeSet::new();
    for (index, source) in batch.sources.into_iter().enumerate() {
        if let Err(error) = operation.check() {
            report_remaining(&mut report, count - index, &error.message);
            break;
        }
        if started.elapsed() >= LOCAL_BATCH_TIME_LIMIT {
            report_remaining(
                &mut report,
                count - index,
                "The import reached its three-minute work limit.",
            );
            break;
        }
        let root = operation.root.clone();
        let name = source.path.to_string_lossy().into_owned();
        let result = operation
            .compute(move || {
                let mut prepared = prepare_path_bounded(&root, &source.path, source.byte_limit)
                    .map_err(|error| IpcFailure::context_attachment(&error))?;
                // Importing creates available sources, never manuscript insertions.
                prepared.attachment.editable_markdown = None;
                prepared.attachment.media_markdown = None;
                Ok(prepared)
            })
            .await
            .and_then(|prepared| operation.publish(state, prepared));
        match result {
            Ok(attachment) => {
                if seen.insert(attachment.id.clone()) {
                    report.imported.push(attachment);
                }
                after_publication();
            }
            Err(error) => report.failures.push(ImportFailure {
                name,
                message: error.message,
            }),
        }
        if let Err(error) = operation.check() {
            report_remaining(&mut report, count - index - 1, &error.message);
            break;
        }
    }
    report
}

fn report_remaining(report: &mut ImportBatch, remaining: usize, reason: &str) {
    report.failures.push(ImportFailure {
        name: "Remaining sources".into(),
        message: format!("{reason} {remaining} discovered sources were not attempted. {} completed sources remain available.", report.imported.len()),
    });
}

fn discover_local_batch(
    roots: Vec<PathBuf>,
    cancel: &AtomicBool,
    started: std::time::Instant,
) -> LocalBatch {
    let mut report = LocalBatch {
        sources: Vec::new(),
        failures: Vec::new(),
    };
    let mut pending = roots;
    pending.sort();
    pending.reverse();
    let mut entries = 0usize;
    let mut remaining = 128 * 1024 * 1024u64;
    let mut hidden = 0usize;
    while let Some(path) = pending.pop() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        entries += 1;
        if entries > 4096
            || report.sources.len() + report.failures.len() >= 256
            || started.elapsed() >= LOCAL_BATCH_TIME_LIMIT
        {
            report.failures.push(ImportFailure { name: "Remaining sources".into(), message: "The batch reached its file, entry, or three-minute work limit. Select a smaller folder to import the remainder.".into() });
            break;
        }
        let name = path.file_name().map_or_else(
            || "Source".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        let result = (|| {
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("Symbolic links are not followed.".into());
            }
            if metadata.is_dir() {
                let children =
                    discover_children(&path, pending.len(), &mut hidden, cancel, started)?;
                pending.extend(children);
                return Ok(None);
            }
            if !metadata.is_file() {
                return Err("The source is not an ordinary file.".into());
            }
            if metadata.len() > remaining {
                return Err("The batch reached its 128 MB source-byte limit.".into());
            }
            let granted = metadata.len();
            remaining -= granted; // Failed parser attempts consume the grant too.
            Ok(Some(LocalSource {
                path,
                byte_limit: granted,
            }))
        })();
        match result {
            Ok(Some(source)) => report.sources.push(source),
            Ok(_) => {}
            Err(message) => report.failures.push(ImportFailure { name, message }),
        }
    }
    if hidden > 0 {
        report.failures.push(ImportFailure {
            name: "Hidden entries".into(),
            message: format!(
                "Skipped {hidden} hidden files or folders. Their contents were not imported."
            ),
        });
    }
    report
}

fn discover_children(
    path: &std::path::Path,
    pending: usize,
    hidden: &mut usize,
    cancel: &AtomicBool,
    started: std::time::Instant,
) -> Result<Vec<PathBuf>, String> {
    let mut children = Vec::new();
    for child in fs::read_dir(path).map_err(|error| error.to_string())? {
        if cancel.load(Ordering::Acquire) || started.elapsed() >= LOCAL_BATCH_TIME_LIMIT {
            return Err(
                "Folder discovery stopped before this folder was completely listed.".into(),
            );
        }
        let child = child.map_err(|error| error.to_string())?;
        if child.file_name().to_string_lossy().starts_with('.') {
            *hidden += 1;
            if *hidden >= 4096 {
                return Err(
                    "Hidden-entry limit exceeded; this folder was not fully listed.".into(),
                );
            }
            continue;
        }
        if children.len() + pending >= 4096 {
            return Err("Folder entry limit exceeded; select a smaller folder.".into());
        }
        children.push(child.path());
    }
    children.sort();
    children.reverse();
    Ok(children)
}

#[tauri::command]
pub(crate) async fn import_text_sources(
    project_id: String,
    session_id: String,
    operation_id: String,
    text: String,
    separator: String,
    state: State<'_, PluginState>,
) -> Result<ImportBatch, IpcFailure> {
    if text.len() > 1024 * 1024 || separator.len() > 128 {
        return Err(IpcFailure::new(
            "paste_import_limit",
            "Paste at most 1 MB with a separator of at most 128 bytes.",
            false,
        ));
    }
    let chunks: Vec<&str> = if separator.is_empty() {
        vec![text.as_str()]
    } else {
        text.split(&separator).take(17).collect()
    };
    if chunks.len() > 16 {
        return Err(IpcFailure::new(
            "paste_import_limit",
            "Split pasted sources into batches of at most 16.",
            false,
        ));
    }
    let chunks: Vec<String> = chunks.into_iter().map(str::to_owned).collect();
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    Ok(import_pasted_chunks(&operation, &state, chunks, || {}).await)
}

async fn import_pasted_chunks(
    operation: &ImportOperation,
    state: &PluginState,
    chunks: Vec<String>,
    mut after_publication: impl FnMut(),
) -> ImportBatch {
    let mut report = ImportBatch {
        imported: Vec::new(),
        failures: Vec::new(),
        next_page_token: None,
    };
    let count = chunks.len();
    for (index, chunk) in chunks.into_iter().enumerate() {
        if let Err(error) = operation.check() {
            report_remaining(&mut report, count - index, &error.message);
            break;
        }
        let root = operation.root.clone();
        let title = chunk
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Pasted source")
            .chars()
            .take(80)
            .collect::<String>();
        let name = format!("{} - {title}.txt", index + 1);
        let file_name = name.clone();
        let result = operation
            .compute(move || {
                let mut prepared = prepare_provided(
                    &root,
                    ProvidedAttachment::from_bytes(
                        &file_name,
                        Some("text/plain".into()),
                        chunk.as_bytes(),
                    ),
                )
                .map_err(|error| IpcFailure::context_attachment(&error))?;
                prepared.attachment.editable_markdown = None;
                prepared.attachment.media_markdown = None;
                Ok(prepared)
            })
            .await
            .and_then(|prepared| operation.publish(state, prepared));
        match result {
            Ok(attachment) => {
                report.imported.push(attachment);
                after_publication();
            }
            Err(error) => report.failures.push(ImportFailure {
                name,
                message: error.message,
            }),
        }
        if let Err(error) = operation.check() {
            report_remaining(&mut report, count - index - 1, &error.message);
            break;
        }
    }
    report
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{CommandId, initialize_project, reserve_project_choice};

    fn opened(root: &std::path::Path) -> (PluginState, String, String) {
        let state = PluginState::default();
        let store = initialize_project(root, "Folder imports".into()).unwrap();
        let snapshot = reserve_project_choice(&state)
            .unwrap()
            .finish_without_document_filesystem_watcher(Ok(store))
            .unwrap();
        (state, snapshot.project_id, snapshot.session_id)
    }

    fn published_count(root: &std::path::Path) -> usize {
        fs::read_dir(root.join(".loom/attachments/manifests"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().is_some_and(|extension| {
                    extension == "json"
                        && !entry.file_name().to_string_lossy().starts_with("source-")
                })
            })
            .count()
    }

    #[test]
    fn folder_import_keeps_successes_and_is_content_idempotent() {
        let project = tempfile::tempdir().expect("project");
        let source = tempfile::tempdir().expect("source");
        let (state, project_id, session) = opened(project.path());
        fs::write(source.path().join("a.txt"), "A source document.").expect("text");
        fs::write(source.path().join("duplicate.txt"), "A source document.").expect("duplicate");
        fs::write(source.path().join("empty.txt"), "").expect("empty");
        let operation =
            ImportOperation::reserve(&state, &project_id, &session, &CommandId::new().to_string())
                .unwrap();
        let report = tauri::async_runtime::block_on(import_local_batch(
            &operation,
            &state,
            vec![source.path().to_path_buf()],
            || {},
        ));
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.failures.len(), 1);
        let id = &report.imported[0].id;
        assert!(report.imported[0].editable_markdown.is_none());
        drop(operation);
        let operation =
            ImportOperation::reserve(&state, &project_id, &session, &CommandId::new().to_string())
                .unwrap();
        let again = tauri::async_runtime::block_on(import_local_batch(
            &operation,
            &state,
            vec![source.path().to_path_buf()],
            || {},
        ));
        assert_eq!(*id, again.imported[0].id);
        assert_eq!(published_count(project.path()), 1);
        assert_eq!(
            fs::read(source.path().join("a.txt")).unwrap(),
            b"A source document."
        );
    }

    #[test]
    fn stopping_folder_import_retains_completed_sources_and_reports_remainder() {
        let project = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let (state, project_id, session) = opened(project.path());
        for name in ["a", "b", "c"] {
            fs::write(
                source.path().join(format!("{name}.txt")),
                format!("Original {name}.\n"),
            )
            .unwrap();
        }
        let operation_id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve(&state, &project_id, &session, &operation_id).unwrap();
        let report = tauri::async_runtime::block_on(import_local_batch(
            &operation,
            &state,
            vec![source.path().to_path_buf()],
            || {
                state.imports.cancel(&session, &operation_id).unwrap();
            },
        ));
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].file_name, "a.txt");
        assert_eq!(published_count(project.path()), 1);
        assert_eq!(report.failures.len(), 1);
        assert!(
            report.failures[0]
                .message
                .contains("2 discovered sources were not attempted")
        );
        assert!(
            report.failures[0]
                .message
                .contains("1 completed sources remain available")
        );
        let retained =
            crate::context_attachments::original_path(project.path(), &report.imported[0].id)
                .unwrap();
        assert_eq!(fs::read(retained).unwrap(), b"Original a.\n");
        // Even an already prepared file cannot cross the latched Stop boundary.
        let late =
            prepare_path_bounded(project.path(), &source.path().join("b.txt"), 1024).unwrap();
        assert!(operation.publish(&state, late).is_err());
        assert_eq!(published_count(project.path()), 1);
        for name in ["a", "b", "c"] {
            assert_eq!(
                fs::read_to_string(source.path().join(format!("{name}.txt"))).unwrap(),
                format!("Original {name}.\n")
            );
        }
    }

    #[test]
    fn folder_import_never_follows_a_symlink() {
        let source = tempfile::tempdir().expect("source");
        std::os::unix::fs::symlink(source.path(), source.path().join("cycle")).expect("symlink");
        let report = discover_local_batch(
            vec![source.path().to_path_buf()],
            &AtomicBool::new(false),
            std::time::Instant::now(),
        );
        assert!(report.sources.is_empty());
        assert_eq!(report.failures.len(), 1);
    }

    #[test]
    fn folder_discovery_reports_skipped_hidden_sources() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("visible.txt"), "Visible source.").unwrap();
        fs::write(source.path().join(".private.txt"), "Hidden source.").unwrap();
        let report = discover_local_batch(
            vec![source.path().to_path_buf()],
            &AtomicBool::new(false),
            std::time::Instant::now(),
        );
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].message.contains("Skipped 1 hidden"));
    }

    #[test]
    fn stopping_paste_retains_exact_completed_chunk_without_publishing_later_chunks() {
        let project = tempfile::tempdir().unwrap();
        let (state, project_id, session) = opened(project.path());
        let operation_id = CommandId::new().to_string();
        let operation =
            ImportOperation::reserve(&state, &project_id, &session, &operation_id).unwrap();
        let first = "First source.\r\nUnchanged Unicode: 雨\n";
        let report = tauri::async_runtime::block_on(import_pasted_chunks(
            &operation,
            &state,
            vec![first.into(), "Second source.".into()],
            || {
                state.imports.cancel(&session, &operation_id).unwrap();
            },
        ));
        assert_eq!(report.imported.len(), 1);
        assert_eq!(published_count(project.path()), 1);
        assert_eq!(report.failures.len(), 1);
        assert!(
            report.failures[0]
                .message
                .contains("1 discovered sources were not attempted")
        );
        let original =
            crate::context_attachments::original_path(project.path(), &report.imported[0].id)
                .unwrap();
        assert_eq!(fs::read(original).unwrap(), first.as_bytes());
        assert!(report.imported[0].editable_markdown.is_none());
    }

    #[test]
    fn paste_keeps_occurrences_of_identical_chunks_under_one_content_identity() {
        let project = tempfile::tempdir().unwrap();
        let (state, project_id, session) = opened(project.path());
        let operation =
            ImportOperation::reserve(&state, &project_id, &session, &CommandId::new().to_string())
                .unwrap();
        let report = tauri::async_runtime::block_on(import_pasted_chunks(
            &operation,
            &state,
            vec!["Repeated source.".into(), "Repeated source.".into()],
            || {},
        ));
        assert!(report.failures.is_empty());
        assert_eq!(report.imported.len(), 2);
        assert_eq!(report.imported[0].id, report.imported[1].id);
        assert_eq!(published_count(project.path()), 1);
    }
}
