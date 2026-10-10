//! Retained connected scopes. Immutable leaves precede the single head commit.
//! Call mutations through `ImportOperation`'s guarded publication closure; this
//! module never acquires credentials, performs network work, or starts jobs.
mod private;
mod storage;
#[cfg(all(test, unix))]
mod tests;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

use loom_store::ProjectStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::context_attachments;
use crate::workspace_template::{CollectionDefinition, CollectionScope};
pub(crate) use private::{load_continuation, require_grant, revoke_grant, save_grant};
use storage::{install_snapshot, load_head, replace_head};

pub(crate) const MAX_PAGES: u32 = 32;
pub(crate) const MAX_MEMBERS: usize = 4096;
pub(crate) const MAX_PAGE_MEMBERS: usize = 128;
pub(crate) const MAX_REFRESH_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FAILURES: usize = 512;
const SCHEMA: &str = "loom.connected-collection.v1";
static WRITE_LOCK: Mutex<()> = Mutex::new(());

type Result<T> = std::result::Result<T, CollectionError>;
#[derive(Debug, Error)]
pub(crate) enum CollectionError {
    #[error("{0}")]
    Invalid(String),
    #[error("The collection changed before this update could be published.")]
    Conflict,
    #[error("Connect this collection's selected account and scope before refreshing.")]
    NeedsAuthorization,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Attachment(#[from] context_attachments::ContextAttachmentError),
}
fn invalid(message: impl Into<String>) -> CollectionError {
    CollectionError::Invalid(message.into())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_collection_id(id: &str) -> bool {
    id.strip_prefix("material-").is_some_and(valid_hash)
}
fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.contains('\0')
}
fn valid_job(id: &str) -> bool {
    id.parse::<crate::CommandId>().is_ok()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectionIdentity {
    pub(crate) collection_id: String,
    pub(crate) definition_fingerprint: String,
    pub(crate) scope_fingerprint: String,
    pub(crate) provider: String,
    pub(crate) principal_fingerprint: String,
}
pub(crate) fn identity(
    definition: &CollectionDefinition,
    principal: &str,
) -> Result<CollectionIdentity> {
    definition
        .validate()
        .map_err(|error| invalid(error.message))?;
    if !bounded(principal, 1024) {
        return Err(invalid("Invalid selected account identity."));
    }
    Ok(CollectionIdentity {
        collection_id: definition.id.clone(),
        definition_fingerprint: definition
            .fingerprint()
            .map_err(|error| invalid(error.message))?,
        scope_fingerprint: definition
            .scope_fingerprint()
            .map_err(|error| invalid(error.message))?,
        provider: match definition.scope {
            CollectionScope::DriveFolder { .. } => "drive",
            CollectionScope::GmailQuery { .. } => "gmail",
        }
        .into(),
        principal_fingerprint: digest(principal.as_bytes()),
    })
}
fn validate_identity(identity: &CollectionIdentity) -> Result<()> {
    if !valid_collection_id(&identity.collection_id)
        || !valid_hash(&identity.definition_fingerprint)
        || !valid_hash(&identity.scope_fingerprint)
        || !valid_hash(&identity.principal_fingerprint)
        || !matches!(identity.provider.as_str(), "drive" | "gmail")
    {
        return Err(invalid("Invalid retained collection identity."));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteMember {
    pub(crate) remote_id: String,
    pub(crate) name: String,
    pub(crate) source_uri: String,
    pub(crate) mime_type: String,
    pub(crate) listed_modified_time: Option<String>,
}
impl RemoteMember {
    fn validate(&self) -> Result<()> {
        if !bounded(&self.remote_id, 2048)
            || !bounded(&self.name, 1024)
            || !bounded(&self.source_uri, 4096)
            || !bounded(&self.mime_type, 256)
            || self
                .listed_modified_time
                .as_ref()
                .is_some_and(|time| !bounded(time, 256))
        {
            return Err(invalid("Invalid or oversized remote member metadata."));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OccurrenceVersion {
    pub(crate) occurrence_id: String,
    pub(crate) provider: String,
    pub(crate) principal_fingerprint: String,
    pub(crate) remote: RemoteMember,
    pub(crate) attachment_id: String,
    pub(crate) origin_receipt_id: String,
    pub(crate) observed_in_refresh: String,
    pub(crate) coverage_complete: bool,
}
fn occurrence_id(provider: &str, principal: &str, remote_id: &str) -> Result<String> {
    Ok(format!(
        "occurrence-{}",
        digest(&serde_json::to_vec(&(provider, principal, remote_id))?)
    ))
}
impl OccurrenceVersion {
    pub(crate) fn new(
        identity: &CollectionIdentity,
        remote: &RemoteMember,
        attachment_id: String,
        origin_receipt_id: String,
    ) -> Result<Self> {
        validate_identity(identity)?;
        remote.validate()?;
        if !valid_hash(&attachment_id)
            || !origin_receipt_id
                .strip_prefix("source-")
                .is_some_and(valid_hash)
        {
            return Err(invalid("Invalid retained source identity."));
        }
        Ok(Self {
            occurrence_id: occurrence_id(
                &identity.provider,
                &identity.principal_fingerprint,
                &remote.remote_id,
            )?,
            provider: identity.provider.clone(),
            principal_fingerprint: identity.principal_fingerprint.clone(),
            remote: remote.clone(),
            attachment_id,
            origin_receipt_id,
            observed_in_refresh: String::new(),
            coverage_complete: false,
        })
    }
    fn validate(&self) -> Result<()> {
        self.remote.validate()?;
        if !matches!(self.provider.as_str(), "drive" | "gmail")
            || !valid_hash(&self.principal_fingerprint)
            || self.occurrence_id
                != occurrence_id(
                    &self.provider,
                    &self.principal_fingerprint,
                    &self.remote.remote_id,
                )?
            || !valid_hash(&self.attachment_id)
            || !self
                .origin_receipt_id
                .strip_prefix("source-")
                .is_some_and(valid_hash)
            || !valid_job(&self.observed_in_refresh)
        {
            return Err(invalid("Invalid occurrence version."));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefreshPhase {
    Running,
    Paused,
    Interrupted,
    Complete,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberFailure {
    pub(crate) remote_id: String,
    pub(crate) name: String,
    pub(crate) message: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshCheckpoint {
    pub(crate) identity: CollectionIdentity,
    pub(crate) job_id: String,
    pub(crate) refresh_id: String,
    pub(crate) phase: RefreshPhase,
    pub(crate) pending: Vec<RemoteMember>,
    pub(crate) failures: Vec<MemberFailure>,
    pub(crate) pages_completed: u32,
    pub(crate) completed_count: u32,
    pub(crate) bytes_read: u64,
    pub(crate) page_open: bool,
    pub(crate) has_next_page: bool,
    pub(crate) continuation_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectionHead {
    schema: String,
    #[serde(skip)]
    pub(crate) revision: String,
    pub(crate) snapshot_id: String,
    pub(crate) checkpoint: RefreshCheckpoint,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectionSnapshot {
    schema: String,
    #[serde(skip)]
    pub(crate) id: String,
    pub(crate) identity: CollectionIdentity,
    pub(crate) previous_snapshot_id: Option<String>,
    pub(crate) members: Vec<OccurrenceVersion>,
    /// Only a completed listing can establish that a retained member was not
    /// observed. Download failures remain separate; this is no coverage claim.
    pub(crate) completed_listing_refresh: Option<String>,
}

pub(crate) fn read_head(store: &ProjectStore, id: &str) -> Result<Option<CollectionHead>> {
    let head = load_head(store, id)?;
    if let Some(head) = &head {
        validate_head(head, id)?;
    }
    Ok(head)
}
pub(crate) fn read_snapshot(
    store: &ProjectStore,
    id: &str,
    snapshot_id: &str,
) -> Result<CollectionSnapshot> {
    let snapshot = storage::load_snapshot(store, id, snapshot_id)?;
    validate_identity(&snapshot.identity)?;
    if snapshot.schema != SCHEMA
        || snapshot.identity.collection_id != id
        || snapshot.members.len() > MAX_MEMBERS
        || snapshot
            .previous_snapshot_id
            .as_ref()
            .is_some_and(|id| !valid_hash(id))
        || snapshot
            .completed_listing_refresh
            .as_ref()
            .is_some_and(|id| !valid_job(id))
    {
        return Err(invalid("Invalid collection snapshot."));
    }
    let mut seen = BTreeSet::new();
    for member in &snapshot.members {
        member.validate()?;
        if !seen.insert(&member.occurrence_id) {
            return Err(invalid("Duplicate source occurrence."));
        }
    }
    Ok(snapshot)
}
fn validate_head(head: &CollectionHead, id: &str) -> Result<()> {
    let checkpoint = &head.checkpoint;
    validate_identity(&checkpoint.identity)?;
    if head.schema != SCHEMA
        || checkpoint.identity.collection_id != id
        || !valid_hash(&head.snapshot_id)
        || !valid_job(&checkpoint.job_id)
        || !valid_job(&checkpoint.refresh_id)
        || checkpoint.pages_completed > MAX_PAGES
        || checkpoint.pending.len() > MAX_PAGE_MEMBERS
        || checkpoint.failures.len() > MAX_FAILURES
        || checkpoint.bytes_read > MAX_REFRESH_BYTES
        || checkpoint
            .continuation_id
            .as_ref()
            .is_some_and(|id| !valid_hash(id))
        || (!checkpoint.page_open && !checkpoint.pending.is_empty())
        || (checkpoint.phase == RefreshPhase::Complete
            && (checkpoint.page_open || checkpoint.has_next_page))
    {
        return Err(invalid("Invalid collection refresh checkpoint."));
    }
    let mut seen = BTreeSet::new();
    for member in &checkpoint.pending {
        member.validate()?;
        if !seen.insert(&member.remote_id) {
            return Err(invalid("Duplicate pending member."));
        }
    }
    for failure in &checkpoint.failures {
        if !bounded(&failure.remote_id, 2048)
            || !bounded(&failure.name, 1024)
            || !bounded(&failure.message, 2048)
        {
            return Err(invalid("Invalid retained import failure."));
        }
    }
    Ok(())
}

pub(crate) fn begin_refresh(
    store: &ProjectStore,
    identity: &CollectionIdentity,
    job_id: &str,
    resume: bool,
) -> Result<CollectionHead> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    validate_identity(identity)?;
    if !valid_job(job_id) {
        return Err(invalid("Invalid refresh job identity."));
    }
    let old = read_head(store, &identity.collection_id)?;
    if resume {
        let mut head = old.ok_or_else(|| invalid("There is no refresh to resume."))?;
        if head.checkpoint.identity != *identity || head.checkpoint.phase == RefreshPhase::Complete
        {
            return Err(invalid("Start a fresh listing for this collection."));
        }
        head.checkpoint.job_id = job_id.into();
        head.checkpoint.phase = RefreshPhase::Running;
        return replace_head(store, head);
    }
    let previous_snapshot_id = old.as_ref().map(|head| head.snapshot_id.clone());
    let members = match &previous_snapshot_id {
        Some(id) => read_snapshot(store, &identity.collection_id, id)?.members,
        None => Vec::new(),
    };
    let snapshot = CollectionSnapshot {
        schema: SCHEMA.into(),
        id: String::new(),
        identity: identity.clone(),
        previous_snapshot_id,
        members,
        completed_listing_refresh: None,
    };
    let snapshot_id = install_snapshot(store, &snapshot)?;
    replace_head(
        store,
        CollectionHead {
            schema: SCHEMA.into(),
            revision: String::new(),
            snapshot_id,
            checkpoint: RefreshCheckpoint {
                identity: identity.clone(),
                job_id: job_id.into(),
                refresh_id: job_id.into(),
                phase: RefreshPhase::Running,
                pending: Vec::new(),
                failures: Vec::new(),
                pages_completed: 0,
                completed_count: 0,
                bytes_read: 0,
                page_open: false,
                has_next_page: true,
                continuation_id: None,
            },
        },
    )
}
fn check_expected(store: &ProjectStore, expected: &CollectionHead) -> Result<()> {
    if read_head(store, &expected.checkpoint.identity.collection_id)?.as_ref() != Some(expected) {
        return Err(CollectionError::Conflict);
    }
    if expected.checkpoint.phase != RefreshPhase::Running {
        return Err(invalid("This refresh is not running."));
    }
    Ok(())
}
fn commit(
    store: &ProjectStore,
    mut head: CollectionHead,
    snapshot: Option<CollectionSnapshot>,
) -> Result<CollectionHead> {
    if let Some(snapshot) = snapshot {
        head.snapshot_id = install_snapshot(store, &snapshot)?;
    }
    validate_head(&head, &head.checkpoint.identity.collection_id)?;
    replace_head(store, head)
}

pub(crate) fn set_page(
    store: &ProjectStore,
    private_root: &Path,
    expected: &CollectionHead,
    members: Vec<RemoteMember>,
    next_cursor: Option<String>,
) -> Result<CollectionHead> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    check_expected(store, expected)?;
    if expected.checkpoint.page_open
        || !expected.checkpoint.has_next_page
        || expected.checkpoint.pages_completed >= MAX_PAGES
        || members.len() > MAX_PAGE_MEMBERS
        || next_cursor
            .as_ref()
            .is_some_and(|cursor| !bounded(cursor, 16 * 1024))
    {
        return Err(invalid(
            "Invalid page transition or collection page limit reached.",
        ));
    }
    let mut head = expected.clone();
    head.checkpoint.pending = members;
    head.checkpoint.page_open = true;
    head.checkpoint.has_next_page = next_cursor.is_some();
    validate_head(&head, &head.checkpoint.identity.collection_id)?;
    // Private continuation is immutable and durable before its opaque ID is
    // exposed by the workspace commit point. Crashes leave only orphan leaves.
    head.checkpoint.continuation_id = Some(private::save_continuation(
        store,
        private_root,
        &head,
        next_cursor,
    )?);
    // Listing observes an existing occurrence even if its subsequent download
    // fails. Never describe that retained item as missing from the provider.
    let mut snapshot = read_snapshot(
        store,
        &head.checkpoint.identity.collection_id,
        &head.snapshot_id,
    )?;
    let mut changed = false;
    for remote in &head.checkpoint.pending {
        let occurrence = occurrence_id(
            &head.checkpoint.identity.provider,
            &head.checkpoint.identity.principal_fingerprint,
            &remote.remote_id,
        )?;
        if let Some(member) = snapshot
            .members
            .iter_mut()
            .find(|member| member.occurrence_id == occurrence)
            && member.observed_in_refresh != head.checkpoint.refresh_id
        {
            member
                .observed_in_refresh
                .clone_from(&head.checkpoint.refresh_id);
            changed = true;
        }
    }
    snapshot.previous_snapshot_id = Some(head.snapshot_id.clone());
    commit(store, head, changed.then_some(snapshot))
}
fn verify_source(store: &ProjectStore, version: &OccurrenceVersion) -> Result<()> {
    let source_bytes =
        context_attachments::source_byte_count(store.root(), &version.attachment_id)?;
    let origin = context_attachments::read_import_origin(store.root(), &version.origin_receipt_id)?;
    let field = |key: &str| origin.get(key).and_then(serde_json::Value::as_str);
    if field("source_sha256") != Some(version.attachment_id.as_str())
        || field("remote_id") != Some(version.remote.remote_id.as_str())
        || field("source_uri") != Some(version.remote.source_uri.as_str())
        || field("service") != Some(version.provider.as_str())
        || field("account_email")
            .is_none_or(|principal| digest(principal.as_bytes()) != version.principal_fingerprint)
        || origin
            .get("source_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(source_bytes)
    {
        return Err(invalid(
            "Acquisition receipt does not describe this source occurrence.",
        ));
    }
    Ok(())
}

pub(crate) fn publish_member(
    store: &ProjectStore,
    expected: &CollectionHead,
    mut version: OccurrenceVersion,
    bytes_read: u64,
) -> Result<CollectionHead> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    check_expected(store, expected)?;
    let identity = &expected.checkpoint.identity;
    let pending = expected
        .checkpoint
        .pending
        .iter()
        .position(|member| *member == version.remote)
        .ok_or_else(|| invalid("This member is not pending on the admitted page."))?;
    if version.provider != identity.provider
        || version.principal_fingerprint != identity.principal_fingerprint
    {
        return Err(invalid(
            "This source belongs to another selected principal.",
        ));
    }
    version
        .observed_in_refresh
        .clone_from(&expected.checkpoint.refresh_id);
    version.validate()?;
    verify_source(store, &version)?;
    version.coverage_complete =
        context_attachments::describe_source(store.root(), &version.attachment_id)?
            .coverage_complete;
    let mut snapshot = read_snapshot(store, &identity.collection_id, &expected.snapshot_id)?;
    snapshot.previous_snapshot_id = Some(expected.snapshot_id.clone());
    if let Some(existing) = snapshot
        .members
        .iter_mut()
        .find(|member| member.occurrence_id == version.occurrence_id)
    {
        *existing = version;
    } else {
        if snapshot.members.len() >= MAX_MEMBERS {
            return Err(invalid("Collection member limit reached."));
        }
        snapshot.members.push(version);
    }
    let mut head = expected.clone();
    head.checkpoint.pending.remove(pending);
    head.checkpoint.completed_count = head
        .checkpoint
        .completed_count
        .checked_add(1)
        .ok_or_else(|| invalid("Collection count overflow."))?;
    head.checkpoint.bytes_read = head
        .checkpoint
        .bytes_read
        .checked_add(bytes_read)
        .ok_or_else(|| invalid("Collection byte count overflow."))?;
    commit(store, head, Some(snapshot))
}
pub(crate) fn fail_member(
    store: &ProjectStore,
    expected: &CollectionHead,
    remote_id: &str,
    message: &str,
    bytes_read: u64,
) -> Result<CollectionHead> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    check_expected(store, expected)?;
    if expected.checkpoint.failures.len() >= MAX_FAILURES || !bounded(message, 2048) {
        return Err(invalid("Collection failure metadata limit reached."));
    }
    let mut head = expected.clone();
    let index = head
        .checkpoint
        .pending
        .iter()
        .position(|member| member.remote_id == remote_id)
        .ok_or_else(|| invalid("This failed member is not pending."))?;
    let member = head.checkpoint.pending.remove(index);
    head.checkpoint.failures.push(MemberFailure {
        remote_id: member.remote_id,
        name: member.name,
        message: message.into(),
    });
    head.checkpoint.bytes_read = head
        .checkpoint
        .bytes_read
        .checked_add(bytes_read)
        .ok_or_else(|| invalid("Collection byte count overflow."))?;
    commit(store, head, None)
}
pub(crate) fn finish_page(
    store: &ProjectStore,
    expected: &CollectionHead,
) -> Result<CollectionHead> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    check_expected(store, expected)?;
    if !expected.checkpoint.page_open || !expected.checkpoint.pending.is_empty() {
        return Err(invalid("Account for every page member before advancing."));
    }
    let mut head = expected.clone();
    head.checkpoint.page_open = false;
    head.checkpoint.pages_completed += 1;
    commit(store, head, None)
}
pub(crate) fn finish_refresh(
    store: &ProjectStore,
    expected: &CollectionHead,
    phase: RefreshPhase,
) -> Result<CollectionHead> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    check_expected(store, expected)?;
    if phase == RefreshPhase::Running {
        return Err(invalid("Choose a terminal refresh phase."));
    }
    let mut head = expected.clone();
    head.checkpoint.phase = phase;
    let snapshot = if phase == RefreshPhase::Complete {
        if head.checkpoint.page_open
            || head.checkpoint.has_next_page
            || !head.checkpoint.pending.is_empty()
        {
            return Err(invalid("The provider listing is not complete."));
        }
        let mut snapshot = read_snapshot(
            store,
            &head.checkpoint.identity.collection_id,
            &head.snapshot_id,
        )?;
        snapshot.previous_snapshot_id = Some(head.snapshot_id.clone());
        snapshot.completed_listing_refresh = Some(head.checkpoint.refresh_id.clone());
        Some(snapshot)
    } else {
        None
    };
    commit(store, head, snapshot)
}

pub(crate) fn fail_refresh(
    store: &ProjectStore,
    expected: &CollectionHead,
    message: &str,
) -> Result<CollectionHead> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    check_expected(store, expected)?;
    if !bounded(message, 2048) || expected.checkpoint.failures.len() >= MAX_FAILURES {
        return Err(invalid("Collection failure metadata limit reached."));
    }
    let mut head = expected.clone();
    head.checkpoint.phase = RefreshPhase::Paused;
    head.checkpoint.failures.push(MemberFailure {
        remote_id: "refresh".into(),
        name: "Collection refresh".into(),
        message: message.into(),
    });
    commit(store, head, None)
}
