//! Explicit folder copies use retained directory capabilities. Discovery never
//! follows links, and later reads cannot escape through a replaced ancestor.
use super::{
    CopyReport, ImportFailure, ImportOperation, IpcFailure, PluginState, failure, prepare_file,
    publish,
};
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};
use loom_store::PreparedWorkspaceCopy;
use std::{
    ffi::OsString,
    io::Read as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt as _;

const MAX_ENTRIES: usize = 4096;
const MAX_BYTES: u64 = 128 * 1024 * 1024;

struct CopyFile {
    parent: Arc<Dir>,
    name: OsString,
    destination: PathBuf,
    byte_limit: u64,
}

struct CopyPlan {
    directories: Vec<PathBuf>,
    files: Vec<CopyFile>,
    failures: Vec<ImportFailure>,
}

fn io_failure(error: std::io::Error) -> IpcFailure {
    IpcFailure::store(loom_store::StoreError::Io(error))
}

fn copyable_name(name: &str, directory: bool) -> bool {
    !name.contains('\\')
        && !name.is_empty()
        && (!name.starts_with('.')
            || (!directory
                && name != ".loom.md"
                && matches!(
                    Path::new(name).extension().and_then(|ext| ext.to_str()),
                    Some("md" | "markdown")
                )))
}

fn discover(source: &Path, destination: &str, stop: &AtomicBool) -> Result<CopyPlan, IpcFailure> {
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| copyable_name(name, true))
        .ok_or_else(|| failure("Choose an ordinary named folder to copy."))?;
    let parent = Dir::open_ambient_dir(
        source
            .parent()
            .ok_or_else(|| failure("The source has no parent."))?,
        cap_std::ambient_authority(),
    )
    .map_err(io_failure)?;
    let directory = parent.open_dir_nofollow(name).map_err(io_failure)?;
    let root = Path::new(destination).join(name);
    let mut pending = vec![(Arc::new(directory), root.clone())];
    let mut plan = CopyPlan {
        directories: vec![root],
        files: Vec::new(),
        failures: Vec::new(),
    };
    let mut remaining = MAX_BYTES;
    let mut visited = 0;
    let deadline = Instant::now() + Duration::from_secs(180);
    while let Some((directory, relative)) = pending.pop() {
        for entry in directory.entries().map_err(io_failure)? {
            if stop.load(Ordering::Acquire) {
                return Err(failure("Folder copy stopped."));
            }
            visited += 1;
            if visited > MAX_ENTRIES || Instant::now() >= deadline {
                return Err(failure(
                    "This folder exceeds the copy limit (4,096 entries or three minutes). Open it in place instead.",
                ));
            }
            let entry = entry.map_err(io_failure)?;
            let name = entry.file_name();
            let target = relative.join(&name);
            let result = (|| {
                let kind = entry.file_type().map_err(io_failure)?;
                if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                    return Err(failure("Links and special files are not copied."));
                }
                if !name
                    .to_str()
                    .is_some_and(|name| copyable_name(name, kind.is_dir()))
                {
                    return Err(failure(
                        "Private settings, hidden folders, and unsupported names remain in the original folder.",
                    ));
                }
                if kind.is_dir() {
                    if target.components().count() > 64 {
                        return Err(failure("The folder is nested too deeply to copy."));
                    }
                    let child = directory.open_dir_nofollow(&name).map_err(io_failure)?;
                    pending.push((Arc::new(child), target.clone()));
                    plan.directories.push(target.clone());
                } else {
                    let length = entry.metadata().map_err(io_failure)?.len();
                    if length > remaining {
                        return Err(failure(
                            "This file exceeds the remaining 128 MB copy allowance.",
                        ));
                    }
                    remaining -= length;
                    plan.files.push(CopyFile {
                        parent: Arc::clone(&directory),
                        name,
                        destination: target.clone(),
                        byte_limit: length,
                    });
                }
                Ok(())
            })();
            if let Err(error) = result {
                plan.failures.push(ImportFailure {
                    name: target.to_string_lossy().into_owned(),
                    message: error.message,
                });
            }
        }
    }
    plan.directories.sort();
    plan.files
        .sort_by(|left, right| left.destination.cmp(&right.destination));
    Ok(plan)
}

fn read(file: &CopyFile) -> Result<PreparedWorkspaceCopy, IpcFailure> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let input = file
        .parent
        .open_with(&file.name, &options)
        .map_err(io_failure)?;
    let metadata = input.metadata().map_err(io_failure)?;
    if !metadata.is_file() || metadata.len() > file.byte_limit {
        return Err(failure(
            "The source changed after the copy was planned. Try again.",
        ));
    }
    let mut bytes = Vec::new();
    input
        .take(file.byte_limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_failure)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != file.byte_limit {
        return Err(failure(
            "The source size changed while it was being copied. Try again.",
        ));
    }
    PreparedWorkspaceCopy::from_bytes(&file.destination, bytes).map_err(IpcFailure::store)
}

async fn copy_plan(
    operation: &ImportOperation,
    state: &PluginState,
    plan: CopyPlan,
) -> Result<CopyReport, IpcFailure> {
    let mut report = CopyReport {
        copied: Vec::new(),
        materials: Vec::new(),
        failures: plan.failures,
    };
    for directory in plan.directories {
        // Refuse the complete copy if its top-level folder already exists;
        // no merge, overwrite, recursive cleanup, or replacement on failure.
        operation.publish_to_store(state, |store| {
            store
                .create_workspace_copy_directory(&directory)
                .map_err(IpcFailure::store)
        })?;
    }
    for file in plan.files {
        if let Err(error) = operation.check() {
            report.failures.push(ImportFailure {
                name: "Remaining files".into(),
                message: error.message,
            });
            break;
        }
        let name = file.destination.to_string_lossy().into_owned();
        let root = operation.root.clone();
        let result = operation
            .compute(move || prepare_file(&root, read(&file)?))
            .await
            .and_then(|prepared| {
                operation.publish_to_store(state, |store| publish(prepared, store))
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
                name,
                message: error.message,
            }),
        }
    }
    Ok(report)
}

#[tauri::command]
pub(crate) async fn workspace_copy_folder_choose<R: Runtime>(
    project_id: String,
    session_id: String,
    operation_id: String,
    destination: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<Option<CopyReport>, IpcFailure> {
    if destination.len() > 4096 {
        return Err(failure("The destination path is too long."));
    }
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    let Some(source) = app
        .dialog()
        .file()
        .set_title("Copy Folder Here")
        .blocking_pick_folder()
    else {
        return Ok(None);
    };
    let source = source
        .into_path()
        .map_err(|_| failure("Choose a local folder."))?;
    let stop = operation.stop_flag();
    let plan = operation
        .compute(move || discover(&source, &destination, &stop))
        .await?;
    copy_plan(&operation, &state, plan).await.map(Some)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn folder_copy_preserves_paths_bytes_and_partial_failures_without_merging() {
        let source = tempfile::tempdir().unwrap();
        let folder = source.path().join("Research");
        fs::create_dir_all(folder.join("Notes/Empty")).unwrap();
        fs::create_dir(folder.join(".loom")).unwrap();
        let writing = b"# Source\r\n\r\nUnchanged.\r\n";
        fs::write(folder.join("Notes/original.md"), writing).unwrap();
        fs::write(folder.join(".loom/history.db"), b"private history").unwrap();
        // Even a disguised SQLite file must not become an inconsistent copy.
        let database = b"SQLite format 3\0source database";
        fs::write(folder.join("library.bin"), database).unwrap();
        fs::write(folder.join("library.bin-wal"), b"pending writes").unwrap();
        std::os::unix::fs::symlink("Notes/original.md", folder.join("linked.md")).unwrap();
        let root = tempfile::tempdir().unwrap();
        let state = PluginState::default();
        let store = crate::initialize_project(root.path(), "Copy".into()).unwrap();
        let snapshot = crate::reserve_project_choice(&state)
            .unwrap()
            .finish_without_document_filesystem_watcher(Ok(store))
            .unwrap();
        let operation = ImportOperation::reserve(
            &state,
            &snapshot.project_id,
            &snapshot.session_id,
            &crate::CommandId::new().to_string(),
        )
        .unwrap();
        let plan = discover(&folder, "Sources", &operation.stop_flag()).unwrap();
        let report = tauri::async_runtime::block_on(copy_plan(&operation, &state, plan)).unwrap();
        assert_eq!(report.copied, ["Sources/Research/Notes/original.md"]);
        assert_eq!(report.failures.len(), 4);
        assert!(root.path().join("Sources/Research/Notes/Empty").is_dir());
        assert_eq!(
            fs::read(root.path().join(&report.copied[0])).unwrap(),
            writing
        );
        assert_eq!(fs::read(folder.join("Notes/original.md")).unwrap(), writing);
        assert_eq!(fs::read(folder.join("library.bin")).unwrap(), database);
        assert_eq!(
            fs::read(folder.join("library.bin-wal")).unwrap(),
            b"pending writes"
        );
        assert!(!root.path().join("Sources/Research/.loom").exists());
        assert!(!root.path().join("Sources/Research/library.bin").exists());
        let retry = discover(&folder, "Sources", &operation.stop_flag()).unwrap();
        assert!(tauri::async_runtime::block_on(copy_plan(&operation, &state, retry)).is_err());
        let escape = discover(&folder, "../escape", &operation.stop_flag()).unwrap();
        assert!(tauri::async_runtime::block_on(copy_plan(&operation, &state, escape)).is_err());
        assert_eq!(
            fs::read(root.path().join(&report.copied[0])).unwrap(),
            writing
        );
        drop(operation);
        state.imports.shutdown().unwrap();
    }

    #[test]
    fn discovery_and_retained_handles_do_not_follow_source_replacements() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("Source");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("note.txt"), "original").unwrap();
        let stopped = AtomicBool::new(true);
        assert!(discover(&folder, "", &stopped).is_err());
        let plan = discover(&folder, "", &AtomicBool::new(false)).unwrap();
        fs::rename(&folder, root.path().join("Moved")).unwrap();
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("note.txt"), "intruder").unwrap();
        let copied = read(&plan.files.into_iter().next().unwrap()).unwrap();
        assert_eq!(copied.bytes(), b"original");
        let plan = discover(&folder, "", &AtomicBool::new(false)).unwrap();
        fs::remove_file(folder.join("note.txt")).unwrap();
        std::os::unix::fs::symlink(root.path().join("Moved/note.txt"), folder.join("note.txt"))
            .unwrap();
        assert!(read(&plan.files.into_iter().next().unwrap()).is_err());
        let linked = root.path().join("Linked");
        std::os::unix::fs::symlink(&folder, &linked).unwrap();
        assert!(discover(&linked, "", &AtomicBool::new(false)).is_err());
    }
}
