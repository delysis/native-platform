//! Explicit acquisition over retained, named collections. Opening and searching
//! never enter this module's refresh worker or acquire account credentials.
use super::{
    ACCOUNT_OPERATION, AccountStore, Duration, GoogleService, GoogleSession, ImportOperation,
    ImportQuery, IpcFailure, PluginState, RemoteFile, Serialize, State, SyncFailure,
    download_page_file, google_import, lock_application_admission, lock_session,
    prepare_remote_file, require_bound_store,
};
use serde::Deserialize;

#[allow(clippy::needless_pass_by_value)] // Consumes heterogeneous errors at map_err boundaries.
fn failure(error: impl ToString) -> IpcFailure {
    IpcFailure::new("collection_refresh_failed", error.to_string(), false)
}
use crate::connected_collections as storage;
use crate::materials::{self, MaterialEntry};
use crate::workspace_template::{self, CollectionDefinition, CollectionScope};
use loom_store::ProjectStore;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager as _, Runtime};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefreshMode {
    Fresh,
    Resume,
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)] // Independent capability and coverage facts, not a state machine.
pub(crate) struct CollectionStatus {
    id: String,
    scope: CollectionScope,
    definition_fingerprint: String,
    job_id: Option<String>,
    phase: String,
    retained_count: usize,
    failures: Vec<SyncFailure>,
    resumable: bool,
    local_readable: bool,
    refresh_authorized: bool,
    coverage_complete: bool,
}

fn private_root(state: &PluginState) -> Result<PathBuf, IpcFailure> {
    let root = state
        .app_local_data_root
        .as_ref()
        .ok_or_else(|| failure("Private connection storage is unavailable."))?;
    std::fs::create_dir_all(root).map_err(failure)?;
    Ok(std::fs::canonicalize(root)
        .map_err(failure)?
        .join("connected-collections"))
}

fn service(scope: &CollectionScope) -> GoogleService {
    match scope {
        CollectionScope::DriveFolder { .. } => GoogleService::Drive,
        CollectionScope::GmailQuery { .. } => GoogleService::Gmail,
    }
}
fn query(scope: &CollectionScope) -> String {
    match scope {
        CollectionScope::DriveFolder { id } => id.clone(),
        CollectionScope::GmailQuery { query } => query.clone(),
    }
}
fn definition(store: &mut ProjectStore, id: &str) -> Result<CollectionDefinition, IpcFailure> {
    workspace_template::collection_definition(store, id)?
        .ok_or_else(|| failure("This collection is no longer in the workspace."))
}

fn status(
    state: &PluginState,
    store: &ProjectStore,
    session_id: &str,
    def: &CollectionDefinition,
) -> Result<CollectionStatus, IpcFailure> {
    let head = storage::read_head(store, &def.id).map_err(failure)?;
    let authorized = private_root(state)
        .ok()
        .and_then(|root| storage::require_grant(store, &root, def).ok())
        .is_some_and(|principal| {
            AccountStore::new(
                &store.manifest().project_id.to_string(),
                service(&def.scope),
            )
            .list()
            .is_ok_and(|accounts| {
                accounts
                    .iter()
                    .any(|account| account.account_email == principal)
            })
        });
    let mut result = CollectionStatus {
        id: def.id.clone(),
        scope: def.scope.clone(),
        definition_fingerprint: def.fingerprint()?,
        job_id: None,
        phase: "idle".into(),
        retained_count: 0,
        failures: Vec::new(),
        resumable: false,
        local_readable: true,
        refresh_authorized: authorized,
        coverage_complete: false,
    };
    if let Some(head) = head {
        let snapshot =
            storage::read_snapshot(store, &def.id, &head.snapshot_id).map_err(failure)?;
        result.retained_count = snapshot.members.len();
        let checkpoint = &head.checkpoint;
        let running = state.imports.is_running(session_id, &checkpoint.job_id);
        result.job_id = Some(checkpoint.job_id.clone());
        result.phase = match checkpoint.phase {
            storage::RefreshPhase::Running if running => "running",
            storage::RefreshPhase::Running | storage::RefreshPhase::Interrupted => "interrupted",
            storage::RefreshPhase::Paused => "paused",
            storage::RefreshPhase::Complete => "complete",
        }
        .into();
        result.resumable = result.phase != "running" && result.phase != "complete";
        result.coverage_complete = result.phase == "complete"
            && checkpoint.failures.is_empty()
            && snapshot
                .members
                .iter()
                .all(|member| member.coverage_complete);
        result.failures = checkpoint
            .failures
            .iter()
            .map(|item| SyncFailure {
                name: item.name.clone(),
                message: item.message.clone(),
            })
            .collect();
    }
    Ok(result)
}

#[tauri::command]
pub(crate) async fn collection_add(
    project_id: String,
    session_id: String,
    operation_id: String,
    name: String,
    scope: CollectionScope,
    account_email: String,
    state: State<'_, PluginState>,
) -> Result<MaterialEntry, IpcFailure> {
    let _account = ACCOUNT_OPERATION
        .try_lock()
        .map_err(|_| failure("Another account import is running."))?;
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &operation_id)?;
    let def = CollectionDefinition {
        id: format!(
            "material-{}",
            loom_types::BlobId::digest(crate::CommandId::new().to_string().as_bytes())
        ),
        name,
        pinned: false,
        workspace_path: None,
        scope,
    };
    def.validate()?;
    if !AccountStore::new(&project_id, service(&def.scope))
        .list()?
        .iter()
        .any(|account| account.account_email == account_email)
    {
        return Err(failure("Choose a connected account for this collection."));
    }
    let root = private_root(&state)?;
    operation.publish_to_store(&state, |store| {
        let current = workspace_template::collection_definitions(store)?;
        workspace_template::upsert_collection(store, current.revision_id, &def)?;
        storage::save_grant(store, &root, &def, &account_email).map_err(failure)?;
        materials::resolve(store, &def.id).map_err(Into::into)
    })
}

#[tauri::command]
pub(crate) async fn collection_status(
    project_id: String,
    session_id: String,
    id: String,
    state: State<'_, PluginState>,
) -> Result<CollectionStatus, IpcFailure> {
    let _admission = lock_application_admission(&state, "collection progress")?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let def = definition(store, &id)?;
    status(&state, store, &session_id, &def)
}

#[tauri::command]
pub(crate) async fn collection_authorize(
    project_id: String,
    session_id: String,
    id: String,
    definition_fingerprint: String,
    account_email: String,
    state: State<'_, PluginState>,
) -> Result<CollectionStatus, IpcFailure> {
    let _account = ACCOUNT_OPERATION
        .try_lock()
        .map_err(|_| failure("Another account import is running."))?;
    let _admission = lock_application_admission(&state, "connecting a collection")?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let def = definition(store, &id)?;
    if def.fingerprint()? != definition_fingerprint {
        return Err(failure(
            "The collection scope changed. Review it before connecting.",
        ));
    }
    if !AccountStore::new(&project_id, service(&def.scope))
        .list()?
        .iter()
        .any(|account| account.account_email == account_email)
    {
        return Err(failure("Choose a connected account for this collection."));
    }
    storage::save_grant(store, &private_root(&state)?, &def, &account_email).map_err(failure)?;
    status(&state, store, &session_id, &def)
}

#[tauri::command]
pub(crate) async fn collection_cancel(
    project_id: String,
    session_id: String,
    id: String,
    job_id: String,
    state: State<'_, PluginState>,
) -> Result<CollectionStatus, IpcFailure> {
    let _admission = lock_application_admission(&state, "stopping a collection refresh")?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let def = definition(store, &id)?;
    let head = storage::read_head(store, &id)
        .map_err(failure)?
        .ok_or_else(|| failure("There is no collection refresh to stop."))?;
    if head.checkpoint.job_id != job_id {
        return Err(failure("This collection refresh has changed."));
    }
    state.imports.cancel(&session_id, &job_id)?;
    status(&state, store, &session_id, &def)
}

#[tauri::command]
pub(crate) async fn collection_refresh<R: Runtime>(
    project_id: String,
    session_id: String,
    id: String,
    mode: RefreshMode,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<CollectionStatus, IpcFailure> {
    let account_lock = ACCOUNT_OPERATION
        .try_lock()
        .map_err(|_| failure("Another account import is running."))?;
    let job_id = crate::CommandId::new().to_string();
    let operation = ImportOperation::reserve(&state, &project_id, &session_id, &job_id)?;
    let private = private_root(&state)?;
    let (def, credentials, head) = operation.publish_to_store(&state, |store| {
        let def = definition(store, &id)?;
        let principal = storage::require_grant(store, &private, &def).map_err(failure)?;
        let credentials = AccountStore::new(&project_id, service(&def.scope))
            .list()?
            .into_iter()
            .find(|account| account.account_email == principal)
            .ok_or_else(|| failure("Reconnect this collection's account to refresh it."))?;
        let identity = storage::identity(&def, &principal).map_err(failure)?;
        let head = storage::begin_refresh(
            store,
            &identity,
            &job_id,
            matches!(mode, RefreshMode::Resume),
        )
        .map_err(failure)?;
        Ok((def, credentials, head))
    })?;
    let initial = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        status(&state, store, &session_id, &def)?
    };
    operation.dispatch(move |operation| {
        let _account = account_lock;
        let state = app.state::<PluginState>();
        tauri::async_runtime::block_on(run_refresh(
            &state,
            &operation,
            &private,
            &def,
            credentials,
            head,
        ));
    })?;
    Ok(initial)
}

/// Revalidate the definition at every publication. Editing config may describe
/// a new scope, but cannot enlarge the already admitted acquisition grant.
fn publish<T>(
    state: &PluginState,
    operation: &ImportOperation,
    def: &CollectionDefinition,
    work: impl FnOnce(&mut ProjectStore) -> Result<T, IpcFailure>,
) -> Result<T, IpcFailure> {
    operation.publish_to_store(state, |store| {
        if definition(store, &def.id)?.fingerprint()? != def.fingerprint()? {
            return Err(failure(
                "The collection settings changed. Refresh again to use them.",
            ));
        }
        work(store)
    })
}

async fn run_refresh(
    state: &PluginState,
    operation: &ImportOperation,
    private: &Path,
    def: &CollectionDefinition,
    credentials: google_import::GoogleCredentials,
    mut head: storage::CollectionHead,
) {
    let outcome = refresh_pages(state, operation, private, def, credentials, &mut head).await;
    if let Err(error) = outcome {
        // Stop revokes publication. Its durable running checkpoint is presented
        // as interrupted after ownership ends; no source can appear after Stop.
        let _ = publish(state, operation, def, |store| {
            storage::fail_refresh(store, &head, &error.message).map_err(failure)
        });
    }
}

async fn refresh_pages(
    state: &PluginState,
    operation: &ImportOperation,
    private: &Path,
    def: &CollectionDefinition,
    credentials: google_import::GoogleCredentials,
    head: &mut storage::CollectionHead,
) -> Result<(), IpcFailure> {
    let principal = credentials.account_email.clone();
    let remote = operation
        .network(async move {
            GoogleSession::refresh(&credentials)
                .await
                .map(std::sync::Arc::new)
                .map_err(failure)
        })
        .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_mins(5);
    let initial_bytes = head.checkpoint.bytes_read;
    let initial_pages = head.checkpoint.pages_completed;
    loop {
        operation.check()?;
        if tokio::time::Instant::now() >= deadline
            || head.checkpoint.bytes_read.saturating_sub(initial_bytes) >= 128 * 1024 * 1024
            || head
                .checkpoint
                .pages_completed
                .saturating_sub(initial_pages)
                >= 8
        {
            *head = publish(state, operation, def, |store| {
                storage::finish_refresh(store, head, storage::RefreshPhase::Paused).map_err(failure)
            })?;
            return Ok(());
        }
        if !head.checkpoint.page_open {
            *head = list_page(state, operation, private, def, head, &remote).await?;
        }
        let page_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_mins(3));
        let mut page_bytes = 0usize;
        while let Some(member) = head.checkpoint.pending.first().cloned() {
            operation.check()?;
            let remote_file = RemoteFile {
                id: member.remote_id.clone(),
                name: member.name.clone(),
                source_uri: member.source_uri.clone(),
                mime_type: member.mime_type.clone(),
                modified_time: member.listed_modified_time.clone(),
            };
            let session = std::sync::Arc::clone(&remote);
            let downloaded = operation
                .network(async move {
                    download_page_file(
                        &session,
                        &remote_file,
                        page_deadline,
                        (64 * 1024 * 1024usize).saturating_sub(page_bytes),
                    )
                    .await
                    .map_err(failure)
                })
                .await;
            let mut charged = 0;
            let result = match downloaded {
                Ok(bytes) => {
                    page_bytes += bytes.len();
                    charged = u64::try_from(bytes.len()).map_err(failure)?;
                    prepare_member(
                        state,
                        operation,
                        def,
                        head,
                        &member,
                        principal.clone(),
                        bytes,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            *head = match result {
                Ok(next) => next,
                Err(error) => publish(state, operation, def, |store| {
                    storage::fail_member(store, head, &member.remote_id, &error.message, charged)
                        .map_err(failure)
                })?,
            };
        }
        let more = head.checkpoint.has_next_page;
        *head = publish(state, operation, def, |store| {
            storage::finish_page(store, head).map_err(failure)
        })?;
        if !more {
            *head = publish(state, operation, def, |store| {
                storage::finish_refresh(store, head, storage::RefreshPhase::Complete)
                    .map_err(failure)
            })?;
            return Ok(());
        }
    }
}

async fn list_page(
    state: &PluginState,
    operation: &ImportOperation,
    private: &Path,
    def: &CollectionDefinition,
    head: &storage::CollectionHead,
    remote: &std::sync::Arc<GoogleSession>,
) -> Result<storage::CollectionHead, IpcFailure> {
    let page_token = publish(state, operation, def, |store| {
        storage::load_continuation(store, private, head).map_err(failure)
    })?;
    let import_query = ImportQuery {
        query: query(&def.scope),
        page_token,
    };
    let session = std::sync::Arc::clone(remote);
    let page = operation
        .network(async move { session.list(&import_query).await.map_err(failure) })
        .await?;
    let members = page
        .files
        .into_iter()
        .map(|file| storage::RemoteMember {
            remote_id: file.id,
            name: file.name,
            source_uri: file.source_uri,
            mime_type: file.mime_type,
            listed_modified_time: file.modified_time,
        })
        .collect();
    publish(state, operation, def, |store| {
        storage::set_page(store, private, head, members, page.next_page_token).map_err(failure)
    })
}

async fn prepare_member(
    state: &PluginState,
    operation: &ImportOperation,
    def: &CollectionDefinition,
    head: &storage::CollectionHead,
    member: &storage::RemoteMember,
    principal: String,
    bytes: Vec<u8>,
) -> Result<storage::CollectionHead, IpcFailure> {
    let charged = u64::try_from(bytes.len()).map_err(failure)?;
    let file = RemoteFile {
        id: member.remote_id.clone(),
        name: member.name.clone(),
        source_uri: member.source_uri.clone(),
        mime_type: member.mime_type.clone(),
        modified_time: member.listed_modified_time.clone(),
    };
    let root = operation.root.clone();
    let selected_service = service(&def.scope);
    let prepared = operation
        .compute(move || prepare_remote_file(&root, selected_service, &principal, &file, bytes))
        .await;
    prepared.and_then(|(prepared, origin)| {
        publish(state, operation, def, |store| {
            let attachment = prepared.publish().map_err(failure)?;
            let version = storage::OccurrenceVersion::new(
                &head.checkpoint.identity,
                member,
                attachment.id,
                origin,
            )
            .map_err(failure)?;
            storage::publish_member(store, head, version, charged).map_err(failure)
        })
    })
}

#[tauri::command]
pub(crate) async fn collection_members(
    project_id: String,
    session_id: String,
    id: String,
    offset: usize,
    snapshot_id: Option<String>,
    state: State<'_, PluginState>,
) -> Result<materials::collections::MembersPage, IpcFailure> {
    let _admission = lock_application_admission(&state, "reading a collection")?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    materials::collections::members(store, &id, offset, snapshot_id.as_deref()).map_err(Into::into)
}
#[tauri::command]
pub(crate) async fn collection_read_member(
    project_id: String,
    session_id: String,
    id: String,
    snapshot_id: String,
    occurrence_id: String,
    state: State<'_, PluginState>,
) -> Result<materials::MaterialRead, IpcFailure> {
    let read = {
        let _admission = lock_application_admission(&state, "reading a collection source")?;
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        materials::collections::read_member(store, &id, &snapshot_id, &occurrence_id)?
    };
    crate::material_media::bind_tokens(read, &project_id, &session_id)
}

pub(crate) fn remove_binding(
    state: &PluginState,
    store: &mut ProjectStore,
    session_id: &str,
    id: &str,
) -> Result<bool, IpcFailure> {
    let definitions = workspace_template::collection_definitions(store)?;
    if !definitions.collections.iter().any(|def| def.id == id) {
        return Ok(false);
    }
    if let Some(head) = storage::read_head(store, id).map_err(failure)? {
        state.imports.cancel(session_id, &head.checkpoint.job_id)?;
    }
    storage::revoke_grant(store, &private_root(state)?, id).map_err(failure)?;
    workspace_template::remove_collection(store, definitions.revision_id, id)?;
    Ok(true)
}
