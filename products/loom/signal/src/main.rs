//! Signal runs as an owned worker so its cryptographic and SQLite dependencies
//! and lifecycle remain independent of the manuscript and inference runtime.
#![forbid(unsafe_code)]

mod client;
mod drafts;
mod messages;
mod retention;
mod vault;

use loom_signal_protocol::{Event, Request, Response, read_frame, write_frame};
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
    let input_stop = stop.clone();
    let reader = tokio::task::spawn_local(async move {
        let mut input = tokio::io::stdin();
        loop {
            let frame = tokio::select! {
                () = input_stop.cancelled() => break,
                frame = read_frame::<Request>(&mut input) => frame,
            };
            match frame {
                Ok(Some(request)) => {
                    if requests_tx.send(request).await.is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        input_stop.cancel();
    });
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
        Ok(vault) => client::run(vault, requests, responses.clone(), stop.clone()).await,
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
