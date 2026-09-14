//! Signal's process is a supervised, opt-in service. Restarting it never replays
//! a command. Its persistent send ledger resolves repeated explicit requests.
use loom_signal_protocol::{
    Command, Event, PROTOCOL_VERSION, Request, Response, read_frame, write_frame,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::Command as ProcessCommand,
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const CAPACITY: usize = 32;
type Observer = Arc<dyn Fn(Event) + Send + Sync>;

struct Pending {
    request: Request,
    reply: oneshot::Sender<Event>,
}
struct Supervisor {
    requests: mpsc::Sender<Pending>,
    stop: CancellationToken,
    worker: JoinHandle<()>,
}

#[derive(Default)]
pub(crate) struct SignalService {
    supervisor: Mutex<Option<Supervisor>>,
    closed: AtomicBool,
}

impl std::fmt::Debug for SignalService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalService").finish_non_exhaustive()
    }
}

impl SignalService {
    pub async fn request(
        &self,
        executable: PathBuf,
        storage: PathBuf,
        observer: Observer,
        request: Request,
    ) -> Event {
        let requests = {
            let mut slot = self.supervisor.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return failure("signal_closed", "Signal is closing.", false);
            }
            if slot
                .as_ref()
                .is_some_and(|owner| owner.worker.is_finished())
            {
                slot.take();
            }
            let supervisor = slot.get_or_insert_with(|| {
                let (requests, receiver) = mpsc::channel(CAPACITY);
                let stop = CancellationToken::new();
                let worker_stop = stop.clone();
                let worker = tokio::spawn(supervise(
                    executable,
                    storage,
                    receiver,
                    observer,
                    worker_stop,
                ));
                Supervisor {
                    requests,
                    stop,
                    worker,
                }
            });
            supervisor.requests.clone()
        };
        let (reply, result) = oneshot::channel();
        if requests.try_send(Pending { request, reply }).is_err() {
            return failure(
                "signal_busy",
                "Signal is busy. No new message was sent.",
                true,
            );
        }
        match tokio::time::timeout(Duration::from_secs(40), result).await {
            Ok(Ok(event)) => event,
            _ => interrupted(),
        }
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(supervisor) = self.supervisor.lock().await.take() {
            supervisor.stop.cancel();
            let _ = supervisor.worker.await;
        }
    }
}

impl Drop for SignalService {
    fn drop(&mut self) {
        if let Some(supervisor) = self.supervisor.get_mut().take() {
            supervisor.stop.cancel();
        }
    }
}

async fn supervise(
    executable: PathBuf,
    storage: PathBuf,
    mut requests: mpsc::Receiver<Pending>,
    observer: Observer,
    stop: CancellationToken,
) {
    let mut backoff = 1_u64;
    loop {
        if stop.is_cancelled() {
            break;
        }
        let started = tokio::time::Instant::now();
        let result = process(&executable, &storage, &mut requests, &observer, &stop).await;
        if stop.is_cancelled() || requests.is_closed() {
            break;
        }
        if result
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::InvalidData)
        {
            break;
        }
        (observer)(failure(
            "signal_offline",
            "Signal disconnected. Reconnecting automatically.",
            true,
        ));
        if result.is_ok() || started.elapsed() > Duration::from_mins(1) {
            backoff = 1;
        }
        let deadline = tokio::time::sleep(Duration::from_secs(backoff));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = &mut deadline => break,
                pending = requests.recv() => {
                    let Some(pending) = pending else { return; };
                    let _ = pending.reply.send(failure("signal_offline", "Signal is reconnecting. Your request has not been sent.", true));
                }
            }
        }
        backoff = (backoff * 2).min(30);
    }
    while let Ok(pending) = requests.try_recv() {
        let _ = pending.reply.send(interrupted());
    }
}

async fn process(
    executable: &Path,
    storage: &Path,
    requests: &mut mpsc::Receiver<Pending>,
    observer: &Observer,
    stop: &CancellationToken,
) -> std::io::Result<()> {
    let mut child = ProcessCommand::new(executable)
        .arg("--store")
        .arg(storage)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("Signal input unavailable"))?;
    let mut output = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("Signal output unavailable"))?;
    let (events, mut events_rx) = mpsc::channel::<Response>(CAPACITY);
    let reader = tokio::spawn(async move {
        while let Ok(Some(response)) = read_frame::<Response>(&mut output).await {
            if events.send(response).await.is_err() {
                break;
            }
        }
    });
    let mut pending: BTreeMap<String, oneshot::Sender<Event>> = BTreeMap::new();
    let mut result = Ok(());
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            _ = child.wait() => break,
            response = events_rx.recv() => {
                let Some(response) = response else { break; };
                if matches!(response.event, Event::Stopped) { break; }
                if matches!(&response.event, Event::Status { status } if status.version != PROTOCOL_VERSION) {
                    (observer)(failure("signal_version_mismatch", "The bundled Signal worker has an incompatible protocol version.", false));
                    result = Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Signal protocol mismatch"));
                    break;
                }
                if let Some(id) = response.id {
                    if let Some(reply) = pending.remove(&id) { let _ = reply.send(response.event); }
                } else {
                    (observer)(response.event);
                }
            }
            command = requests.recv() => {
                let Some(command) = command else { break; };
                if pending.len() >= CAPACITY || pending.contains_key(&command.request.id) {
                    let _ = command.reply.send(failure("signal_busy", "This Signal request is already running, or Signal is busy.", true));
                    continue;
                }
                pending.insert(command.request.id.clone(), command.reply);
                if !matches!(tokio::time::timeout(Duration::from_secs(3), write_frame(&mut input, &command.request)).await, Ok(Ok(()))) { break; }
            }
        }
    }
    if stop.is_cancelled() {
        let _ = tokio::time::timeout(
            Duration::from_millis(250),
            write_frame(
                &mut input,
                &Request {
                    id: "shutdown".into(),
                    command: Command::Shutdown,
                },
            ),
        )
        .await;
    }
    // EOF releases Tokio's uncancellable stdin read after a worker failure.
    // Close our pipe before waiting, including when Stopped came unsolicited.
    drop(input);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    match child.try_wait() {
        Ok(Some(_)) => (),
        Ok(None) => {
            if let Err(error) = child.kill().await {
                result = Err(error);
            }
        }
        Err(error) => {
            let _ = child.kill().await;
            result = Err(error);
        }
    }
    let exit = child.wait().await;
    if result.is_ok() {
        result = match exit {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(std::io::Error::other("Signal worker exited unsuccessfully")),
            Err(error) => Err(error),
        };
    }
    reader.abort();
    let _ = reader.await;
    for (_, reply) in pending {
        let _ = reply.send(interrupted());
    }
    result
}

fn interrupted() -> Event {
    failure(
        "signal_interrupted",
        "Signal disconnected before returning a result. Check the conversation before retrying a send.",
        false,
    )
}

fn failure(code: &str, message: &str, retryable: bool) -> Event {
    Event::Failure {
        code: code.into(),
        message: message.into(),
        retryable,
    }
}

pub(crate) fn executable() -> std::io::Result<PathBuf> {
    let parent = std::env::current_exe()?
        .parent()
        .ok_or_else(|| std::io::Error::other("Loom executable has no directory"))?
        .to_path_buf();
    let filename = if cfg!(windows) {
        "loom-signal.exe"
    } else {
        "loom-signal"
    };
    let bundled = parent.join(filename);
    if bundled.is_file() {
        return Ok(bundled);
    }
    #[cfg(debug_assertions)]
    {
        let filename = format!(
            "loom-signal-{}{}",
            env!("LOOM_BUILD_TARGET"),
            if cfg!(windows) { ".exe" } else { "" }
        );
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/loom/src-tauri/binaries")
            .join(filename);
        if development.is_file() {
            return Ok(development);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "The bundled Signal client is missing. Run the Signal build step.",
    ))
}

#[tauri::command]
pub(crate) async fn signal_request<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    state: tauri::State<'_, super::PluginState>,
    request: Request,
) -> Result<Event, super::IpcFailure> {
    use tauri::Emitter;
    super::ensure_application_running(&state, "Signal work")?;
    if matches!(
        request.command,
        Command::Send { .. }
            | Command::Link { .. }
            | Command::CancelLink
            | Command::VerifyIdentity { .. }
    ) && !window.is_focused().unwrap_or(false)
    {
        return Err(super::IpcFailure::new(
            "signal_focus_required",
            "Return to Loom before linking, verifying a safety number, or sending a Signal message.",
            false,
        ));
    }
    let executable = executable().map_err(|error| {
        super::IpcFailure::new("signal_worker_missing", error.to_string(), false)
    })?;
    let storage = state
        .app_local_data_root
        .as_ref()
        .ok_or_else(|| {
            super::IpcFailure::new(
                "signal_storage_unavailable",
                "Signal requires an application data directory.",
                false,
            )
        })?
        .join("signal");
    let observer = Arc::new(move |event| {
        let _ = window.emit("loom://signal", event);
    });
    Ok(state
        .signal
        .request(executable, storage, observer, request)
        .await)
}

pub(crate) fn resume<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::{Emitter, Manager};
    let state = app.state::<super::PluginState>();
    let Some(root) = state.app_local_data_root.as_ref() else {
        return;
    };
    let storage = root.join("signal");
    if !storage.join("signal.db").is_file() {
        return;
    }
    let Ok(executable) = executable() else {
        return;
    };
    let signal = state.signal.clone();
    let app = app.clone();
    let observer = Arc::new(move |event| {
        let _ = app.emit_to("main", "loom://signal", event);
    });
    tauri::async_runtime::spawn(async move {
        let _ = signal
            .request(
                executable,
                storage,
                observer,
                Request {
                    id: "startup".into(),
                    command: Command::Status,
                },
            )
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failed_child_exit_remains_a_failure_for_restart_backoff() {
        // The Rust test harness rejects the worker's --store argument. This
        // exercises a real nonzero child exit through the supervisor adapter.
        let (_sender, mut requests) = mpsc::channel(1);
        let observer: Observer = Arc::new(|_| {});
        let result = process(
            &std::env::current_exe().expect("test executable"),
            Path::new("unused-test-store"),
            &mut requests,
            &observer,
            &CancellationToken::new(),
        )
        .await;
        assert!(
            result.is_err(),
            "a failed worker must not reset restart backoff"
        );
    }
}
