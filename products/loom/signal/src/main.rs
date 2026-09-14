//! Signal runs as an owned worker so its cryptographic and SQLite dependencies
//! and lifecycle remain independent of the manuscript and inference runtime.
#![forbid(unsafe_code)]

mod client;
mod drafts;
mod identity;
mod messages;
mod retention;
mod vault;
mod workspaces;

use loom_signal_protocol::{Command, Event, Request, Response, read_frame, write_frame};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    // Upstream provisioning traces contain secret link URLs. Do not install a
    // tracing subscriber; protocol failures deliberately omit upstream details.
    match tokio::task::LocalSet::new().run_until(run()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("Loom Signal stopped; its encrypted data has been preserved.");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn read_requests(requests: mpsc::Sender<Request>, stop: CancellationToken) {
    let mut input = tokio::io::stdin();
    loop {
        let frame = tokio::select! {
            () = stop.cancelled() => break,
            frame = read_frame::<Request>(&mut input) => frame,
        };
        let Ok(Some(request)) = frame else {
            break;
        };
        let shutdown = matches!(request.command, Command::Shutdown);
        let sent = tokio::select! {
            () = stop.cancelled() => break,
            sent = requests.send(request) => sent,
        };
        if sent.is_err() {
            break;
        }
        if shutdown {
            // Tokio stdin uses an uncancellable blocking read. Do not start
            // another read after forwarding the terminal command. The client
            // processes it in order and owns cancellation of the session.
            return;
        }
    }
    stop.cancel();
}

async fn run() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    anyhow::ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--store")),
        "Expected --store"
    );
    let directory = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("Missing store"))?,
    );
    anyhow::ensure!(args.next().is_none(), "Unexpected arguments");
    let stop = CancellationToken::new();
    let (requests_tx, requests) = mpsc::channel(32);
    let (responses, mut responses_rx) = mpsc::channel::<Response>(64);
    let reader = tokio::task::spawn_local(read_requests(requests_tx, stop.clone()));
    let output_stop = stop.clone();
    let mut writer = tokio::task::spawn_local(async move {
        let mut output = tokio::io::stdout();
        while let Some(response) = responses_rx.recv().await {
            if write_frame(&mut output, &response).await.is_err() {
                break;
            }
        }
        output_stop.cancel();
    });
    let result = match vault::Vault::open(&directory).await {
        Ok(vault) => {
            let result = client::run(&vault, requests, responses.clone(), stop.clone()).await;
            vault.close().await;
            result
        }
        Err(_) => {
            let _ = responses.send(Response { id: None, event: Event::Failure {
                code: "vault_unavailable".into(),
                message: "Signal could not open its encrypted store. Unlock the system keychain and ensure this profile is not already open.".into(),
                retryable: true,
            }}).await;
            Err(anyhow::anyhow!("Signal vault unavailable"))
        }
    };
    stop.cancel();
    reader.abort();
    let _ = reader.await;
    // Signal tasks have joined and the vault has closed before the parent is
    // told to retire this process. On failure the parent closes stdin, also
    // releasing any blocking read that was in flight when its task was aborted.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        responses.send(Response {
            id: None,
            event: Event::Stopped,
        }),
    )
    .await;
    drop(responses);
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        process::Stdio,
        time::{Duration, Instant},
    };

    #[test]
    fn shutdown_releases_the_real_stdin_reader_without_parent_eof() {
        const CHILD: &str = "LOOM_SIGNAL_STDIN_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (sender, mut requests) = mpsc::channel(2);
                let stop = CancellationToken::new();
                read_requests(sender, stop.clone()).await;
                assert!(matches!(
                    requests.recv().await.unwrap().command,
                    Command::Status
                ));
                assert!(matches!(
                    requests.recv().await.unwrap().command,
                    Command::Shutdown
                ));
                assert!(
                    !stop.is_cancelled(),
                    "the client owns the forwarded shutdown"
                );
            });
            drop(runtime); // This hung while stdin scheduled one more read.
            return;
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::shutdown_releases_the_real_stdin_reader_without_parent_eof",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        for command in [Command::Status, Command::Shutdown] {
            let bytes = serde_json::to_vec(&Request {
                id: "test".into(),
                command,
            })
            .unwrap();
            input
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .unwrap();
            input.write_all(&bytes).unwrap();
        }
        input.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("the worker waited for stdin EOF after its terminal command");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(input);
    }
}
