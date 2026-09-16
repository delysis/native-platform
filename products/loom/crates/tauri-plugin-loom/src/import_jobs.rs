//! Imports own their workers until completion, independently of the RPC waiter.
//! The editor locks protect admission/publication, never conversion or download.
use super::{
    IpcFailure, PluginState, lock_application_admission, lock_session, require_bound_store,
};
use crate::context_attachments::{PreparedAttachment, StoredAttachment};
use same_file::Handle;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

#[derive(Debug, Default)]
pub(super) struct ImportJobs {
    inner: Mutex<Registry>,
    drain: Mutex<()>,
}

#[derive(Debug, Default)]
struct Registry {
    closed: bool,
    cancel_scope: Option<String>,
    cancelled: std::collections::BTreeSet<String>,
    revoked_session: Option<String>,
    active: Option<ActiveImport>,
    workers: Vec<JoinHandle<()>>,
}

#[derive(Debug)]
struct ActiveImport {
    session_id: String,
    operation_id: String,
    cancel: Arc<AtomicBool>,
    signal: tokio::sync::watch::Sender<bool>,
}

#[derive(Debug)]
pub(super) struct ImportOperation {
    jobs: Arc<ImportJobs>,
    cancel: Arc<AtomicBool>,
    signal: tokio::sync::watch::Sender<bool>,
    project_id: String,
    session_id: String,
    pub(super) root: PathBuf,
    root_identity: Handle,
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("import_stopped", message, false)
}

fn validate_operation_id(id: &str) -> Result<(), IpcFailure> {
    id.parse::<crate::CommandId>()
        .map(|_| ())
        .map_err(|_| failure("The import operation ID is invalid."))
}

impl ImportJobs {
    pub(super) fn cancel(&self, session_id: &str, operation_id: &str) -> Result<(), IpcFailure> {
        validate_operation_id(operation_id)?;
        let mut registry = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(active) = &registry.active
            && active.session_id == session_id
            && active.operation_id == operation_id
        {
            active.cancel.store(true, Ordering::Release);
            active.signal.send_replace(true);
        }
        if registry.cancel_scope.as_deref() != Some(session_id) {
            registry.cancel_scope = Some(session_id.to_owned());
            registry.cancelled.clear();
        }
        // Keep preadmission cancellation until the session ends. Never evict a
        // latch that an outstanding IPC request could still consume.
        if registry.cancelled.len() >= 1024 && !registry.cancelled.contains(operation_id) {
            registry.revoked_session = Some(session_id.to_owned());
            return Err(failure(
                "Too many stopped imports. Reopen this folder to import more sources.",
            ));
        }
        registry.cancelled.insert(operation_id.to_owned());
        Ok(())
    }

    pub(super) fn revoke_session(&self, session_id: &str) {
        let mut registry = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.revoked_session = Some(session_id.to_owned());
        if let Some(active) = &registry.active {
            active.cancel.store(true, Ordering::Release);
            active.signal.send_replace(true);
        }
    }

    pub(super) fn drain_session(&self, session_id: &str) -> Result<usize, IpcFailure> {
        self.drain(Some(session_id))
    }

    pub(super) fn shutdown(&self) -> Result<usize, IpcFailure> {
        self.drain(None)
    }

    fn drain(&self, session_id: Option<&str>) -> Result<usize, IpcFailure> {
        // A second close must wait for the first join, not mistake its taken
        // handles for an already drained registry. Cancel uses only inner.
        let _drain = self
            .drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workers = {
            let mut registry = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(session_id) = session_id {
                registry.revoked_session = Some(session_id.to_owned());
            } else {
                registry.closed = true;
            }
            if let Some(active) = &registry.active {
                active.cancel.store(true, Ordering::Release);
                active.signal.send_replace(true);
            }
            std::mem::take(&mut registry.workers)
        };
        let count = workers.len();
        let mut panicked = false;
        for worker in workers {
            panicked |= worker.join().is_err();
        }
        if panicked {
            Err(failure("An import worker stopped unexpectedly."))
        } else {
            Ok(count)
        }
    }
}

impl ImportOperation {
    pub(super) fn reserve(
        state: &PluginState,
        project_id: &str,
        session_id: &str,
        operation_id: &str,
    ) -> Result<Self, IpcFailure> {
        validate_operation_id(operation_id)?;
        let _admission = lock_application_admission(state, "an import")?;
        let mut session = lock_session(state)?;
        let store = require_bound_store(&mut session, project_id, session_id)?;
        let root = store.root().to_owned();
        let root_identity = Handle::from_path(&root).map_err(|error| failure(error.to_string()))?;
        let mut registry = state
            .imports
            .inner
            .lock()
            .map_err(|_| failure("Import state is unavailable."))?;
        if registry.closed || registry.revoked_session.as_deref() == Some(session_id) {
            return Err(failure("This project session is closing."));
        }
        if registry.cancel_scope.as_deref() == Some(session_id)
            && registry.cancelled.contains(operation_id)
        {
            return Err(failure("Import stopped before it began."));
        }
        if registry.active.is_some() || registry.workers.iter().any(|worker| !worker.is_finished())
        {
            return Err(failure(
                "Another import is running. Stop it or wait for it to finish.",
            ));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (signal, _) = tokio::sync::watch::channel(false);
        registry.active = Some(ActiveImport {
            session_id: session_id.to_owned(),
            operation_id: operation_id.to_owned(),
            cancel: Arc::clone(&cancel),
            signal: signal.clone(),
        });
        Ok(Self {
            jobs: Arc::clone(&state.imports),
            cancel,
            signal,
            root,
            root_identity,
            project_id: project_id.to_owned(),
            session_id: session_id.to_owned(),
        })
    }

    pub(super) fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    pub(super) fn check(&self) -> Result<(), IpcFailure> {
        if self.cancel.load(Ordering::Acquire) {
            Err(failure(
                "Import stopped. Completed sources remain available.",
            ))
        } else {
            Ok(())
        }
    }

    /// The registry keeps the join handle even if the RPC future is abandoned.
    pub(super) async fn compute<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, IpcFailure> + Send + 'static,
    ) -> Result<T, IpcFailure> {
        self.check()?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut registry = self
                .jobs
                .inner
                .lock()
                .map_err(|_| failure("Import state is unavailable."))?;
            self.check()?;
            if registry.closed {
                return Err(failure("Loom is closing."));
            }
            // Reap completed handles without waiting for any running worker.
            let mut index = 0;
            while index < registry.workers.len() {
                if registry.workers[index].is_finished() {
                    registry
                        .workers
                        .swap_remove(index)
                        .join()
                        .map_err(|_| failure("An import worker stopped unexpectedly."))?;
                } else {
                    index += 1;
                }
            }
            let cancel = Arc::clone(&self.cancel);
            let worker = std::thread::Builder::new()
                .name("loom-import".into())
                .spawn(move || {
                    let result = if cancel.load(Ordering::Acquire) {
                        Err(failure("Import stopped."))
                    } else {
                        work()
                    };
                    let _ = sender.send(result);
                })
                .map_err(|error| failure(error.to_string()))?;
            registry.workers.push(worker);
        }
        let result = receiver
            .await
            .map_err(|_| failure("An import worker stopped unexpectedly."))?;
        self.check()?;
        result
    }

    /// Network futures live on an owned worker too. Cancellation drops the
    /// request future there; close joins that worker before releasing the store.
    pub(super) async fn network<T: Send + 'static>(
        &self,
        work: impl std::future::Future<Output = Result<T, IpcFailure>> + Send + 'static,
    ) -> Result<T, IpcFailure> {
        let mut signal = self.signal.subscribe();
        self.compute(move || {
            tauri::async_runtime::block_on(async move {
                if *signal.borrow() {
                    return Err(failure("Import stopped."));
                }
                tokio::select! {
                    result = tokio::time::timeout(std::time::Duration::from_mins(3), work) =>
                        result.map_err(|_| failure("The import reached its three-minute limit."))?,
                    _ = signal.changed() => Err(failure("Import stopped.")),
                }
            })
        })
        .await
    }

    pub(super) fn publish(
        &self,
        state: &PluginState,
        prepared: PreparedAttachment,
    ) -> Result<StoredAttachment, IpcFailure> {
        self.publish_action(state, || {
            prepared
                .publish()
                .map_err(|error| IpcFailure::context_attachment(&error))
        })
    }

    /// Only final publication belongs here; acquisition and conversion run in compute.
    pub(super) fn publish_action<T>(
        &self,
        state: &PluginState,
        commit: impl FnOnce() -> Result<T, IpcFailure>,
    ) -> Result<T, IpcFailure> {
        self.check()?;
        let _admission = lock_application_admission(state, "publishing an import")?;
        let mut session = lock_session(state)?;
        let store = require_bound_store(&mut session, &self.project_id, &self.session_id)?;
        // Serialize the final link with cancel/revoke. Once Stop returns, no
        // previously admitted worker can publish another selectable source.
        let _publication = self
            .jobs
            .inner
            .lock()
            .map_err(|_| failure("Import state is unavailable."))?;
        self.check()?;
        if store.root() != self.root
            || Handle::from_path(store.root()).map_err(|error| failure(error.to_string()))?
                != self.root_identity
        {
            return Err(failure("The import destination changed."));
        }
        commit()
    }
}

impl Drop for ImportOperation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.signal.send_replace(true);
        let mut registry = self
            .jobs
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.cancel, &self.cancel))
        {
            registry.active = None;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        CommandId, Duration, INITIAL_DOCUMENT, close_project_with_wait, initialize_project,
        reserve_project_choice,
    };
    use attachment_native_host::ProvidedAttachment;
    use std::sync::mpsc;

    fn opened(root: &std::path::Path) -> (Arc<PluginState>, String, String) {
        let state = Arc::new(PluginState::default());
        let store = initialize_project(root, "Import lifecycle".into()).unwrap();
        let snapshot = reserve_project_choice(&state)
            .unwrap()
            .finish_without_document_filesystem_watcher(Ok(store))
            .unwrap();
        (state, snapshot.project_id, snapshot.session_id)
    }

    fn published_count(root: &std::path::Path) -> usize {
        std::fs::read_dir(root.join(".loom/attachments/manifests"))
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
    fn stop_before_admission_is_retained_for_that_operation_only() {
        let temporary = tempfile::tempdir().unwrap();
        let (state, project, session) = opened(temporary.path());
        let id = CommandId::new().to_string();
        state.imports.cancel(&session, &id).unwrap();
        assert!(ImportOperation::reserve(&state, &project, &session, &id).is_err());
        let next =
            ImportOperation::reserve(&state, &project, &session, &CommandId::new().to_string())
                .unwrap();
        assert!(next.check().is_ok());
        assert_eq!(published_count(temporary.path()), 0);
    }

    #[test]
    fn stop_after_preparation_prevents_manifest_and_account_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let (state, project, session) = opened(temporary.path());
        let id = CommandId::new().to_string();
        let operation = ImportOperation::reserve(&state, &project, &session, &id).unwrap();
        let prepared = crate::context_attachments::prepare_provided(
            temporary.path(),
            ProvidedAttachment::from_bytes(
                "source.txt",
                None,
                b"Retained but not selected".to_vec(),
            ),
        )
        .unwrap();
        state.imports.cancel(&session, &id).unwrap();
        assert!(operation.publish(&state, prepared).is_err());
        let mut committed = false;
        assert!(
            operation
                .publish_action(&state, || {
                    committed = true;
                    Ok(())
                })
                .is_err()
        );
        assert!(
            !committed,
            "account publication shares the same Stop boundary"
        );
        assert_eq!(published_count(temporary.path()), 0);
    }

    #[test]
    fn blocked_conversion_releases_editor_locks_and_cancel_prevents_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let (state, project, session) = opened(temporary.path());
        tauri::async_runtime::block_on(async {
            let operation_id = CommandId::new().to_string();
            let operation =
                ImportOperation::reserve(&state, &project, &session, &operation_id).unwrap();
            let root = operation.root.clone();
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, blocked) = mpsc::channel();
            let importing = Arc::clone(&state);
            let task = tokio::spawn(async move {
                let prepared = operation
                    .compute(move || {
                        entered.send(()).unwrap();
                        blocked.recv().unwrap();
                        crate::context_attachments::prepare_provided(
                            &root,
                            ProvidedAttachment::from_bytes(
                                "source.txt",
                                None,
                                b"Untrusted source".to_vec(),
                            ),
                        )
                        .map_err(|error| IpcFailure::context_attachment(&error))
                    })
                    .await?;
                operation.publish(&importing, prepared)
            });
            started.await.unwrap();
            {
                let _admission = state
                    .application
                    .try_lock()
                    .expect("conversion cannot hold application admission");
                let mut live = state
                    .session
                    .try_lock()
                    .expect("conversion cannot hold the editor lock");
                let store = require_bound_store(&mut live, &project, &session).unwrap();
                assert_eq!(store.read_document(INITIAL_DOCUMENT).unwrap().text, "");
            }
            state.imports.cancel(&session, &operation_id).unwrap();
            release.send(()).unwrap();
            assert!(task.await.unwrap().is_err());
            assert_eq!(published_count(temporary.path()), 0);
        });
    }

    #[test]
    fn project_close_joins_conversion_and_revokes_original_session() {
        let temporary = tempfile::tempdir().unwrap();
        let (state, project, session) = opened(temporary.path());
        tauri::async_runtime::block_on(async {
            let operation_id = CommandId::new().to_string();
            let operation =
                ImportOperation::reserve(&state, &project, &session, &operation_id).unwrap();
            let mut cancelled = operation.signal.subscribe();
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, blocked) = mpsc::channel();
            let task = tokio::spawn(async move {
                operation
                    .compute(move || {
                        entered.send(()).unwrap();
                        blocked.recv().unwrap();
                        Ok(())
                    })
                    .await
            });
            started.await.unwrap();
            let closing_state = Arc::clone(&state);
            let closing_session = session.clone();
            let close = std::thread::spawn(move || {
                close_project_with_wait(
                    &closing_state,
                    project,
                    closing_session,
                    CommandId::new(),
                    Duration::ZERO,
                )
            });
            cancelled.changed().await.unwrap();
            assert!(
                !close.is_finished(),
                "close must join the admitted converter"
            );
            release.send(()).unwrap();
            assert!(task.await.unwrap().is_err());
            close.join().unwrap().unwrap();
            let next = tempfile::tempdir().unwrap();
            let next_store = initialize_project(next.path(), "Next writing".into()).unwrap();
            let next_snapshot = reserve_project_choice(&state)
                .unwrap()
                .finish_without_document_filesystem_watcher(Ok(next_store))
                .unwrap();
            assert_ne!(next_snapshot.session_id, session);
            assert!(
                ImportOperation::reserve(
                    &state,
                    &next_snapshot.project_id,
                    &next_snapshot.session_id,
                    &CommandId::new().to_string()
                )
                .is_ok()
            );
            assert_eq!(published_count(next.path()), 0);
        });
    }

    #[test]
    fn abandoned_rpc_keeps_worker_owned_until_shutdown() {
        let temporary = tempfile::tempdir().unwrap();
        let (state, project, session) = opened(temporary.path());
        tauri::async_runtime::block_on(async {
            let operation_id = CommandId::new().to_string();
            let operation =
                ImportOperation::reserve(&state, &project, &session, &operation_id).unwrap();
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, blocked) = mpsc::channel();
            let task = tokio::spawn(async move {
                operation
                    .compute(move || {
                        entered.send(()).unwrap();
                        blocked.recv().unwrap();
                        Ok(())
                    })
                    .await
            });
            started.await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(
                ImportOperation::reserve(&state, &project, &session, &CommandId::new().to_string())
                    .is_err(),
                "abandoning the RPC must not allow unlimited concurrent converters"
            );
            let start = Arc::new(std::sync::Barrier::new(3));
            let (finished, completions) = mpsc::channel();
            let mut drains = Vec::new();
            for _ in 0..2 {
                let jobs = Arc::clone(&state.imports);
                let start = Arc::clone(&start);
                let finished = finished.clone();
                drains.push(std::thread::spawn(move || {
                    start.wait();
                    let count = jobs.shutdown().unwrap();
                    finished.send(()).unwrap();
                    count
                }));
            }
            start.wait();
            assert!(
                completions.recv_timeout(Duration::from_millis(50)).is_err(),
                "neither concurrent close may finish while the converter runs"
            );
            release.send(()).unwrap();
            assert_eq!(
                drains
                    .into_iter()
                    .map(|drain| drain.join().unwrap())
                    .sum::<usize>(),
                1
            );
            assert_eq!(published_count(temporary.path()), 0);
        });
    }

    #[test]
    fn network_cancel_drops_request_and_joins_worker() {
        let temporary = tempfile::tempdir().unwrap();
        let (state, project, session) = opened(temporary.path());
        tauri::async_runtime::block_on(async {
            let operation_id = CommandId::new().to_string();
            let operation =
                ImportOperation::reserve(&state, &project, &session, &operation_id).unwrap();
            let (entered, started) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                operation
                    .network(async move {
                        entered.send(()).unwrap();
                        std::future::pending::<Result<(), IpcFailure>>().await
                    })
                    .await
            });
            started.await.unwrap();
            state.imports.cancel(&session, &operation_id).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            assert_eq!(state.imports.shutdown().unwrap(), 1);
        });
    }

    #[test]
    fn direct_import_reports_partial_success_and_retry_reuses_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let (state, project, session) = opened(temporary.path());
        tauri::async_runtime::block_on(async {
            let source = temporary.path().join("source.txt");
            std::fs::write(&source, "Exact source.\r\n").unwrap();
            let operation_id = CommandId::new().to_string();
            let operation =
                ImportOperation::reserve(&state, &project, &session, &operation_id).unwrap();
            let paths = vec![
                source.to_string_lossy().into_owned(),
                temporary
                    .path()
                    .join("absent.txt")
                    .to_string_lossy()
                    .into_owned(),
            ];
            let first = crate::import_batch::import_paths(&operation, &state, paths.clone())
                .await
                .unwrap();
            assert_eq!(first.imported.len(), 1);
            assert_eq!(first.failures.len(), 1);
            let second = crate::import_batch::import_paths(&operation, &state, paths)
                .await
                .unwrap();
            assert_eq!(first.imported[0].id, second.imported[0].id);
            assert_eq!(published_count(temporary.path()), 1);
            let mut live = state.session.lock().unwrap();
            assert_eq!(
                require_bound_store(&mut live, &project, &session)
                    .unwrap()
                    .read_document(INITIAL_DOCUMENT)
                    .unwrap()
                    .text,
                ""
            );
        });
    }
}
