use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use loom_backend_llama::{
    DownloadCancellation, DownloadPhase, DownloadProgress, GgufDownloadResult,
};
use loom_types::CommandId;
use serde::Serialize;
use thiserror::Error;

pub(crate) const MAX_ACTIVE_MODEL_DOWNLOADS: usize = 2;
pub(crate) const MAX_RETAINED_MODEL_DOWNLOADS: usize = 128;
pub(crate) const MAX_MODEL_FILE_NAME_BYTES: usize = 240;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelDownloadSpec {
    pub command_id: CommandId,
    pub request_fingerprint: String,
    pub display_name: String,
    pub target_path: PathBuf,
    pub expected_sha256: String,
    pub expected_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum ModelDownloadStatus {
    Queued,
    Running,
    Completed {
        bytes: u64,
        sha256: String,
        disposition: &'static str,
    },
    Cancelled,
    Failed {
        message: String,
        retryable: bool,
    },
}

impl ModelDownloadStatus {
    pub(crate) const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled | Self::Failed { .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ModelDownloadSnapshot {
    pub command_id: String,
    pub request_fingerprint: String,
    pub display_name: String,
    pub target_path: String,
    pub expected_sha256: String,
    pub expected_bytes: Option<u64>,
    pub phase: Option<DownloadPhase>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub resumed_from_bytes: u64,
    pub status: ModelDownloadStatus,
    pub cancel_requested: bool,
    pub event_sequence: u64,
    pub event_delivery_failures: u64,
    pub updated_at_unix_ms: i64,
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReservationOutcome {
    Started,
    Replayed,
}

#[derive(Debug)]
struct ModelDownloadEntry {
    spec: ModelDownloadSpec,
    phase: Option<DownloadPhase>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    resumed_from_bytes: u64,
    status: ModelDownloadStatus,
    cancellation: DownloadCancellation,
    cancel_requested: bool,
    event_sequence: u64,
    event_delivery_failures: u64,
    updated_at_unix_ms: i64,
}

impl ModelDownloadEntry {
    fn snapshot(&self, replayed: bool) -> ModelDownloadSnapshot {
        ModelDownloadSnapshot {
            command_id: self.spec.command_id.to_string(),
            request_fingerprint: self.spec.request_fingerprint.clone(),
            display_name: self.spec.display_name.clone(),
            target_path: self.spec.target_path.to_string_lossy().into_owned(),
            expected_sha256: self.spec.expected_sha256.clone(),
            expected_bytes: self.spec.expected_bytes,
            phase: self.phase,
            downloaded_bytes: self.downloaded_bytes,
            total_bytes: self.total_bytes.or(self.spec.expected_bytes),
            resumed_from_bytes: self.resumed_from_bytes,
            status: self.status.clone(),
            cancel_requested: self.cancel_requested,
            event_sequence: self.event_sequence,
            event_delivery_failures: self.event_delivery_failures,
            updated_at_unix_ms: self.updated_at_unix_ms,
            replayed,
        }
    }
}

#[derive(Debug, Default)]
struct ModelDownloadState {
    entries: BTreeMap<CommandId, ModelDownloadEntry>,
}

#[derive(Debug, Default)]
pub(crate) struct ModelDownloadRegistry {
    state: Mutex<ModelDownloadState>,
}

impl ModelDownloadRegistry {
    pub(crate) fn reserve(
        &self,
        spec: ModelDownloadSpec,
        now_unix_ms: i64,
    ) -> Result<(ReservationOutcome, ModelDownloadSnapshot), ModelDownloadRegistryError> {
        let mut state = self.lock()?;
        if let Some(existing) = state.entries.get(&spec.command_id) {
            if existing.spec.request_fingerprint != spec.request_fingerprint {
                return Err(ModelDownloadRegistryError::IdempotencyConflict {
                    command_id: spec.command_id,
                });
            }
            return Ok((ReservationOutcome::Replayed, existing.snapshot(true)));
        }

        let active = state
            .entries
            .values()
            .filter(|entry| !entry.status.is_terminal())
            .count();
        if active >= MAX_ACTIVE_MODEL_DOWNLOADS {
            return Err(ModelDownloadRegistryError::ActiveCapacity {
                active,
                limit: MAX_ACTIVE_MODEL_DOWNLOADS,
            });
        }
        prune_oldest_terminal(&mut state);
        if state.entries.len() >= MAX_RETAINED_MODEL_DOWNLOADS {
            return Err(ModelDownloadRegistryError::RetainedCapacity {
                limit: MAX_RETAINED_MODEL_DOWNLOADS,
            });
        }

        let command_id = spec.command_id;
        let entry = ModelDownloadEntry {
            spec,
            phase: None,
            downloaded_bytes: 0,
            total_bytes: None,
            resumed_from_bytes: 0,
            status: ModelDownloadStatus::Queued,
            cancellation: DownloadCancellation::default(),
            cancel_requested: false,
            event_sequence: 0,
            event_delivery_failures: 0,
            updated_at_unix_ms: now_unix_ms,
        };
        let snapshot = entry.snapshot(false);
        state.entries.insert(command_id, entry);
        Ok((ReservationOutcome::Started, snapshot))
    }

    pub(crate) fn cancellation(
        &self,
        command_id: CommandId,
    ) -> Result<DownloadCancellation, ModelDownloadRegistryError> {
        let state = self.lock()?;
        state
            .entries
            .get(&command_id)
            .map(|entry| entry.cancellation.clone())
            .ok_or(ModelDownloadRegistryError::NotFound(command_id))
    }

    pub(crate) fn record_progress(
        &self,
        command_id: CommandId,
        progress: DownloadProgress,
        now_unix_ms: i64,
    ) -> Result<ModelDownloadSnapshot, ModelDownloadRegistryError> {
        let mut state = self.lock()?;
        let entry = state
            .entries
            .get_mut(&command_id)
            .ok_or(ModelDownloadRegistryError::NotFound(command_id))?;
        if entry.status.is_terminal() {
            return Err(ModelDownloadRegistryError::AlreadyTerminal(command_id));
        }
        entry.phase = Some(progress.phase);
        entry.downloaded_bytes = progress.downloaded_bytes;
        entry.total_bytes = progress.total_bytes;
        entry.resumed_from_bytes = progress.resumed_from_bytes;
        entry.status = ModelDownloadStatus::Running;
        entry.event_sequence = entry.event_sequence.saturating_add(1);
        entry.updated_at_unix_ms = now_unix_ms;
        Ok(entry.snapshot(false))
    }

    pub(crate) fn complete(
        &self,
        command_id: CommandId,
        result: &GgufDownloadResult,
        now_unix_ms: i64,
    ) -> Result<ModelDownloadSnapshot, ModelDownloadRegistryError> {
        let mut state = self.lock()?;
        let entry = terminal_entry(&mut state, command_id)?;
        entry.phase = Some(DownloadPhase::Complete);
        entry.downloaded_bytes = result.bytes;
        entry.total_bytes = Some(result.bytes);
        entry.status = ModelDownloadStatus::Completed {
            bytes: result.bytes,
            sha256: result.sha256.to_string(),
            disposition: download_disposition_name(result),
        };
        entry.event_sequence = entry.event_sequence.saturating_add(1);
        entry.updated_at_unix_ms = now_unix_ms;
        Ok(entry.snapshot(false))
    }

    pub(crate) fn fail(
        &self,
        command_id: CommandId,
        message: String,
        retryable: bool,
        now_unix_ms: i64,
    ) -> Result<ModelDownloadSnapshot, ModelDownloadRegistryError> {
        let mut state = self.lock()?;
        let entry = terminal_entry(&mut state, command_id)?;
        entry.status = ModelDownloadStatus::Failed { message, retryable };
        entry.event_sequence = entry.event_sequence.saturating_add(1);
        entry.updated_at_unix_ms = now_unix_ms;
        Ok(entry.snapshot(false))
    }

    pub(crate) fn finish_cancelled(
        &self,
        command_id: CommandId,
        now_unix_ms: i64,
    ) -> Result<ModelDownloadSnapshot, ModelDownloadRegistryError> {
        let mut state = self.lock()?;
        let entry = terminal_entry(&mut state, command_id)?;
        entry.status = ModelDownloadStatus::Cancelled;
        entry.event_sequence = entry.event_sequence.saturating_add(1);
        entry.updated_at_unix_ms = now_unix_ms;
        Ok(entry.snapshot(false))
    }

    pub(crate) fn request_cancel(
        &self,
        command_id: CommandId,
        now_unix_ms: i64,
    ) -> Result<ModelDownloadSnapshot, ModelDownloadRegistryError> {
        let mut state = self.lock()?;
        let entry = state
            .entries
            .get_mut(&command_id)
            .ok_or(ModelDownloadRegistryError::NotFound(command_id))?;
        if !entry.status.is_terminal() {
            entry.cancel_requested = true;
            entry.event_sequence = entry.event_sequence.saturating_add(1);
            entry.updated_at_unix_ms = now_unix_ms;
            entry.cancellation.cancel();
        }
        Ok(entry.snapshot(true))
    }

    pub(crate) fn status(
        &self,
        command_id: CommandId,
    ) -> Result<ModelDownloadSnapshot, ModelDownloadRegistryError> {
        let state = self.lock()?;
        state
            .entries
            .get(&command_id)
            .map(|entry| entry.snapshot(true))
            .ok_or(ModelDownloadRegistryError::NotFound(command_id))
    }

    pub(crate) fn record_delivery_failure(
        &self,
        command_id: CommandId,
    ) -> Result<(), ModelDownloadRegistryError> {
        let mut state = self.lock()?;
        let entry = state
            .entries
            .get_mut(&command_id)
            .ok_or(ModelDownloadRegistryError::NotFound(command_id))?;
        entry.event_delivery_failures = entry.event_delivery_failures.saturating_add(1);
        Ok(())
    }

    pub(crate) fn list(&self) -> Result<Vec<ModelDownloadSnapshot>, ModelDownloadRegistryError> {
        let state = self.lock()?;
        let mut snapshots = state
            .entries
            .values()
            .map(|entry| entry.snapshot(true))
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            right
                .updated_at_unix_ms
                .cmp(&left.updated_at_unix_ms)
                .then_with(|| right.command_id.cmp(&left.command_id))
        });
        Ok(snapshots)
    }

    fn lock(&self) -> Result<MutexGuard<'_, ModelDownloadState>, ModelDownloadRegistryError> {
        self.state
            .lock()
            .map_err(|_| ModelDownloadRegistryError::Poisoned)
    }
}

pub(crate) fn prepare_model_library(
    app_local_data_root: &std::path::Path,
) -> Result<PathBuf, ModelLibraryError> {
    fs::create_dir_all(app_local_data_root).map_err(|source| ModelLibraryError::Io {
        operation: "create application data directory",
        path: app_local_data_root.to_path_buf(),
        source,
    })?;
    let library = app_local_data_root.join("models");
    match fs::symlink_metadata(&library) {
        Ok(metadata) => validate_model_library_metadata(&library, &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(&library) {
            Ok(()) => set_private_directory_permissions(&library)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata =
                    fs::symlink_metadata(&library).map_err(|source| ModelLibraryError::Io {
                        operation: "inspect raced model library",
                        path: library.clone(),
                        source,
                    })?;
                validate_model_library_metadata(&library, &metadata)?;
            }
            Err(source) => {
                return Err(ModelLibraryError::Io {
                    operation: "create model library",
                    path: library,
                    source,
                });
            }
        },
        Err(source) => {
            return Err(ModelLibraryError::Io {
                operation: "inspect model library",
                path: library,
                source,
            });
        }
    }
    Ok(library)
}

pub(crate) fn model_target_path(
    library: &std::path::Path,
    file_name: &str,
) -> Result<PathBuf, ModelLibraryError> {
    validate_model_file_name(file_name)?;
    Ok(library.join(file_name))
}

fn validate_model_file_name(file_name: &str) -> Result<(), ModelLibraryError> {
    if file_name.is_empty() || file_name.len() > MAX_MODEL_FILE_NAME_BYTES {
        return Err(ModelLibraryError::InvalidFileName);
    }
    if file_name.ends_with('.')
        || file_name.ends_with(' ')
        || file_name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
    {
        return Err(ModelLibraryError::InvalidFileName);
    }
    let path = std::path::Path::new(file_name);
    if path.file_name().and_then(std::ffi::OsStr::to_str) != Some(file_name)
        || path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("gguf"))
        || path
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .is_none_or(str::is_empty)
    {
        return Err(ModelLibraryError::InvalidFileName);
    }
    let stem = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or(ModelLibraryError::InvalidFileName)?;
    let device_name = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    if is_reserved_windows_device_name(&device_name) {
        return Err(ModelLibraryError::InvalidFileName);
    }
    Ok(())
}

fn is_reserved_windows_device_name(value: &str) -> bool {
    matches!(value, "CON" | "PRN" | "AUX" | "NUL")
        || value
            .strip_prefix("COM")
            .or_else(|| value.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

fn validate_model_library_metadata(
    path: &std::path::Path,
    metadata: &fs::Metadata,
) -> Result<(), ModelLibraryError> {
    if metadata.file_type().is_symlink() {
        return Err(ModelLibraryError::Symlink(path.to_path_buf()));
    }
    if !metadata.is_dir() {
        return Err(ModelLibraryError::NotDirectory(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &std::path::Path) -> Result<(), ModelLibraryError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        ModelLibraryError::Io {
            operation: "protect model library",
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &std::path::Path) -> Result<(), ModelLibraryError> {
    Ok(())
}

fn terminal_entry(
    state: &mut ModelDownloadState,
    command_id: CommandId,
) -> Result<&mut ModelDownloadEntry, ModelDownloadRegistryError> {
    let entry = state
        .entries
        .get_mut(&command_id)
        .ok_or(ModelDownloadRegistryError::NotFound(command_id))?;
    if entry.status.is_terminal() {
        return Err(ModelDownloadRegistryError::AlreadyTerminal(command_id));
    }
    Ok(entry)
}

fn prune_oldest_terminal(state: &mut ModelDownloadState) {
    while state.entries.len() >= MAX_RETAINED_MODEL_DOWNLOADS {
        let oldest = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.status.is_terminal())
            .min_by_key(|(command_id, entry)| (entry.updated_at_unix_ms, **command_id))
            .map(|(command_id, _)| *command_id);
        let Some(command_id) = oldest else {
            break;
        };
        state.entries.remove(&command_id);
    }
}

fn download_disposition_name(result: &GgufDownloadResult) -> &'static str {
    use loom_backend_llama::DownloadDisposition;

    match result.disposition {
        DownloadDisposition::ReusedExisting => "reused_existing",
        DownloadDisposition::DownloadedFresh => "downloaded_fresh",
        DownloadDisposition::DownloadedResumed { .. } => "downloaded_resumed",
        DownloadDisposition::DownloadedAfterRestart { .. } => "downloaded_after_restart",
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum ModelDownloadRegistryError {
    #[error("download command {command_id} was reused with different inputs")]
    IdempotencyConflict { command_id: CommandId },
    #[error("model download capacity exceeded: {active} active, limit {limit}")]
    ActiveCapacity { active: usize, limit: usize },
    #[error("model download history reached its {limit}-entry limit")]
    RetainedCapacity { limit: usize },
    #[error("model download command {0} was not found")]
    NotFound(CommandId),
    #[error("model download command {0} is already terminal")]
    AlreadyTerminal(CommandId),
    #[error("model download registry is poisoned")]
    Poisoned,
}

#[derive(Debug, Error)]
pub(crate) enum ModelLibraryError {
    #[error("model file name must be one portable .gguf file name")]
    InvalidFileName,
    #[error("model library is a symbolic link: {0}")]
    Symlink(PathBuf),
    #[error("model library is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("could not {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_backend_llama::{DownloadDisposition, Sha256Digest};

    fn spec(command_id: CommandId, fingerprint: &str) -> ModelDownloadSpec {
        ModelDownloadSpec {
            command_id,
            request_fingerprint: fingerprint.to_owned(),
            display_name: "Writer model".to_owned(),
            target_path: PathBuf::from("/models/writer.gguf"),
            expected_sha256: "ab".repeat(32),
            expected_bytes: Some(128),
        }
    }

    #[test]
    fn exact_command_replays_and_changed_input_fails_closed() {
        let registry = ModelDownloadRegistry::default();
        let command_id = CommandId::new();
        let (outcome, first) = registry
            .reserve(spec(command_id, "one"), 1)
            .expect("reserve download");
        assert_eq!(outcome, ReservationOutcome::Started);
        assert!(!first.replayed);

        let (outcome, replay) = registry
            .reserve(spec(command_id, "one"), 2)
            .expect("replay download");
        assert_eq!(outcome, ReservationOutcome::Replayed);
        assert!(replay.replayed);
        assert_eq!(
            registry.reserve(spec(command_id, "two"), 3),
            Err(ModelDownloadRegistryError::IdempotencyConflict { command_id })
        );
    }

    #[test]
    fn active_jobs_are_bounded_and_terminal_jobs_free_capacity() {
        let registry = ModelDownloadRegistry::default();
        let first = CommandId::new();
        let second = CommandId::new();
        let third = CommandId::new();
        registry.reserve(spec(first, "one"), 1).expect("first");
        registry.reserve(spec(second, "two"), 2).expect("second");
        assert_eq!(
            registry.reserve(spec(third, "three"), 3),
            Err(ModelDownloadRegistryError::ActiveCapacity {
                active: 2,
                limit: MAX_ACTIVE_MODEL_DOWNLOADS,
            })
        );
        registry
            .finish_cancelled(first, 4)
            .expect("finish cancelled");
        registry.reserve(spec(third, "three"), 5).expect("third");
    }

    #[test]
    fn progress_cancellation_and_terminal_state_are_queryable() {
        let registry = ModelDownloadRegistry::default();
        let command_id = CommandId::new();
        registry
            .reserve(spec(command_id, "one"), 1)
            .expect("reserve");
        let progress = registry
            .record_progress(
                command_id,
                DownloadProgress {
                    phase: DownloadPhase::Downloading,
                    downloaded_bytes: 64,
                    total_bytes: Some(128),
                    resumed_from_bytes: 32,
                },
                2,
            )
            .expect("progress");
        assert_eq!(progress.downloaded_bytes, 64);
        assert_eq!(progress.resumed_from_bytes, 32);
        assert_eq!(progress.event_sequence, 1);

        registry
            .record_delivery_failure(command_id)
            .expect("record delivery failure");
        assert_eq!(
            registry
                .status(command_id)
                .expect("status after delivery failure")
                .event_delivery_failures,
            1
        );

        let cancelled = registry
            .request_cancel(command_id, 3)
            .expect("request cancellation");
        assert!(cancelled.cancel_requested);
        assert_eq!(cancelled.event_sequence, 2);
        assert!(
            registry
                .cancellation(command_id)
                .expect("token")
                .is_cancelled()
        );
        let terminal = registry
            .finish_cancelled(command_id, 4)
            .expect("finish cancellation");
        assert_eq!(terminal.status, ModelDownloadStatus::Cancelled);
        assert_eq!(terminal.event_sequence, 3);
        assert_eq!(
            registry.record_progress(
                command_id,
                DownloadProgress {
                    phase: DownloadPhase::Downloading,
                    downloaded_bytes: 65,
                    total_bytes: Some(128),
                    resumed_from_bytes: 32,
                },
                5,
            ),
            Err(ModelDownloadRegistryError::AlreadyTerminal(command_id))
        );
    }

    #[test]
    fn completion_records_verified_result_once() {
        let registry = ModelDownloadRegistry::default();
        let command_id = CommandId::new();
        registry
            .reserve(spec(command_id, "one"), 1)
            .expect("reserve");
        let result = GgufDownloadResult {
            target_path: PathBuf::from("/models/writer.gguf"),
            bytes: 128,
            sha256: Sha256Digest::from_hex(&"ab".repeat(32)).expect("digest"),
            disposition: DownloadDisposition::DownloadedFresh,
            partial_removed: true,
        };
        let terminal = registry.complete(command_id, &result, 2).expect("complete");
        assert_eq!(terminal.phase, Some(DownloadPhase::Complete));
        assert!(matches!(
            terminal.status,
            ModelDownloadStatus::Completed {
                bytes: 128,
                disposition: "downloaded_fresh",
                ..
            }
        ));
        assert_eq!(
            registry.complete(command_id, &result, 3),
            Err(ModelDownloadRegistryError::AlreadyTerminal(command_id))
        );
    }

    #[test]
    fn model_file_names_are_portable_single_components() {
        let library = PathBuf::from("/models");
        assert_eq!(
            model_target_path(&library, "Gemma 4 base.Q8_0.gguf").expect("portable name"),
            library.join("Gemma 4 base.Q8_0.gguf")
        );
        for invalid in [
            "",
            ".gguf",
            "model.bin",
            "../model.gguf",
            "folder/model.gguf",
            "folder\\model.gguf",
            "model.gguf.",
            "CON.gguf",
            "lpt9.gguf",
        ] {
            assert!(
                matches!(
                    model_target_path(&library, invalid),
                    Err(ModelLibraryError::InvalidFileName)
                ),
                "{invalid:?} should be rejected"
            );
        }
    }

    #[test]
    fn model_library_refuses_symlinks_and_non_directories() {
        let root = tempfile::tempdir().expect("tempdir");
        let library = prepare_model_library(root.path()).expect("create library");
        assert!(library.is_dir());

        let other = tempfile::tempdir().expect("other tempdir");
        let file_root = other.path().join("file-root");
        fs::create_dir(&file_root).expect("file root");
        fs::write(file_root.join("models"), b"not a directory").expect("write blocker");
        assert!(matches!(
            prepare_model_library(&file_root),
            Err(ModelLibraryError::NotDirectory(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn model_library_refuses_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let destination = tempfile::tempdir().expect("destination");
        symlink(destination.path(), root.path().join("models")).expect("create symlink");
        assert!(matches!(
            prepare_model_library(root.path()),
            Err(ModelLibraryError::Symlink(_))
        ));
    }
}
