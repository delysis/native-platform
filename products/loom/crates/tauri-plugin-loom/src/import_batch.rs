//! Explicit local file/folder grants. Entries are bounded and symlinks are
//! never followed; one bad file cannot hide the results of earlier files.
use super::{IpcFailure, PluginState};
use crate::context_attachments::{
    PreparedAttachment, StoredAttachment, prepare_path_bounded, prepare_provided,
};
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
    let root = operation.root.clone();
    let cancel = operation.stop_flag();
    let prepared = operation
        .compute(move || Ok(prepare_local_batch(&root, roots, &cancel)))
        .await?;
    Ok(publish_batch(&operation, &state, prepared))
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

struct PreparedBatch {
    prepared: Vec<PreparedAttachment>,
    failures: Vec<ImportFailure>,
}

fn publish_batch(
    operation: &ImportOperation,
    state: &PluginState,
    batch: PreparedBatch,
) -> ImportBatch {
    let mut report = ImportBatch {
        imported: Vec::new(),
        failures: batch.failures,
        next_page_token: None,
    };
    for prepared in batch.prepared {
        let name = prepared.attachment.file_name.clone();
        match operation.publish(state, prepared) {
            Ok(attachment) => report.imported.push(attachment),
            Err(error) => report.failures.push(ImportFailure {
                name,
                message: error.message,
            }),
        }
    }
    report
}

fn prepare_local_batch(
    project_root: &std::path::Path,
    roots: Vec<PathBuf>,
    cancel: &AtomicBool,
) -> PreparedBatch {
    let mut report = PreparedBatch {
        prepared: Vec::new(),
        failures: Vec::new(),
    };
    let mut pending = roots;
    pending.sort();
    pending.reverse();
    let mut seen = BTreeSet::new();
    let started = std::time::Instant::now();
    let mut entries = 0usize;
    let mut remaining = 128 * 1024 * 1024u64;
    while let Some(path) = pending.pop() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        entries += 1;
        if entries > 4096
            || report.prepared.len() + report.failures.len() >= 256
            || started.elapsed() >= std::time::Duration::from_mins(3)
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
                let mut children = Vec::new();
                for child in fs::read_dir(&path).map_err(|e| e.to_string())? {
                    let child = child.map_err(|e| e.to_string())?;
                    if child.file_name().to_string_lossy().starts_with('.') {
                        continue;
                    }
                    if children.len() + pending.len() >= 4096 {
                        return Err("Folder entry limit exceeded; select a smaller folder.".into());
                    }
                    children.push(child.path());
                }
                children.sort();
                children.reverse();
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
            let mut attachment =
                prepare_path_bounded(project_root, &path, granted).map_err(|e| e.to_string())?;
            // Full text is retained on disk and added only after selection.
            attachment.attachment.editable_markdown = None;
            attachment.attachment.media_markdown = None;
            Ok(Some(attachment))
        })();
        match result {
            Ok(Some(attachment)) if seen.insert(attachment.attachment.id.clone()) => {
                report.prepared.push(attachment);
            }
            Ok(_) => {}
            Err(message) => report.failures.push(ImportFailure { name, message }),
        }
    }
    report
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
    let root = operation.root.clone();
    let cancel = operation.stop_flag();
    let batch = operation
        .compute(move || {
            let mut batch = PreparedBatch {
                prepared: Vec::new(),
                failures: Vec::new(),
            };
            for (index, chunk) in chunks.into_iter().enumerate() {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let title = chunk
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("Pasted source")
                    .chars()
                    .take(80)
                    .collect::<String>();
                let name = format!("{} - {title}.txt", index + 1);
                match prepare_provided(
                    &root,
                    ProvidedAttachment::from_bytes(
                        &name,
                        Some("text/plain".into()),
                        chunk.as_bytes(),
                    ),
                ) {
                    Ok(mut prepared) => {
                        prepared.attachment.editable_markdown = None;
                        prepared.attachment.media_markdown = None;
                        batch.prepared.push(prepared);
                    }
                    Err(error) => batch.failures.push(ImportFailure {
                        name,
                        message: error.to_string(),
                    }),
                }
            }
            Ok(batch)
        })
        .await?;
    Ok(publish_batch(&operation, &state, batch))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_import_keeps_successes_and_is_content_idempotent() {
        let project = tempfile::tempdir().expect("project");
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("a.txt"), "A source document.").expect("text");
        fs::write(source.path().join("duplicate.txt"), "A source document.").expect("duplicate");
        fs::write(source.path().join("empty.txt"), "").expect("empty");
        let report = prepare_local_batch(
            project.path(),
            vec![source.path().to_path_buf()],
            &AtomicBool::new(false),
        );
        assert_eq!(report.prepared.len(), 1);
        assert_eq!(report.failures.len(), 1);
        let id = report
            .prepared
            .into_iter()
            .next()
            .unwrap()
            .publish()
            .unwrap()
            .id;
        let again = prepare_local_batch(
            project.path(),
            vec![source.path().to_path_buf()],
            &AtomicBool::new(false),
        );
        assert_eq!(id, again.prepared[0].attachment.id);
    }
    #[cfg(unix)]
    #[test]
    fn folder_import_never_follows_a_symlink() {
        let project = tempfile::tempdir().expect("project");
        let source = tempfile::tempdir().expect("source");
        std::os::unix::fs::symlink(source.path(), source.path().join("cycle")).expect("symlink");
        let report = prepare_local_batch(
            project.path(),
            vec![source.path().to_path_buf()],
            &AtomicBool::new(false),
        );
        assert!(report.prepared.is_empty());
        assert_eq!(report.failures.len(), 1);
    }
}
