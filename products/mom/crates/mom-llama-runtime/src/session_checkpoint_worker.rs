use crate::operation_scope::WeakOperationScope;
use crate::{OperationScope, kv_cache::SessionCheckpointProgress};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_PENDING_CONVERSATIONS: usize = 64;
const DEBOUNCE: Duration = Duration::from_secs(3);
const DEFER_RETRY: Duration = Duration::from_secs(3);
const ERROR_RETRY_MAX: Duration = Duration::from_secs(30);

type Runner = dyn Fn(&OperationScope, &str, &str, &mut Option<SessionCheckpointProgress>) -> Result<bool>
    + Send
    + Sync;

struct PendingCheckpoint {
    request_id: String,
    deadline: Instant,
    failures: u8,
}

struct WorkerState {
    pending: Mutex<PendingState>,
    wake: Condvar,
    weak_scope: WeakOperationScope,
    runner: Arc<Runner>,
}

struct PendingState {
    entries: HashMap<String, PendingCheckpoint>,
    in_flight: Option<String>,
    stopped: bool,
    spawn_error: Option<String>,
    last_error: Option<String>,
    join_error: Option<String>,
}

/// One scheduler is shared by the root scope and all of its dispatch children.
/// Queue entries contain identities only: the worker never retains an owner.
pub(crate) struct SessionCheckpointScheduler {
    state: Arc<WorkerState>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl SessionCheckpointScheduler {
    pub(crate) fn new(weak_scope: WeakOperationScope) -> Self {
        Self::with_runner(
            weak_scope,
            |scope, conversation_id, request_id, progress| {
                crate::kv_cache::run_scheduled_session_checkpoint(
                    scope,
                    conversation_id,
                    request_id,
                    progress,
                )
            },
        )
    }

    fn with_runner(
        weak_scope: WeakOperationScope,
        runner: impl Fn(
            &OperationScope,
            &str,
            &str,
            &mut Option<SessionCheckpointProgress>,
        ) -> Result<bool>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            state: Arc::new(WorkerState {
                pending: Mutex::new(PendingState {
                    entries: HashMap::new(),
                    in_flight: None,
                    stopped: false,
                    spawn_error: None,
                    last_error: None,
                    join_error: None,
                }),
                wake: Condvar::new(),
                weak_scope,
                runner: Arc::new(runner),
            }),
            worker: Mutex::new(None),
        }
    }

    pub(crate) fn schedule(&self, conversation_id: &str, request_id: &str) -> Result<()> {
        self.schedule_after(conversation_id, request_id, DEBOUNCE)
    }

    fn schedule_after(
        &self,
        conversation_id: &str,
        request_id: &str,
        delay: Duration,
    ) -> Result<()> {
        if conversation_id.is_empty() || request_id.is_empty() {
            return Err(anyhow!("session checkpoint identity is empty"));
        }
        self.ensure_worker()?;
        let mut pending = self
            .state
            .pending
            .lock()
            .map_err(|_| anyhow!("session checkpoint scheduler is unavailable"))?;
        if pending.stopped {
            return Err(anyhow!("session checkpoint scheduler is stopped"));
        }
        let known_conversation = pending.entries.contains_key(conversation_id)
            || pending.in_flight.as_deref() == Some(conversation_id);
        if !known_conversation
            && pending.entries.len() + usize::from(pending.in_flight.is_some())
                >= MAX_PENDING_CONVERSATIONS
        {
            return Err(anyhow!("session checkpoint queue is full"));
        }
        pending.entries.insert(
            conversation_id.to_owned(),
            PendingCheckpoint {
                request_id: request_id.to_owned(),
                deadline: Instant::now() + delay,
                failures: 0,
            },
        );
        self.state.wake.notify_one();
        Ok(())
    }

    pub(crate) fn stop(&self) {
        if let Ok(mut pending) = self.state.pending.lock() {
            pending.stopped = true;
            pending.entries.clear();
            self.state.wake.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.state
            .pending
            .lock()
            .map_or(0, |pending| pending.entries.len())
    }

    pub(crate) fn join(&self) -> Result<()> {
        let stopped = self
            .state
            .pending
            .lock()
            .map_err(|_| anyhow!("session checkpoint scheduler is unavailable"))?
            .stopped;
        if !stopped {
            return Err(anyhow!(
                "session checkpoint scheduler must be stopped before join"
            ));
        }
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| anyhow!("session checkpoint worker handle is unavailable"))?;
        if let Some(handle) = worker.as_ref() {
            if handle.thread().id() == thread::current().id() {
                // If the last scope is released by the callback itself, stop
                // wakes the worker and it will return immediately after this
                // callback. Explicit owner shutdown joins from its own thread.
                return Ok(());
            }
            let handle = worker.take().expect("worker handle was present");
            if handle.join().is_err()
                && let Ok(mut pending) = self.state.pending.lock()
            {
                pending.join_error = Some("session checkpoint worker panicked".to_owned());
            }
        }
        let (spawn_error, join_error) = self
            .state
            .pending
            .lock()
            .ok()
            .map(|pending| (pending.spawn_error.clone(), pending.join_error.clone()))
            .unwrap_or_default();
        if let Some(error) = spawn_error {
            return Err(anyhow!(
                "session checkpoint worker failed to start: {error}"
            ));
        }
        if let Some(error) = join_error {
            return Err(anyhow!("{error}"));
        }
        Ok(())
    }

    fn ensure_worker(&self) -> Result<()> {
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| anyhow!("session checkpoint worker handle is unavailable"))?;
        if worker.is_some() {
            return Ok(());
        }
        let mut pending = self
            .state
            .pending
            .lock()
            .map_err(|_| anyhow!("session checkpoint scheduler is unavailable"))?;
        if pending.stopped {
            return Err(anyhow!("session checkpoint scheduler is stopped"));
        }
        if let Some(error) = &pending.spawn_error {
            return Err(anyhow!(
                "session checkpoint worker failed to start: {error}"
            ));
        }
        let state = Arc::clone(&self.state);
        match thread::Builder::new()
            .name("mom-session-checkpoint-idle".to_owned())
            .spawn(move || worker_loop(state))
        {
            Ok(handle) => {
                *worker = Some(handle);
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                pending.spawn_error = Some(message.clone());
                Err(anyhow!(
                    "session checkpoint worker failed to start: {message}"
                ))
            }
        }
    }
}

impl Drop for SessionCheckpointScheduler {
    fn drop(&mut self) {
        self.stop();
        let _ = self.join();
    }
}

struct DueCheckpoint {
    conversation_id: String,
    request_id: String,
    failures: u8,
}

fn worker_loop(state: Arc<WorkerState>) {
    let mut progress: Option<SessionCheckpointProgress> = None;
    let mut progress_conversation: Option<String> = None;
    loop {
        let due = match take_due(&state, progress_conversation.as_deref()) {
            Some(due) => due,
            None => return,
        };
        if progress_conversation.as_deref() != Some(due.conversation_id.as_str()) {
            progress = None;
            progress_conversation = None;
        }
        let Some(scope) = state.weak_scope.upgrade() else {
            return;
        };
        if scope.cancellation_requested() {
            return;
        }
        let result = (state.runner)(&scope, &due.conversation_id, &due.request_id, &mut progress);
        drop(scope);
        let (completed, retry_after, error) = match result {
            Ok(true) => (true, None, None),
            Ok(false) => (false, Some(DEFER_RETRY), None),
            Err(error) => (
                false,
                Some(error_backoff(due.failures)),
                Some(error.to_string()),
            ),
        };
        if completed {
            progress = None;
            progress_conversation = None;
        } else if progress.is_some() {
            progress_conversation = Some(due.conversation_id.clone());
        }
        finish_due(&state, due, completed, retry_after, error);
        refill_pending_from_registry(&state);
    }
}

/// Fill newly available bounded queue slots from committed runtime state.
/// Scope ownership is released before locking scheduler state so a last-owner
/// drop can never happen while holding the pending mutex.
fn refill_pending_from_registry(state: &WorkerState) {
    let Some(scope) = state.weak_scope.upgrade() else {
        return;
    };
    if scope.cancellation_requested() {
        return;
    }
    let Ok(requests) = scope.committed_session_requests() else {
        return;
    };
    drop(scope);

    let Ok(mut pending) = state.pending.lock() else {
        return;
    };
    if pending.stopped {
        return;
    }
    let mut available = MAX_PENDING_CONVERSATIONS
        .saturating_sub(pending.entries.len() + usize::from(pending.in_flight.is_some()));
    if available == 0 {
        return;
    }
    let mut inserted = false;
    for (conversation_id, request_id) in requests {
        if pending.entries.contains_key(&conversation_id)
            || pending.in_flight.as_deref() == Some(conversation_id.as_str())
        {
            continue;
        }
        pending.entries.insert(
            conversation_id,
            PendingCheckpoint {
                request_id,
                deadline: Instant::now(),
                failures: 0,
            },
        );
        inserted = true;
        available -= 1;
        if available == 0 {
            break;
        }
    }
    if inserted {
        state.wake.notify_one();
    }
}

fn take_due(state: &WorkerState, priority_conversation: Option<&str>) -> Option<DueCheckpoint> {
    let Ok(mut pending) = state.pending.lock() else {
        return None;
    };
    loop {
        if pending.stopped {
            return None;
        }
        if pending.entries.is_empty() {
            pending = state.wake.wait(pending).ok()?;
            continue;
        }
        let priority = priority_conversation.and_then(|key| pending.entries.get(key));
        let next_deadline = priority
            .map(|entry| entry.deadline)
            .or_else(|| pending.entries.values().map(|entry| entry.deadline).min())?;
        let now = Instant::now();
        if next_deadline > now {
            let (next, _) = state.wake.wait_timeout(pending, next_deadline - now).ok()?;
            pending = next;
            continue;
        }
        let conversation_id = if priority.is_some() {
            priority_conversation?.to_owned()
        } else {
            pending
                .entries
                .iter()
                .filter(|(_, entry)| entry.deadline <= now)
                .min_by_key(|(_, entry)| entry.deadline)
                .map(|(conversation_id, _)| conversation_id.clone())?
        };
        let entry = pending.entries.remove(&conversation_id)?;
        pending.in_flight = Some(conversation_id.clone());
        return Some(DueCheckpoint {
            conversation_id,
            request_id: entry.request_id,
            failures: entry.failures,
        });
    }
}

fn finish_due(
    state: &WorkerState,
    due: DueCheckpoint,
    completed: bool,
    retry_after: Option<Duration>,
    error: Option<String>,
) {
    let Ok(mut pending) = state.pending.lock() else {
        return;
    };
    pending.in_flight = None;
    if pending.stopped {
        return;
    }
    let had_error = error.is_some();
    if completed {
        pending.last_error = None;
    } else if let Some(error) = error
        && pending.last_error.as_deref() != Some(error.as_str())
    {
        eprintln!("Session checkpoint attempt failed; retry scheduled: {error}");
        pending.last_error = Some(error);
    }
    if let Some(retry_after) = retry_after {
        if let Some(newer) = pending.entries.get_mut(&due.conversation_id) {
            if newer.request_id == due.request_id {
                newer.failures = if completed {
                    0
                } else if had_error {
                    due.failures.saturating_add(1)
                } else {
                    due.failures
                };
                newer.deadline = Instant::now() + retry_after;
            }
        } else if pending.entries.len() < MAX_PENDING_CONVERSATIONS {
            pending.entries.insert(
                due.conversation_id,
                PendingCheckpoint {
                    request_id: due.request_id,
                    deadline: Instant::now() + retry_after,
                    failures: if had_error {
                        due.failures.saturating_add(1)
                    } else {
                        due.failures
                    },
                },
            );
        }
    } else if let Some(newer) = pending.entries.get(&due.conversation_id)
        && newer.request_id == due.request_id
    {
        pending.entries.remove(&due.conversation_id);
    }
    state.wake.notify_one();
}

fn error_backoff(failures: u8) -> Duration {
    let multiplier = 1_u32 << u32::from(failures.min(5));
    DEFER_RETRY.saturating_mul(multiplier).min(ERROR_RETRY_MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        DueCheckpoint, PendingCheckpoint, PendingState, SessionCheckpointScheduler, WorkerState,
        error_backoff, finish_due, refill_pending_from_registry, take_due,
    };
    use crate::OperationScope;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn worker_runs_only_after_scheduled_deadline_and_root_shutdown_joins_it() {
        let root = OperationScope::detached();
        let (finished_tx, finished_rx) = mpsc::channel();
        let scheduler =
            SessionCheckpointScheduler::with_runner(root.downgrade(), move |_, _, _, _| {
                finished_tx.send(()).expect("report completion");
                Ok(true)
            });
        scheduler
            .schedule_after("conversation", "request", Duration::ZERO)
            .expect("test runner schedule");
        finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker invoked");
        root.request_cancellation();
        scheduler.stop();
        scheduler.join().expect("join test scheduler");
    }

    #[test]
    fn worker_refills_overflow_from_committed_requests_and_never_after_stop() {
        const REQUESTS: usize = 70;
        let root = OperationScope::detached();
        for index in 0..REQUESTS {
            root.note_committed_session_request(
                &format!("conversation-{index:03}"),
                &format!("request-{index:03}"),
            )
            .expect("register committed request");
        }

        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (finished_tx, finished_rx) = mpsc::channel();
        let scheduler = SessionCheckpointScheduler::with_runner(
            root.downgrade(),
            move |scope, conversation, request, _| {
                if conversation == "conversation-000" {
                    started_tx.send(()).expect("report first attempt");
                    release_rx
                        .lock()
                        .expect("release receiver")
                        .recv()
                        .expect("release first attempt");
                }
                scope
                    .retire_committed_session_request(conversation, request)
                    .expect("retire completed request");
                finished_tx
                    .send(conversation.to_owned())
                    .expect("report completion");
                Ok(true)
            },
        );

        scheduler
            .schedule_after("conversation-000", "request-000", Duration::ZERO)
            .expect("schedule first request");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first request is in flight");
        for index in 1..64 {
            scheduler
                .schedule_after(
                    &format!("conversation-{index:03}"),
                    &format!("request-{index:03}"),
                    Duration::ZERO,
                )
                .expect("fill bounded queue");
        }
        for index in 64..REQUESTS {
            assert!(
                scheduler
                    .schedule_after(
                        &format!("conversation-{index:03}"),
                        &format!("request-{index:03}"),
                        Duration::ZERO,
                    )
                    .is_err(),
                "overflow request should remain in the committed registry"
            );
        }

        release_tx.send(()).expect("release first request");
        let mut completed = std::collections::BTreeSet::new();
        for _ in 0..REQUESTS {
            completed.insert(
                finished_rx
                    .recv_timeout(Duration::from_secs(3))
                    .expect("overflow request is eventually refilled"),
            );
        }
        assert_eq!(completed.len(), REQUESTS);

        scheduler.stop();
        root.note_committed_session_request("late-conversation", "late-request")
            .expect("retain late committed identity for cancellation test");
        refill_pending_from_registry(&scheduler.state);
        assert_eq!(scheduler.pending_count(), 0, "stop must prevent refill");
        scheduler.join().expect("join stopped test scheduler");
    }

    #[test]
    fn same_conversation_update_during_checkpoint_is_not_lost() {
        let root = OperationScope::detached();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_runner = Arc::clone(&seen);
        let scheduler =
            SessionCheckpointScheduler::with_runner(root.downgrade(), move |_, _, request, _| {
                seen_runner
                    .lock()
                    .expect("requests")
                    .push(request.to_owned());
                if request == "old" {
                    started_tx.send(()).expect("started");
                    release_rx
                        .lock()
                        .expect("receiver")
                        .recv()
                        .expect("release");
                }
                Ok(true)
            });
        scheduler
            .schedule_after("conversation", "old", Duration::ZERO)
            .expect("schedule first");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first callback started");
        scheduler
            .schedule_after("conversation", "new", Duration::ZERO)
            .expect("coalesce newer request");
        release_tx.send(()).expect("release first callback");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while seen.lock().expect("requests").len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            *seen.lock().expect("requests"),
            vec!["old".to_owned(), "new".to_owned()]
        );
        root.request_cancellation();
        root.join_session_checkpoints().expect("join");
    }

    #[test]
    fn concurrent_joins_do_not_report_success_before_worker_exit() {
        let root = OperationScope::detached();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let scheduler = Arc::new(SessionCheckpointScheduler::with_runner(
            root.downgrade(),
            move |_, _, _, _| {
                started_tx.send(()).expect("started");
                release_rx
                    .lock()
                    .expect("receiver")
                    .recv()
                    .expect("release");
                Ok(true)
            },
        ));
        scheduler
            .schedule_after("conversation", "request", Duration::ZERO)
            .expect("schedule");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started");
        scheduler.stop();

        let (first_done_tx, first_done_rx) = mpsc::channel();
        let first_scheduler = Arc::clone(&scheduler);
        let first = std::thread::spawn(move || {
            first_done_tx
                .send(first_scheduler.join())
                .expect("first result");
        });
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second_scheduler = Arc::clone(&scheduler);
        let second = std::thread::spawn(move || {
            second_done_tx
                .send(second_scheduler.join())
                .expect("second result");
        });
        assert!(
            second_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );
        release_tx.send(()).expect("release worker");
        first_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first join completed")
            .expect("first join succeeded");
        second_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second join completed")
            .expect("second join succeeded");
        first.join().expect("first join thread");
        second.join().expect("second join thread");
    }

    #[test]
    fn retry_preserves_request_and_errors_back_off_boundedly() {
        assert_eq!(error_backoff(0), Duration::from_secs(3));
        assert_eq!(error_backoff(1), Duration::from_secs(6));
        assert_eq!(error_backoff(20), Duration::from_secs(30));
        let scope = OperationScope::detached();
        let state = WorkerState {
            pending: Mutex::new(PendingState {
                entries: Default::default(),
                in_flight: Some("conversation".to_owned()),
                stopped: false,
                spawn_error: None,
                last_error: None,
                join_error: None,
            }),
            wake: std::sync::Condvar::new(),
            weak_scope: scope.downgrade(),
            runner: Arc::new(|_, _, _, _| Ok(false)),
        };
        finish_due(
            &state,
            DueCheckpoint {
                conversation_id: "conversation".to_owned(),
                request_id: "request".to_owned(),
                failures: 0,
            },
            false,
            Some(error_backoff(0)),
            Some("transient persistence failure".to_owned()),
        );
        let pending = state.pending.lock().expect("pending");
        assert_eq!(pending.entries.len(), 1);
        assert_eq!(pending.entries["conversation"].request_id, "request");
        assert_eq!(pending.entries["conversation"].failures, 1);
        assert!(
            pending.entries["conversation"].deadline > Instant::now() + Duration::from_secs(2),
            "error retries must wait for the bounded backoff deadline"
        );
        assert_eq!(
            pending.last_error.as_deref(),
            Some("transient persistence failure")
        );
    }

    #[test]
    fn yielded_snapshot_conversation_has_priority_over_other_due_work() {
        let scope = OperationScope::detached();
        let now = std::time::Instant::now();
        let state = WorkerState {
            pending: Mutex::new(PendingState {
                entries: [
                    (
                        "partial".to_owned(),
                        PendingCheckpoint {
                            request_id: "request-a".to_owned(),
                            deadline: now,
                            failures: 0,
                        },
                    ),
                    (
                        "other".to_owned(),
                        PendingCheckpoint {
                            request_id: "request-b".to_owned(),
                            deadline: now,
                            failures: 0,
                        },
                    ),
                ]
                .into_iter()
                .collect(),
                in_flight: None,
                stopped: false,
                spawn_error: None,
                last_error: None,
                join_error: None,
            }),
            wake: std::sync::Condvar::new(),
            weak_scope: scope.downgrade(),
            runner: Arc::new(|_, _, _, _| Ok(true)),
        };
        let due = take_due(&state, Some("partial")).expect("prioritized work is due");
        assert_eq!(due.conversation_id, "partial");
        assert_eq!(due.request_id, "request-a");
    }
}
