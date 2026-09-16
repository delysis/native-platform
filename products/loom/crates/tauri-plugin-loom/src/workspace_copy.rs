//! A drop onto a visible folder is a copy, never an overwrite or context edit.
use std::path::{Path, PathBuf};

use attachment_native_host::ProvidedAttachment;
use loom_store::PreparedWorkspaceCopy;
use serde::Serialize;
use tauri::State;

use crate::context_attachments::{PreparedAttachment, prepare_provided};
use crate::import_batch::ImportFailure;
use crate::import_jobs::ImportOperation;
use crate::{IpcFailure, PluginState, materials};

#[derive(Debug, Serialize)]
pub(crate) struct CopyReport {
    copied: Vec<String>,
    materials: Vec<materials::MaterialEntry>,
    failures: Vec<ImportFailure>,
}

struct PreparedCopy {
    file: PreparedWorkspaceCopy,
    attachment: Option<PreparedAttachment>,
}

fn prepare(
    root: &Path,
    source: &Path,
    destination: &str,
    limit: u64,
) -> Result<PreparedCopy, IpcFailure> {
    let name = source
        .file_name()
        .ok_or_else(|| failure("The source has no filename."))?;
    let path = Path::new(destination).join(name);
    let file = PreparedWorkspaceCopy::read(source, &path, limit).map_err(IpcFailure::store)?;
    let attachment = if file.is_writing() {
        None
    } else {
        Some(
            prepare_provided(
                root,
                ProvidedAttachment::from_bytes(name.to_string_lossy(), None, file.bytes().to_vec()),
            )
            .map_err(|error| IpcFailure::context_attachment(&error))?,
        )
    };
    Ok(PreparedCopy { file, attachment })
}

struct PublishedCopy {
    path: String,
    material: Option<materials::MaterialEntry>,
    warning: Option<String>,
}

fn publish(
    prepared: PreparedCopy,
    store: &mut loom_store::ProjectStore,
) -> Result<PublishedCopy, IpcFailure> {
    let copied = prepared.file.publish(store).map_err(IpcFailure::store)?;
    let path = copied.relative_path;
    let material = prepared
        .attachment
        .map(|attachment| {
            let attachment = attachment
                .publish()
                .map_err(|error| IpcFailure::context_attachment(&error))?;
            materials::bind_workspace_attachment(store, &attachment.id, &path)
                .map_err(|error| failure(error.to_string()))
        })
        .transpose();
    let (material, warning) = match material {
        Ok(material) => (material, copied.registration_warning),
        Err(error) => (
            None,
            Some(format!(
                "The file was copied to {path}, but source preparation could not be published: {}",
                error.message
            )),
        ),
    };
    Ok(PublishedCopy {
        path,
        material,
        warning,
    })
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("workspace_copy_failed", message, false)
}

#[tauri::command]
pub(crate) async fn workspace_copy_files(
    project_id: String,
    session_id: String,
    operation_id: String,
    destination: String,
    paths: Vec<String>,
    state: State<'_, PluginState>,
) -> Result<CopyReport, IpcFailure> {
    if paths.len() > 16 || destination.len() > 4096 {
        return Err(failure("Copy at most 16 files into one workspace folder."));
    }
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    let mut report = CopyReport {
        copied: Vec::new(),
        materials: Vec::new(),
        failures: Vec::new(),
    };
    let mut remaining = 128 * 1024 * 1024u64;
    for path in paths {
        if let Err(error) = operation.check() {
            report.failures.push(ImportFailure {
                name: "Remaining files".into(),
                message: error.message,
            });
            break;
        }
        let source = PathBuf::from(&path);
        let grant = match std::fs::symlink_metadata(&source) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= remaining =>
            {
                metadata.len()
            }
            Ok(_) => {
                report.failures.push(ImportFailure {
                    name: path,
                    message: "Copy requires ordinary files within the 128 MB batch limit.".into(),
                });
                continue;
            }
            Err(error) => {
                report.failures.push(ImportFailure {
                    name: path,
                    message: error.to_string(),
                });
                continue;
            }
        };
        remaining -= grant; // Failed conversion also consumes its read budget.
        let root = operation.root.clone();
        let destination = destination.clone();
        let prepared = operation
            .compute(move || prepare(&root, &source, &destination, grant))
            .await;
        let result = prepared.and_then(|prepared| {
            operation.publish_to_store(&state, |store| publish(prepared, store))
        });
        match result {
            Ok(copied) => {
                if let Some(message) = copied.warning {
                    report.failures.push(ImportFailure {
                        name: copied.path.clone(),
                        message,
                    });
                }
                report.copied.push(copied.path);
                report.materials.extend(copied.material);
            }
            Err(error) => report.failures.push(ImportFailure {
                name: path,
                message: error.message,
            }),
        }
    }
    Ok(report)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn copied_media_keeps_its_physical_path_and_stop_prevents_late_copy() {
        let root = tempfile::tempdir().unwrap();
        let state = PluginState::default();
        let store = crate::initialize_project(root.path(), "Copies".into()).unwrap();
        let snapshot = crate::reserve_project_choice(&state)
            .unwrap()
            .finish_without_document_filesystem_watcher(Ok(store))
            .unwrap();
        let command = crate::CommandId::new().to_string();
        let operation =
            ImportOperation::reserve(&state, &snapshot.project_id, &snapshot.session_id, &command)
                .unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native.png");
        let original = std::fs::read(&source).unwrap();
        let prepared = prepare(root.path(), &source, "Pictures", 1024 * 1024).unwrap();
        let copied = operation
            .publish_to_store(&state, |store| publish(prepared, store))
            .unwrap();
        assert_eq!(copied.path, "Pictures/native.png");
        assert!(copied.warning.is_none());
        assert_eq!(
            copied.material.unwrap().workspace_path.as_deref(),
            Some("Pictures/native.png")
        );
        assert_eq!(
            std::fs::read(root.path().join("Pictures/native.png")).unwrap(),
            original
        );
        let late = prepare(root.path(), &source, "Later", 1024 * 1024).unwrap();
        state
            .imports
            .cancel(&snapshot.session_id, &command)
            .unwrap();
        assert!(
            operation
                .publish_to_store(&state, |store| publish(late, store))
                .is_err()
        );
        assert!(!root.path().join("Later/native.png").exists());
        assert_eq!(std::fs::read(source).unwrap(), original);
        drop(operation);
        assert_eq!(state.imports.shutdown().unwrap(), 0);
    }
}
