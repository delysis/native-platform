use crate::{messages, retention, vault::Vault};
use anyhow::Result;
use base64::Engine;
use futures::{StreamExt, channel::oneshot};
use loom_signal_protocol::{
    Command, Event, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Phase, Request, Response, Status,
};
use presage::{
    Manager,
    libsignal_service::{configuration::SignalServers, protocol::ServiceId},
    manager::Registered,
    model::messages::Received,
    proto::{DataMessage, GroupContextV2},
    store::{ContentsStore, StateStore, Thread},
};
use presage_store_sqlite::SqliteStore;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

type Client = Manager<SqliteStore, Registered>;

#[derive(Clone)]
struct Output {
    sender: mpsc::Sender<Response>,
    stop: CancellationToken,
}

pub async fn run(
    vault: &Vault,
    mut requests: mpsc::Receiver<Request>,
    output: mpsc::Sender<Response>,
    stop: CancellationToken,
) -> Result<()> {
    crate::identity::apply_approved(&vault.database).await?;
    retention::sweep(&vault.database).await?;
    let output = Output {
        sender: output,
        stop: stop.clone(),
    };
    let mut manager = registered_client(&vault.store).await?;
    let (phase_tx, phase) = watch::channel(if manager.is_some() {
        Phase::Connecting
    } else {
        Phase::Unlinked
    });
    let mut receiver = manager.as_ref().map(|client| {
        start_receiver(
            client.clone(),
            vault.database.clone(),
            output.clone(),
            phase_tx.clone(),
            stop.clone(),
        )
    });
    let mut linking: Option<JoinHandle<Result<Client, ()>>> = None;
    let mut expiry = tokio::time::interval(Duration::from_secs(10));
    let initial_phase = *phase.borrow();
    emit(&output, None, status(manager.as_ref(), initial_phase)).await;
    // Every fallible exit from the session joins its owned tasks. In
    // particular, a disk failure must not detach the message receiver.
    let result: Result<()> = async {
      loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            linked = async {
                match linking.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            } => {
                linking = None;
                // Provisioning may have committed registration before its
                // final network step failed or timed out. That device remains
                // ours and must be resumed instead of replaced by a new link.
                let linked = match linked {
                    Ok(Ok(client)) => Some(client),
                    Ok(Err(())) | Err(_) => registered_client(&vault.store).await?,
                };
                match linked {
                    Some(client) => {
                        phase_tx.send_replace(Phase::Connecting);
                        receiver = Some(start_receiver(client.clone(), vault.database.clone(), output.clone(), phase_tx.clone(), stop.clone()));
                        manager = Some(client);
                    }
                    None => {
                        phase_tx.send_replace(Phase::Unlinked);
                        emit(&output, None, failure("link_failed", "Signal linking did not complete. You can try again.", true)).await;
                    }
                }
            }
            _ = async {
                match receiver.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            } => {
                receiver = None;
                // Let the process supervisor reopen the same encrypted
                // device after a receiver panic, rather than remain falsely
                // connected with no task receiving messages.
                anyhow::bail!("Signal receiver stopped unexpectedly");
            }
            _ = expiry.tick() => {
                if retention::sweep(&vault.database).await? {
                    emit(&output, None, Event::Changed { conversation_id: None }).await;
                }
            }
            request = requests.recv() => {
                let Some(Request { id, command }) = request else { break; };
                if id.is_empty() || id.len() > 128 || !id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)) {
                    emit(&output, None, failure("invalid_request", "Invalid Signal request identity.", false)).await;
                    continue;
                }
                match command {
                    Command::Shutdown => break,
                    Command::Status => {
                        let observed = *phase.borrow();
                        emit(&output, Some(id), status(manager.as_ref(), observed)).await;
                    }
                    Command::CancelLink => {
                        if let Some(task) = linking.take() { task.abort(); let _ = task.await; }
                        // Cancellation can race the durable registration commit.
                        // Recover that committed device instead of advertising a
                        // fresh link and accidentally replacing its credentials.
                        if manager.is_none() {
                            manager = registered_client(&vault.store).await?;
                            if let Some(client) = &manager {
                                receiver = Some(start_receiver(client.clone(), vault.database.clone(), output.clone(), phase_tx.clone(), stop.clone()));
                                phase_tx.send_replace(Phase::Connecting);
                            } else { phase_tx.send_replace(Phase::Unlinked); }
                        }
                        let observed = *phase.borrow();
                        emit(&output, Some(id), status(manager.as_ref(), observed)).await;
                    }
                    Command::Link { device_name } => {
                        if manager.is_some() || linking.is_some() || device_name.trim().is_empty() || device_name.len() > 64 {
                            emit(&output, Some(id), failure("link_unavailable", "Signal is already linked or linking, or the device name is invalid.", false)).await;
                            continue;
                        }
                        phase_tx.send_replace(Phase::Linking);
                        let store = vault.store.clone();
                        let output = output.clone();
                        linking = Some(tokio::task::spawn_local(async move {
                            let (link_tx, link_rx) = oneshot::channel::<url::Url>();
                            let provision = async {
                                match link_rx.await {
                                    Ok(url) => {
                                        let event = match qrcode::QrCode::new(url.as_str()) {
                                            Ok(code) => {
                                                let svg = code.render::<qrcode::render::svg::Color>().min_dimensions(256, 256).build();
                                                Event::Link { url: url::Url::to_string(&url), qr_code: format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)) }
                                            }
                                            Err(_) => failure("link_failed", "Signal could not render this link.", true),
                                        };
                                        emit(&output, Some(id), event).await;
                                    }
                                    Err(_) => emit(&output, Some(id), failure("link_failed", "Signal could not create a link. Try again.", true)).await,
                                }
                            };
                            let registration = Manager::link_secondary_device(store, SignalServers::Production, device_name, link_tx);
                            tokio::time::timeout(Duration::from_secs(180), async {
                                let (result, ()) = futures::future::join(registration, provision).await;
                                result.map_err(|_| ())
                            }).await.unwrap_or(Err(()))
                        }));
                    }
                    command => {
                        let observed = *phase.borrow();
                        let result = handle(vault, manager.as_mut(), observed, &id, command).await;
                        let restart = matches!(&result, Event::Identity { review: Some(review), .. } if review.state == loom_signal_protocol::IdentityState::Pending);
                        emit(&output, Some(id), result).await;
                        // Retire this process before applying a reviewed key.
                        // Upstream receive/sync work may retain old sessions.
                        // The supervisor waits for exit, then reopens our vault.
                        if restart { break; }
                    }
                }
            }
        }
      }
      Ok(())
    }.await;
    stop.cancel();
    if let Some(task) = linking {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = receiver.take() {
        let _ = task.await;
    }
    result
}

async fn registered_client(store: &SqliteStore) -> Result<Option<Client>> {
    if store.load_registration_data().await?.is_some() {
        Ok(Some(Manager::load_registered(store.clone()).await?))
    } else {
        Ok(None)
    }
}

fn status(manager: Option<&Client>, phase: Phase) -> Event {
    Event::Status {
        status: Status {
            version: PROTOCOL_VERSION,
            phase,
            account_id: manager.map(|client| {
                ServiceId::from(client.registration_data().service_ids.aci()).service_id_string()
            }),
            device_name: manager.and_then(|client| client.registration_data().device_name.clone()),
        },
    }
}

async fn emit(output: &Output, id: Option<String>, event: Event) {
    tokio::select! {
        biased;
        () = output.stop.cancelled() => (),
        sent = output.sender.send(Response { id, event }) => {
            if sent.is_err() { output.stop.cancel(); }
        }
    }
}

fn failure(code: &str, message: &str, retryable: bool) -> Event {
    Event::Failure {
        code: code.into(),
        message: message.into(),
        retryable,
    }
}

async fn handle(
    vault: &Vault,
    manager: Option<&mut Client>,
    phase: Phase,
    request_id: &str,
    command: Command,
) -> Event {
    let Some(manager) = manager else {
        return failure("unlinked", "Link Signal from your phone first.", false);
    };
    match command {
        Command::Identity {
            conversation_id,
            recipient_id,
            refresh,
        } => {
            match crate::identity::inspect(
                vault,
                manager,
                &conversation_id,
                recipient_id.as_deref(),
                refresh,
            )
            .await
            {
                Ok(event) => event,
                Err(_) => failure(
                    "identity_unavailable",
                    "The safety number could not be read. Reconnect and refresh it before verifying.",
                    true,
                ),
            }
        }
        Command::VerifyIdentity {
            conversation_id,
            recipient_id,
            review_id,
        } => {
            match crate::identity::approve(vault, &conversation_id, &recipient_id, &review_id).await
            {
                Ok(event) => event,
                Err(_) => failure(
                    "identity_review_stale",
                    "This safety-number review is no longer current. Refresh and compare it again.",
                    true,
                ),
            }
        }
        Command::Workspaces { conversation_id } => {
            let result = async {
                messages::resolve(&vault.store, &conversation_id).await?;
                crate::workspaces::load(&vault.database, &conversation_id).await
            }
            .await;
            match result {
                Ok(links) => Event::Workspaces {
                    conversation_id,
                    links,
                },
                Err(_) => failure(
                    "workspace_read_failed",
                    "This conversation's saved workspaces could not be read.",
                    true,
                ),
            }
        }
        Command::UpdateWorkspace {
            conversation_id,
            expected_version,
            workspace_id,
            title,
        } => {
            let result = async {
                messages::resolve(&vault.store, &conversation_id).await?;
                let remove = title.is_none();
                crate::workspaces::update(
                    &vault.database,
                    &conversation_id,
                    request_id,
                    expected_version,
                    loom_signal_protocol::Workspace {
                        id: workspace_id,
                        title: title.unwrap_or_default(),
                    },
                    remove,
                )
                .await
            }
            .await;
            match result {
                Ok(links) => Event::Workspaces {
                    conversation_id,
                    links,
                },
                Err(_) => failure(
                    "workspace_save_failed",
                    "Workspace links could not be saved. Refresh this conversation before retrying.",
                    true,
                ),
            }
        }
        Command::Draft { conversation_id } => {
            match crate::drafts::load(&vault.database, &conversation_id).await {
                Ok(draft) => Event::Draft {
                    conversation_id,
                    draft,
                },
                Err(_) => failure(
                    "draft_failed",
                    "The saved Signal draft could not be read.",
                    true,
                ),
            }
        }
        Command::SaveDraft {
            conversation_id,
            expected_version,
            text,
            pending,
        } => {
            match crate::drafts::save(
                &vault.database,
                &conversation_id,
                request_id,
                expected_version,
                text,
                pending,
            )
            .await
            {
                Ok(draft) => Event::Draft {
                    conversation_id,
                    draft,
                },
                Err(_) => failure(
                    "draft_failed",
                    "The Signal draft could not be saved. Keep this pane open and retry.",
                    true,
                ),
            }
        }
        Command::CheckSend { attempt } => receipt(
            &vault.database,
            &attempt.id,
            &attempt.conversation,
            &attempt.text,
            attempt.timestamp,
        )
        .await
        .unwrap_or(Event::NotSent),
        Command::Conversations => match messages::conversations(&vault.store).await {
            Ok(items) => Event::Conversations {
                conversations: items.into_iter().map(|(item, _)| item).collect(),
            },
            Err(_) => failure(
                "read_failed",
                "Signal conversations could not be read.",
                true,
            ),
        },
        Command::Messages {
            conversation_id,
            before,
            limit,
        } => {
            let result = async {
                let thread = messages::resolve(&vault.store, &conversation_id).await?;
                messages::page(
                    &vault.store,
                    &vault.database,
                    &thread,
                    manager.registration_data().service_ids.aci().into(),
                    before,
                    limit,
                )
                .await
            }
            .await;
            match result {
                Ok(messages) => Event::Messages {
                    conversation_id,
                    messages,
                },
                Err(_) => failure("read_failed", "Signal messages could not be read.", true),
            }
        }
        Command::Send {
            conversation_id,
            text,
            timestamp,
        } => {
            send(
                vault,
                manager,
                phase,
                request_id,
                &conversation_id,
                &text,
                timestamp,
            )
            .await
        }
        _ => failure("invalid_request", "Unexpected Signal command.", false),
    }
}

async fn send(
    vault: &Vault,
    manager: &mut Client,
    phase: Phase,
    request_id: &str,
    conversation: &str,
    text: &str,
    timestamp: u64,
) -> Event {
    if text.trim().is_empty() || text.len() > MAX_MESSAGE_BYTES || timestamp > i64::MAX as u64 {
        return failure(
            "invalid_message",
            "The Signal message is empty or too large.",
            false,
        );
    }
    if let Some(event) = receipt(&vault.database, request_id, conversation, text, timestamp).await {
        return event;
    }
    let fingerprint = send_fingerprint(conversation, text, timestamp);
    if phase != Phase::Connected {
        return failure(
            "offline",
            "Signal is reconnecting. Your message has not been sent.",
            true,
        );
    }
    if timestamp.abs_diff(messages::now()) > 300_000 {
        return failure(
            "invalid_timestamp",
            "The system clock or message timestamp is out of date.",
            false,
        );
    }
    let Ok(thread) = messages::resolve(&vault.store, conversation).await else {
        return failure(
            "unknown_conversation",
            "Select a known Signal conversation.",
            false,
        );
    };
    let mut message = DataMessage {
        body: Some(text.into()),
        ..Default::default()
    };
    if crate::identity::ensure_send_allowed(&vault.store, &vault.database, conversation)
        .await
        .is_err()
    {
        return failure(
            "identity_check_required",
            "Review this conversation's safety numbers before sending. This message has not been sent.",
            false,
        );
    }
    if let Thread::Group(key) = &thread {
        let Ok(Some(group)) = vault.store.group(*key).await else {
            return failure(
                "unknown_group",
                "Signal group details are unavailable.",
                true,
            );
        };
        message.group_v2 = Some(GroupContextV2 {
            master_key: Some(key.to_vec()),
            revision: Some(group.revision),
            ..Default::default()
        });
    }
    // Commit uncertainty BEFORE touching the network. After a timeout or crash,
    // the same request can inspect its receipt but cannot send twice.
    if sqlx::query("INSERT INTO loom_send_v1(id, fingerprint, state, conversation, timestamp) VALUES (?, ?, 'uncertain', ?, ?)")
        .bind(request_id).bind(fingerprint).bind(conversation).bind(timestamp as i64)
        .execute(&vault.database).await.is_err() {
        return failure("store_failed", "Signal could not reserve this send. No message was sent.", false);
    }
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        match thread {
            Thread::Contact(recipient) => manager.send_message(recipient, message, timestamp).await,
            Thread::Group(key) => {
                manager
                    .send_message_to_group(&key, message, timestamp)
                    .await
            }
        }
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        return uncertain();
    }
    if sqlx::query("UPDATE loom_send_v1 SET state = 'sent' WHERE id = ?")
        .bind(request_id)
        .execute(&vault.database)
        .await
        .is_err()
    {
        return uncertain();
    }
    Event::Sent {
        conversation_id: conversation.into(),
        timestamp,
    }
}

fn send_fingerprint(conversation: &str, text: &str, timestamp: u64) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(conversation, text, timestamp)).expect("string tuple serialization"),
    ))
}

async fn receipt(
    database: &sqlx::SqlitePool,
    request_id: &str,
    conversation: &str,
    text: &str,
    timestamp: u64,
) -> Option<Event> {
    let fingerprint = send_fingerprint(conversation, text, timestamp);
    let prior = sqlx::query_as::<_, (String, String)>(
        "SELECT fingerprint, state FROM loom_send_v1 WHERE id = ?",
    )
    .bind(request_id)
    .fetch_optional(database)
    .await;
    Some(match prior {
        Ok(Some((known, state))) if known == fingerprint && state == "sent" => Event::Sent {
            conversation_id: conversation.into(),
            timestamp,
        },
        Ok(Some((known, _))) if known == fingerprint => uncertain(),
        Ok(Some(_)) => failure(
            "request_reused",
            "This send identity was already used for another message.",
            false,
        ),
        Err(_) => failure(
            "store_failed",
            "The Signal send ledger could not be read.",
            false,
        ),
        Ok(None) => return None,
    })
}

fn uncertain() -> Event {
    failure(
        "send_uncertain",
        "Signal may have sent this message. Check the conversation before sending it again.",
        false,
    )
}

fn start_receiver(
    mut manager: Client,
    database: sqlx::SqlitePool,
    output: Output,
    phase: watch::Sender<Phase>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::task::spawn_local(async move {
        let mut backoff = 1_u64;
        loop {
            phase.send_replace(Phase::Connecting);
            emit(&output, None, status(Some(&manager), Phase::Connecting)).await;
            let receive = async {
                let stream = manager.receive_messages().await?;
                futures::pin_mut!(stream);
                // Request a contact sync from our own primary Signal device.
                let _ = manager.request_contacts().await;
                while let Some(event) = stream.next().await {
                    match event {
                        Received::QueueEmpty => {
                            backoff = 1;
                            phase.send_replace(Phase::Connected);
                            emit(&output, None, status(Some(&manager), Phase::Connected)).await;
                        }
                        Received::Contacts => emit(&output, None, Event::Changed { conversation_id: None }).await,
                        Received::Content(content) => {
                            if retention::sweep(&database).await.is_err() {
                                stop.cancel();
                                break;
                            }
                            let conversation_id = Thread::try_from(content.as_ref()).ok().map(|thread| messages::id(&thread));
                            emit(&output, None, Event::Changed { conversation_id }).await;
                        }
                        Received::DecryptionError(_) => emit(&output, None,
                            failure("decryption_failed", "A Signal message could not be decrypted. Open Safety numbers and review this contact.", false)).await,
                    }
                }
                Ok::<(), presage::Error<presage_store_sqlite::SqliteStoreError>>(())
            };
            tokio::select! { biased; () = stop.cancelled() => break, _ = receive => () }
            phase.send_replace(Phase::Offline);
            emit(&output, None, status(Some(&manager), Phase::Offline)).await;
            let mut jitter = [0_u8; 2];
            let _ = getrandom::fill(&mut jitter);
            let delay = Duration::from_millis(
                backoff * 1000 + u64::from(u16::from_le_bytes(jitter)) % 1000,
            );
            tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(delay) => () }
            backoff = (backoff * 2).min(30);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stopping_releases_a_receiver_blocked_by_the_full_outbox() {
        let (sender, mut responses) = mpsc::channel(1);
        let stop = CancellationToken::new();
        let output = Output {
            sender,
            stop: stop.clone(),
        };
        emit(&output, None, Event::Stopped).await;
        let blocked = tokio::spawn(async move { emit(&output, None, Event::Stopped).await });
        tokio::task::yield_now().await;
        assert!(
            !blocked.is_finished(),
            "the full outbox applies backpressure"
        );
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("cancellation releases the sender")
            .expect("sender joined");
        assert!(responses.recv().await.is_some());
        assert!(
            responses.recv().await.is_none(),
            "no detached sender retains the outbox"
        );
    }
}
