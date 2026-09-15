//! Product-owned account grants, credential storage, and explicit sync.
mod credentials;

use super::{
    IpcFailure, PluginState, lock_application_admission, lock_session, require_bound_store,
};
use crate::context_attachments::{PreparedAttachment, prepare_provided, record_import_origin};
use crate::import_jobs::ImportOperation;
use attachment_native_host::ProvidedAttachment;
use credentials::AccountStore;
use information_native_acquire::google_import::{
    self, GoogleService, GoogleSession, ImportQuery, RemoteFile,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::State;

// Serializes account changes without holding editor/session locks.
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
        for account in AccountStore::new(&project_id, service).list()? {
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
    operation_id: String,
    service: GoogleService,
    client_id: String,
    client_secret: String,
    state: State<'_, PluginState>,
) -> Result<AccountStatus, IpcFailure> {
    let _operation = ACCOUNT_OPERATION
        .try_lock()
        .map_err(|_| failure("Another account import is running."))?;
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    let credential = operation
        .network(async move {
            let pending = google_import::begin_authorization(client_id, client_secret, service)
                .await
                .map_err(|error| failure(error.to_string()))?;
            // The operation and cancellation signal already exist before the browser opens.
            tauri_plugin_opener::open_url(&pending.authorization_url, None::<&str>)
                .map_err(|_| failure("The authorization browser could not be opened."))?;
            pending
                .finish()
                .await
                .map_err(|error| failure(error.to_string()))
        })
        .await?;
    let email = credential.account_email.clone();
    operation.publish_action(&state, || {
        AccountStore::new(&project_id, service).save(&credential)
    })?;
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
    AccountStore::new(&project_id, service).disconnect(&account_email)
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

fn prepare_remote_file(
    root: &std::path::Path,
    source: ImportSource,
    email: &str,
    file: &RemoteFile,
    bytes: Vec<u8>,
) -> Result<PreparedAttachment, IpcFailure> {
    let mut prepared = prepare_provided(
        root,
        ProvidedAttachment::from_bytes(&file.name, None, bytes),
    )
    .map_err(|error| failure(error.to_string()))?;
    record_import_origin(
        root,
        &ImportOrigin {
            schema: "loom.connected-import.v1",
            service: source.service(),
            account_email: email,
            source_uri: &file.source_uri,
            remote_id: &file.id,
            listed_modified_time: file.modified_time.as_deref(),
            source_sha256: &prepared.attachment.id,
            source_bytes: prepared.attachment.byte_count,
            network_used: true,
            human_authored: None,
            human_reviewed: false,
        },
    )
    .map_err(|error| failure(error.to_string()))?;
    prepared.attachment.editable_markdown = None;
    prepared.attachment.media_markdown = None;
    Ok(prepared)
}

async fn download_page_file(
    remote: &GoogleSession,
    file: &RemoteFile,
    deadline: tokio::time::Instant,
    remaining_bytes: usize,
) -> Result<Vec<u8>, String> {
    match tokio::time::timeout_at(deadline, remote.download(file)).await {
        Ok(Ok(bytes)) if bytes.len() <= remaining_bytes => Ok(bytes),
        Ok(Ok(_)) => Err("This page exceeded the 64 MB import budget.".into()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("This page reached its three-minute time limit.".into()),
    }
}

// Keep the existing flat IPC fields and the separately cancellable operation identity.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_account_sync(
    project_id: String,
    session_id: String,
    operation_id: String,
    source: ImportSource,
    account_email: String,
    query: String,
    page_token: Option<String>,
    state: State<'_, PluginState>,
) -> Result<SyncReport, IpcFailure> {
    let _operation = ACCOUNT_OPERATION
        .try_lock()
        .map_err(|_| failure("Another account import is running."))?;
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    if query.trim().is_empty() && !matches!(source, ImportSource::Drive) {
        return Err(failure(
            "Enter a Gmail search, such as newer_than:30d or label:Research.",
        ));
    }
    let credential = AccountStore::new(&project_id, source.service())
        .list()?
        .into_iter()
        .find(|account| account.account_email == account_email)
        .ok_or_else(|| failure("Connect this account first."))?;
    let import_query = ImportQuery {
        query: source.query(query.trim()),
        page_token,
    };
    let origin_email = credential.account_email.clone();
    let (remote, page) = operation
        .network(async move {
            let remote = GoogleSession::refresh(&credential)
                .await
                .map_err(|error| failure(error.to_string()))?;
            let page = remote
                .list(&import_query)
                .await
                .map_err(|error| failure(error.to_string()))?;
            Ok((std::sync::Arc::new(remote), page))
        })
        .await?;
    let mut report = SyncReport {
        imported: Vec::new(),
        failures: Vec::new(),
        next_page_token: page.next_page_token,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_mins(3);
    let mut total_bytes = 0usize;
    let file_count = page.files.len();
    for (index, file) in page.files.into_iter().enumerate() {
        let name = file.name.clone();
        let root = operation.root.clone();
        let remote = std::sync::Arc::clone(&remote);
        let remaining = (64 * 1024 * 1024usize).saturating_sub(total_bytes);
        let downloaded = operation
            .network(async move {
                let bytes = download_page_file(&remote, &file, deadline, remaining)
                    .await
                    .map_err(failure)?;
                Ok((file, bytes))
            })
            .await;
        let result = match downloaded {
            Ok((file, bytes)) => {
                total_bytes += bytes.len();
                let email = origin_email.clone();
                operation
                    .compute(move || prepare_remote_file(&root, source, &email, &file, bytes))
                    .await
                    .and_then(|prepared| operation.publish(&state, prepared))
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(attachment) => report.imported.push(attachment),
            Err(error) => report.failures.push(SyncFailure {
                name,
                message: error.message,
            }),
        }
        if operation.check().is_err() {
            report.next_page_token = None;
            if index + 1 < file_count {
                report.failures.push(SyncFailure {
                    name: "Remaining sources".into(),
                    message: format!("Import stopped with {} sources remaining on this page. Retry this page to import them.", file_count - index - 1),
                });
            }
            break;
        }
    }

    Ok(report)
}

#[tauri::command]
pub(crate) async fn import_source_url(
    project_id: String,
    session_id: String,
    operation_id: String,
    url: String,
    state: State<'_, PluginState>,
) -> Result<SyncReport, IpcFailure> {
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    if url.len() > 4096 || !url.starts_with("https://") {
        return Err(failure("Enter a public HTTPS document URL."));
    }
    let root = operation.root.clone();
    let cancel = operation.stop_flag();
    let prepared = operation.compute(move || {
        let client = information_native_acquire::AcquireClient::new(information_native_acquire::AcquireConfig {
            request_timeout: Duration::from_secs(45), ..Default::default()
        }).map_err(|error| failure(error.to_string()))?;
        let downloaded = client.fetch_catalogue(&url, 32 * 1024 * 1024).map_err(|error| failure(error.to_string()))?;
        if cancel.load(std::sync::atomic::Ordering::Acquire) { return Err(failure("Import stopped.")); }
        let mut prepared = prepare_provided(&root, ProvidedAttachment::from_bytes("Web source", None, downloaded.bytes))
            .map_err(|error| failure(error.to_string()))?;
        record_import_origin(&root, &serde_json::json!({
            "schema": "loom.web-import.v1", "requested_uri": url,
            "final_source_uri": downloaded.final_source_uri, "source_attestation": downloaded.source_attestation,
            "source_sha256": prepared.attachment.id, "source_bytes": prepared.attachment.byte_count,
            "network_used": downloaded.network_used, "human_reviewed": false,
        })).map_err(|error| failure(error.to_string()))?;
        prepared.attachment.editable_markdown = None;
        prepared.attachment.media_markdown = None;
        Ok(prepared)
    }).await?;
    let attachment = operation.publish(&state, prepared)?;
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
    operation_id: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    validate_session(&state, &project_id, &session_id)?;
    state.imports.cancel(&session_id, &operation_id)?;
    Ok(())
}
