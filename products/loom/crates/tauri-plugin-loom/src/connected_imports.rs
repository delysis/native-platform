//! Product-owned account grants, credential storage, and explicit sync.
use super::{
    IpcFailure, PluginState, lock_application_admission, lock_session, require_bound_store,
};
use crate::context_attachments::{import_provided, record_import_origin};
use attachment_native_host::ProvidedAttachment;
use information_native_acquire::google_import::{
    self, GoogleCredentials, GoogleService, GoogleSession, ImportQuery,
};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Duration;
use tauri::State;

// Serializes connect/sync/disconnect without holding the editor/session lock
// across browser authorization or network waits.
static AUTH_CANCEL: Mutex<Option<(String, String, tokio::sync::oneshot::Sender<()>)>> =
    Mutex::new(None);

static ACCOUNT_OPERATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Serialize)]
pub(crate) struct AccountStatus {
    service: GoogleService,
    email: Option<String>,
}

use crate::import_batch::{ImportBatch as SyncReport, ImportFailure as SyncFailure};

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImportSource {
    Gmail,
    GoogleAlerts,
    LinkedIn,
    Drive,
}
impl ImportSource {
    fn service(self) -> GoogleService {
        if matches!(self, Self::Drive) {
            GoogleService::Drive
        } else {
            GoogleService::Gmail
        }
    }
    fn query(self, input: &str) -> String {
        match self {
            Self::GoogleAlerts => format!("from:googlealerts-noreply@google.com ({input})"),
            Self::LinkedIn => format!("from:linkedin.com ({input})"),
            Self::Gmail | Self::Drive => input.to_string(),
        }
    }
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("connected_import_failed", message, false)
}
fn keyring_failure(_: keyring::Error) -> IpcFailure {
    failure(
        "The system credential store is unavailable. No credential was written to a project file.",
    )
}

fn entry(project_id: &str, service: GoogleService) -> Result<keyring::Entry, IpcFailure> {
    let service_name = match service {
        GoogleService::Gmail => "gmail",
        GoogleService::Drive => "drive",
    };
    keyring::Entry::new(
        "com.delysis.loom.connected-import",
        &format!("{project_id}:{service_name}"),
    )
    .map_err(keyring_failure)
}

fn credentials(
    project_id: &str,
    service: GoogleService,
) -> Result<Vec<GoogleCredentials>, IpcFailure> {
    match entry(project_id, service)?.get_password() {
        Ok(value) => {
            let accounts: Vec<GoogleCredentials> = serde_json::from_str(&value).map_err(|_| {
                failure("The stored account credential is invalid; disconnect and reconnect it.")
            })?;
            if accounts.len() > 8 || accounts.iter().any(|account| account.service != service) {
                return Err(failure(
                    "The stored account scope does not match this import.",
                ));
            }
            Ok(accounts)
        }
        Err(keyring::Error::NoEntry) => Ok(Vec::new()),
        Err(error) => Err(keyring_failure(error)),
    }
}

fn validate_session(
    state: &PluginState,
    project_id: &str,
    session_id: &str,
) -> Result<(), IpcFailure> {
    let _admission = lock_application_admission(state, "connected import")?;
    require_bound_store(&mut *lock_session(state)?, project_id, session_id)?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn import_accounts(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<AccountStatus>, IpcFailure> {
    validate_session(&state, &project_id, &session_id)?;
    let mut result = Vec::new();
    for service in [GoogleService::Gmail, GoogleService::Drive] {
        for account in credentials(&project_id, service)? {
            result.push(AccountStatus {
                service,
                email: Some(account.account_email),
            });
        }
    }
    Ok(result)
}

#[tauri::command]
pub(crate) async fn import_account_connect(
    project_id: String,
    session_id: String,
    service: GoogleService,
    client_id: String,
    client_secret: String,
    state: State<'_, PluginState>,
) -> Result<AccountStatus, IpcFailure> {
    let _operation = ACCOUNT_OPERATION
        .try_lock()
        .map_err(|_| failure("Another account import is running."))?;
    validate_session(&state, &project_id, &session_id)?;
    let credential_entry = entry(&project_id, service)?;
    let pending = google_import::begin_authorization(client_id, client_secret, service)
        .await
        .map_err(|e| failure(e.to_string()))?;
    tauri_plugin_opener::open_url(&pending.authorization_url, None::<&str>)
        .map_err(|_| failure("The authorization browser could not be opened."))?;
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    *AUTH_CANCEL
        .lock()
        .map_err(|_| failure("Authorization state is unavailable."))? =
        Some((project_id.clone(), session_id.clone(), cancel_tx));
    let result = tokio::select! {
        result = pending.finish() => result.map_err(|e| failure(e.to_string())),
        _ = cancel_rx => Err(failure("Account connection canceled.")),
    };
    AUTH_CANCEL
        .lock()
        .map_err(|_| failure("Authorization state is unavailable."))?
        .take();
    let credential = result?;
    let _admission = lock_application_admission(&state, "saving an account authorization")?;
    require_bound_store(&mut *lock_session(&state)?, &project_id, &session_id)?;
    let email = credential.account_email.clone();
    let mut accounts = credentials(&project_id, service)?;
    accounts.retain(|account| account.account_email != email);
    if accounts.len() >= 8 {
        return Err(failure(
            "Disconnect an account before adding another; this project supports eight accounts per service.",
        ));
    }
    accounts.push(credential);
    let encoded = serde_json::to_string(&accounts)
        .map_err(|_| failure("The account credential could not be encoded."))?;
    credential_entry
        .set_password(&encoded)
        .map_err(keyring_failure)?;
    Ok(AccountStatus {
        service,
        email: Some(email),
    })
}

#[tauri::command]
pub(crate) async fn import_account_disconnect(
    project_id: String,
    session_id: String,
    service: GoogleService,
    account_email: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let _operation = ACCOUNT_OPERATION.try_lock().map_err(|_| {
        failure("An account import is running; wait for it to finish before disconnecting.")
    })?;
    let _admission = lock_application_admission(&state, "disconnecting an import account")?;
    require_bound_store(&mut *lock_session(&state)?, &project_id, &session_id)?;
    let mut accounts = credentials(&project_id, service)?;
    accounts.retain(|account| account.account_email != account_email);
    let credential_entry = entry(&project_id, service)?;
    if accounts.is_empty() {
        match credential_entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_failure(error)),
        }
    } else {
        let encoded = serde_json::to_string(&accounts)
            .map_err(|_| failure("The account list could not be encoded."))?;
        credential_entry
            .set_password(&encoded)
            .map_err(keyring_failure)
    }
}

#[derive(Serialize)]
struct ImportOrigin<'a> {
    schema: &'static str,
    service: GoogleService,
    account_email: &'a str,
    source_uri: &'a str,
    remote_id: &'a str,
    listed_modified_time: Option<&'a str>,
    source_sha256: &'a str,
    source_bytes: u64,
    network_used: bool,
    human_authored: Option<bool>,
    human_reviewed: bool,
}

#[tauri::command]
pub(crate) async fn import_account_sync(
    project_id: String,
    session_id: String,
    source: ImportSource,
    account_email: String,
    query: String,
    page_token: Option<String>,
    state: State<'_, PluginState>,
) -> Result<SyncReport, IpcFailure> {
    let _operation = ACCOUNT_OPERATION
        .try_lock()
        .map_err(|_| failure("Another account import is running."))?;
    validate_session(&state, &project_id, &session_id)?;
    if query.trim().is_empty() && !matches!(source, ImportSource::Drive) {
        return Err(failure(
            "Enter a Gmail search, such as newer_than:30d or label:Research.",
        ));
    }
    let credential = credentials(&project_id, source.service())?
        .into_iter()
        .find(|account| account.account_email == account_email)
        .ok_or_else(|| failure("Connect this account first."))?;
    let remote = GoogleSession::refresh(&credential)
        .await
        .map_err(|e| failure(e.to_string()))?;
    let page = remote
        .list(&ImportQuery {
            query: source.query(query.trim()),
            page_token,
        })
        .await
        .map_err(|e| failure(e.to_string()))?;
    let mut report = SyncReport {
        imported: Vec::new(),
        failures: Vec::new(),
        next_page_token: page.next_page_token,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_mins(3);
    let mut total_bytes = 0usize;
    for file in page.files {
        validate_session(&state, &project_id, &session_id)?;
        let bytes = match tokio::time::timeout_at(deadline, remote.download(&file)).await {
            Ok(Ok(bytes)) if bytes.len() <= (64 * 1024 * 1024usize).saturating_sub(total_bytes) => {
                bytes
            }
            Ok(Ok(_)) => {
                report.failures.push(SyncFailure {
                    name: file.name,
                    message: "This page exceeded the 64 MB import budget.".into(),
                });
                continue;
            }
            Ok(Err(error)) => {
                report.failures.push(SyncFailure {
                    name: file.name,
                    message: error.to_string(),
                });
                continue;
            }
            Err(_) => {
                report.failures.push(SyncFailure {
                    name: file.name,
                    message: "This page reached its three-minute time limit.".into(),
                });
                continue;
            }
        };
        total_bytes += bytes.len();
        // Commit each completed import against the exact live project session.
        // Failed files remain explicit and never erase already completed rows.
        let _admission = lock_application_admission(&state, "storing a connected import")?;
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        let result = import_provided(
            store.root(),
            ProvidedAttachment::from_bytes(&file.name, None, bytes),
        );
        match result {
            Ok(mut attachment) => {
                if let Err(error) = record_import_origin(
                    store.root(),
                    &ImportOrigin {
                        schema: "loom.connected-import.v1",
                        service: source.service(),
                        account_email: &credential.account_email,
                        source_uri: &file.source_uri,
                        remote_id: &file.id,
                        listed_modified_time: file.modified_time.as_deref(),
                        source_sha256: &attachment.id,
                        source_bytes: attachment.byte_count,
                        network_used: true,
                        human_authored: None,
                        human_reviewed: false,
                    },
                ) {
                    report.failures.push(SyncFailure {
                        name: file.name,
                        message: error.to_string(),
                    });
                    continue;
                }
                attachment.editable_markdown = None;
                attachment.media_markdown = None;
                report.imported.push(attachment);
            }
            Err(error) => report.failures.push(SyncFailure {
                name: file.name,
                message: error.to_string(),
            }),
        }
    }
    Ok(report)
}

#[tauri::command]
pub(crate) async fn import_source_url(
    project_id: String,
    session_id: String,
    url: String,
    state: State<'_, PluginState>,
) -> Result<SyncReport, IpcFailure> {
    validate_session(&state, &project_id, &session_id)?;
    if url.len() > 4096 || !url.starts_with("https://") {
        return Err(failure("Enter a public HTTPS document URL."));
    }
    let requested = url.clone();
    let downloaded = tauri::async_runtime::spawn_blocking(move || {
        let client = information_native_acquire::AcquireClient::new(
            information_native_acquire::AcquireConfig {
                request_timeout: Duration::from_secs(45),
                ..Default::default()
            },
        )?;
        client.fetch_catalogue(&url, 32 * 1024 * 1024)
    })
    .await
    .map_err(|_| failure("The download worker stopped."))?
    .map_err(|e| failure(e.to_string()))?;
    let _admission = lock_application_admission(&state, "storing a web import")?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let mut attachment = import_provided(
        store.root(),
        ProvidedAttachment::from_bytes("Web source", None, downloaded.bytes),
    )
    .map_err(|e| failure(e.to_string()))?;
    record_import_origin(
        store.root(),
        &serde_json::json!({
            "schema": "loom.web-import.v1", "requested_uri": requested,
            "final_source_uri": downloaded.final_source_uri,
            "source_attestation": downloaded.source_attestation,
            "source_sha256": attachment.id, "source_bytes": attachment.byte_count,
            "network_used": downloaded.network_used, "human_reviewed": false,
        }),
    )
    .map_err(|e| failure(e.to_string()))?;
    attachment.editable_markdown = None;
    attachment.media_markdown = None;
    Ok(SyncReport {
        imported: vec![attachment],
        failures: Vec::new(),
        next_page_token: None,
    })
}

#[tauri::command]
pub(crate) async fn import_account_cancel(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    validate_session(&state, &project_id, &session_id)?;
    let mut active = AUTH_CANCEL
        .lock()
        .map_err(|_| failure("Authorization state is unavailable."))?;
    if active
        .as_ref()
        .is_some_and(|(project, session, _)| project == &project_id && session == &session_id)
        && let Some((_, _, sender)) = active.take()
    {
        let _ = sender.send(());
    }
    Ok(())
}
