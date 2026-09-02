use crate::attachments::{commit_generated_exchange_with_journal, prepare_chat_attachments};
use crate::chat::{
    ChatSendInput, ChatSendOptions, ChatSendOutput, ChatStreamEvent, native_context_messages,
};
use crate::config::{Settings, resolve_settings};
use crate::conversation_store::{
    CONVERSATIONS_NAMESPACE, Conversation, ConversationDb, ConversationExecutionProfile,
    ConversationKind, Message, MessageAttribution, MessageRole, MessageSpeakerKind, active_leaf_id,
    active_path_messages, load_db, strip_reserved_attribution_prefix, upsert_conversation,
};
use crate::kv_cache::ensure_persona_prefix;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use crate::mcp::mcp_platform_blocker;
use crate::mcp::{
    McpCallToolOutput, McpServerConfig, McpTool, cached_persona_mcp_tool_contract,
    exact_mcp_server_config_sha256, load_mcp_db, mcp_call_tool_supervised_with_config,
    validate_frozen_mcp_server,
};
use crate::native_runtime::{
    model_configuration_for_profile, resident_model_for_configuration,
    resident_model_for_fingerprint, resident_model_for_frozen_config, resident_model_for_profile,
};
use crate::now_ms;
use crate::operation_scope::{
    MentionCancelControl, MentionCancelKey, MentionCancelRegistry, OperationScope,
};
use crate::personas::{
    MAX_PERSONA_TOOL_BINDINGS, conversation_and_group_handles, persona_instantiate,
};
use crate::receipts::{Blocker, CommandResult};
use crate::store::{DocumentMutations, DocumentSnapshot, RuntimeStore};
use crate::tool_loop::{ToolPermissionPolicy, tool_permission_policy, validate_tool_arguments};
use anyhow::{Result, anyhow};
use crossbeam_channel::TryRecvError;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use fs2::FileExt;
use llama_native_engine::{ControlledGenerationSubmission, NativeModelHandle};
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, ChatTemplateChoice, ConstraintArtifactReference,
    ControlProgram, ControlledGenerationBatchRequest, ControlledGenerationCase,
    DistributionObservationPolicy, ExactTokenPrompt, ExtendedSamplerProgram, GenerationEventKind,
    GenerationInput, GenerationMetrics, GenerationOutput, GenerationRequest, GenerationState,
    ModelFingerprint, NativeModelConfig, SamplingConfig, SharedPrefixBatchRequest,
    StructuredConstraint, TerminalSelector,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::fs::{File, OpenOptions};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

const INVOCATIONS_NAMESPACE: &str = "mention-invocations.v1";
const ACTIVE_APPROVALS_NAMESPACE: &str = "mention-active-approvals.v1";
const MAX_TARGETS: usize = 4;
const PERSONA_TOOL_DECISION_MAX_TOKENS: u32 = 512;
const PERSONA_TOOL_APPROVAL_TTL_MS: u128 = 5 * 60 * 1000;
const PERSONA_TOOL_CLOCK_ANCHOR_TOLERANCE_MS: u128 = 2_000;
const MAX_ACTIVE_APPROVAL_INVOCATIONS: usize = 64;
const MAX_VISIBLE_TOOL_APPROVALS: usize = 16;
const MAX_PERSONA_TOOL_INPUT_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_PERSONA_TOOL_MANIFEST_BYTES: usize = 128 * 1024;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MentionTargetKind {
    Persona,
    LiveChat,
    Group,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionCandidate {
    pub id: String,
    pub kind: MentionTargetKind,
    pub handle: String,
    pub label: String,
    pub detail: String,
    pub member_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionTargetSnapshot {
    pub target_id: String,
    pub kind: MentionTargetKind,
    pub handle: String,
    pub label: String,
    pub version: u64,
    pub source_leaf_message_id: Option<String>,
    pub profile: ConversationExecutionProfile,
    pub source_messages: Vec<Message>,
    pub snapshot_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionTargetResult {
    pub target_id: String,
    pub handle: String,
    pub label: String,
    pub state: GenerationState,
    pub text: String,
    pub model_id: String,
    pub message_id: Option<String>,
    pub metrics: GenerationMetrics,
    #[serde(default)]
    pub cache_id: Option<String>,
    #[serde(default)]
    pub cache_reused: bool,
    #[serde(default)]
    pub tool_receipt_ids: Vec<String>,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MentionInvocationState {
    Running,
    AwaitingApproval,
    Completed,
    PartiallyCompleted,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MentionToolApprovalDecision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MentionToolApprovalState {
    Pending,
    Resuming,
    Completed,
    Cancelled,
    Failed,
    Expired,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MentionToolEffectOutcome {
    Known,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionToolApproval {
    pub id: String,
    pub invocation_id: String,
    pub host_conversation_id: String,
    pub user_message_id: String,
    pub target_id: String,
    pub handle: String,
    pub label: String,
    pub persona_version: u64,
    pub snapshot_sha256: String,
    pub server: String,
    pub tool: String,
    pub arguments: Value,
    pub arguments_sha256: String,
    #[serde(default)]
    pub server_config_sha256: String,
    #[serde(default)]
    pub tool_schema_sha256: String,
    #[serde(default)]
    pub model_config_sha256: String,
    pub call_sha256: String,
    #[serde(default)]
    pub effect_receipt_id: Option<String>,
    #[serde(default)]
    pub effect_outcome: Option<MentionToolEffectOutcome>,
    #[serde(with = "serde_u128_as_u64")]
    pub created_at_ms: u128,
    #[serde(with = "serde_u128_as_u64")]
    pub expires_at_ms: u128,
    #[serde(default, with = "serde_option_u128_as_u64")]
    pub consumed_at_ms: Option<u128>,
    pub decision: Option<MentionToolApprovalDecision>,
    pub state: MentionToolApprovalState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionInvocation {
    pub id: String,
    pub host_conversation_id: String,
    pub user_message_id: String,
    pub addressed_message: String,
    pub host_context: Vec<Message>,
    pub targets: Vec<MentionTargetSnapshot>,
    pub results: Vec<MentionTargetResult>,
    #[serde(default)]
    pub tool_approvals: Vec<MentionToolApproval>,
    #[serde(default)]
    pub synthesis_message_id: Option<String>,
    #[serde(default)]
    pub synthesis_sha256: Option<String>,
    pub state: MentionInvocationState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FrozenMentionToolContinuation {
    approval_id: String,
    model_path: PathBuf,
    mmproj_path: Option<PathBuf>,
    #[serde(default)]
    model_config: Option<NativeModelConfig>,
    model_fingerprint: ModelFingerprint,
    chat_template: ChatTemplateChoice,
    sampling: SamplingConfig,
    messages: Vec<ChatMessage>,
    provisional_output: GenerationOutput,
    input_schema: Value,
    #[serde(default)]
    mcp_server_config: Option<McpServerConfig>,
    #[serde(default)]
    mcp_server_config_sha256: String,
    #[serde(default)]
    deadline_clock: Option<PersonaToolDeadlineClock>,
    #[serde(default)]
    resume_lease_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct PersonaToolDeadlineClock {
    #[serde(with = "serde_u128_as_u64")]
    monotonic_created_ms: u128,
    #[serde(with = "serde_u128_as_u64")]
    boot_wall_anchor_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StoredMentionInvocation {
    #[serde(flatten)]
    invocation: MentionInvocation,
    #[serde(default, rename = "_frozen_tool_continuations")]
    frozen_tool_continuations: Vec<FrozenMentionToolContinuation>,
}

impl Deref for StoredMentionInvocation {
    type Target = MentionInvocation;

    fn deref(&self) -> &Self::Target {
        &self.invocation
    }
}

impl DerefMut for StoredMentionInvocation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.invocation
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct MentionInvocationDb {
    invocations: Vec<StoredMentionInvocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonaInvocationRemovalImpact {
    pub active_invocation_ids: Vec<String>,
    pub retained_invocation_ids: Vec<String>,
}

pub(crate) fn with_persona_invocation_registry<R>(
    scope: &OperationScope,
    persona_id: &str,
    operation: impl FnOnce(&[String]) -> Result<R>,
) -> Result<R> {
    scope.with_mention_registry(|registry, _| {
        let mut live_invocation_ids = registry
            .keys()
            .filter(|(_, target_id)| target_id == persona_id)
            .map(|(invocation_id, _)| invocation_id.clone())
            .collect::<Vec<_>>();
        live_invocation_ids.sort();
        live_invocation_ids.dedup();
        operation(&live_invocation_ids)
    })
}

pub(crate) fn persona_invocation_impact_from_snapshot(
    snapshot: &DocumentSnapshot<'_, '_, '_>,
    persona_id: &str,
    live_invocation_ids: &[String],
) -> Result<PersonaInvocationRemovalImpact> {
    let db = snapshot
        .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
        .unwrap_or_default();
    Ok(persona_invocation_impact(
        &db,
        persona_id,
        live_invocation_ids,
    ))
}

pub(crate) fn persona_invocation_impact_from_documents(
    documents: &DocumentMutations<'_, '_, '_>,
    persona_id: &str,
    live_invocation_ids: &[String],
) -> Result<PersonaInvocationRemovalImpact> {
    let db = documents
        .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
        .unwrap_or_default();
    Ok(persona_invocation_impact(
        &db,
        persona_id,
        live_invocation_ids,
    ))
}

fn persona_invocation_impact(
    db: &MentionInvocationDb,
    persona_id: &str,
    live_invocation_ids: &[String],
) -> PersonaInvocationRemovalImpact {
    let mut retained_invocation_ids = db
        .invocations
        .iter()
        .filter(|invocation| {
            invocation
                .targets
                .iter()
                .any(|target| target.target_id == persona_id)
        })
        .map(|invocation| invocation.id.clone())
        .collect::<Vec<_>>();
    retained_invocation_ids.sort();
    retained_invocation_ids.dedup();
    let mut active_invocation_ids = db
        .invocations
        .iter()
        .filter(|invocation| {
            matches!(
                invocation.state,
                MentionInvocationState::Running | MentionInvocationState::AwaitingApproval
            ) && invocation
                .targets
                .iter()
                .any(|target| target.target_id == persona_id)
        })
        .map(|invocation| invocation.id.clone())
        .chain(live_invocation_ids.iter().cloned())
        .collect::<Vec<_>>();
    active_invocation_ids.sort();
    active_invocation_ids.dedup();
    PersonaInvocationRemovalImpact {
        active_invocation_ids,
        retained_invocation_ids,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct ActivePersonaToolApprovalIndex {
    approvals: Vec<ActivePersonaToolApproval>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ActivePersonaToolApproval {
    invocation_id: String,
    approval_id: String,
    target_id: String,
    #[serde(with = "serde_u128_as_u64")]
    created_at_ms: u128,
    #[serde(with = "serde_u128_as_u64")]
    expires_at_ms: u128,
    #[serde(default)]
    #[serde(with = "serde_option_u128_as_u64")]
    monotonic_created_ms: Option<u128>,
    #[serde(default)]
    #[serde(with = "serde_option_u128_as_u64")]
    boot_wall_anchor_ms: Option<u128>,
    state: MentionToolApprovalState,
    decision: Option<MentionToolApprovalDecision>,
    resume_lease_id: Option<String>,
}

mod serde_u128_as_u64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(value: &u128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = u64::try_from(*value).map_err(serde::ser::Error::custom)?;
        serializer.serialize_u64(value)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<u128, D::Error>
    where
        D: Deserializer<'de>,
    {
        u64::deserialize(deserializer).map(u128::from)
    }
}

mod serde_option_u128_as_u64 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S>(value: &Option<u128>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value
            .map(u64::try_from)
            .transpose()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Option<u128>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<u64>::deserialize(deserializer).map(|value| value.map(u128::from))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionDispatchInput {
    pub conversation_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatDispatchOutput {
    Direct {
        conversation_id: String,
        output: ChatSendOutput,
    },
    Mention {
        conversation_id: String,
        invocation: MentionInvocation,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "event", rename_all = "snake_case")]
pub enum ChatDispatchStreamEvent {
    Chat(ChatStreamEvent),
    Mention(MentionStreamEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionStreamEvent {
    pub schema: String,
    pub invocation_id: String,
    pub target_id: String,
    pub handle: String,
    pub label: String,
    pub event: String,
    pub delta: Option<String>,
    pub state: Option<GenerationState>,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionCancelOutput {
    pub invocation_id: String,
    pub target_id: Option<String>,
    pub cancelled_sequences: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionSynthesisOutput {
    pub invocation_id: String,
    pub host_conversation_id: String,
    pub message_id: String,
    pub text: String,
    pub model_id: String,
    pub source_message_ids: Vec<String>,
    pub source_content_sha256: Vec<String>,
    pub metrics: GenerationMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionToolApprovalResolution {
    pub approval: MentionToolApproval,
    pub invocation_id: String,
    pub invocation_state: MentionInvocationState,
    pub remaining_approvals: Vec<MentionToolApproval>,
}

// The live-generation witness relies on Unix advisory locks: recovery must be
// able to open and read the locked inode without acquiring it. Unsupported
// platforms never construct this authority; their persisted state is handled
// by the terminal recovery branch below.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct PersonaToolResumeLease {
    file: File,
    lease_id: String,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl PersonaToolResumeLease {
    fn acquire(
        data_dir: &Path,
        invocation_id: &str,
        approval_id: &str,
    ) -> Result<std::result::Result<Self, Blocker>> {
        let path = persona_tool_resume_lease_path(data_dir, invocation_id, approval_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                let lease_id = Uuid::new_v4().to_string();
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(lease_id.as_bytes())?;
                file.sync_data()?;
                Ok(Ok(Self { file, lease_id }))
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(Err(Blocker::new(
                "mention_tool_approval_resume_active",
                "This exact Persona tool approval is already resuming.",
                vec!["Wait for its terminal result before refreshing.".to_string()],
            ))),
            Err(error) => Err(error.into()),
        }
    }

    fn id(&self) -> &str {
        &self.lease_id
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for PersonaToolResumeLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn persona_tool_resume_lease_path(
    data_dir: &Path,
    invocation_id: &str,
    _approval_id: &str,
) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(b"mom_llama.persona_tool_resume_lease.v1\0");
    digest.update(invocation_id.as_bytes());
    data_dir
        .join("operation-leases")
        .join(format!("persona-tool-{:x}.lock", digest.finalize()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn persona_tool_resume_lease_is_live(
    data_dir: &Path,
    invocation_id: &str,
    approval_id: &str,
    expected_lease_id: &str,
) -> Result<bool> {
    let path = persona_tool_resume_lease_path(data_dir, invocation_id, approval_id);
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            FileExt::unlock(&file)?;
            Ok(false)
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => {
            if file.metadata()?.len() > 128 {
                return Ok(false);
            }
            file.seek(SeekFrom::Start(0))?;
            let mut lease_id = String::new();
            file.read_to_string(&mut lease_id)?;
            Ok(lease_id == expected_lease_id)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn resume_lease_is_live_for_recovery(
    data_dir: &Path,
    invocation_id: &str,
    approval_id: &str,
    expected_lease_id: &str,
) -> Result<bool> {
    persona_tool_resume_lease_is_live(data_dir, invocation_id, approval_id, expected_lease_id)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resume_lease_is_live_for_recovery(
    data_dir: &Path,
    invocation_id: &str,
    approval_id: &str,
    expected_lease_id: &str,
) -> Result<bool> {
    let _ = (data_dir, invocation_id, approval_id, expected_lease_id);
    // External-process execution cannot be live under this platform's
    // authority. A persisted Resuming record is therefore an interrupted
    // unknown-effect state and must be terminalized without touching a lock.
    Ok(false)
}

pub fn mention_candidates(
    query: &str,
    current_conversation_id: Option<&str>,
) -> Result<CommandResult<Vec<MentionCandidate>>> {
    let (conversations, groups) = conversation_and_group_handles()?;
    let query = query.trim().trim_start_matches('@').to_ascii_lowercase();
    let mut candidates = conversations
        .into_iter()
        .filter(|conversation| Some(conversation.id.as_str()) != current_conversation_id)
        .filter(|conversation| {
            query.is_empty()
                || conversation
                    .execution_profile
                    .mention_handle
                    .to_ascii_lowercase()
                    .contains(&query)
                || conversation.title.to_ascii_lowercase().contains(&query)
        })
        .map(|conversation| MentionCandidate {
            id: conversation.id,
            kind: if conversation.kind == ConversationKind::PersonaTemplate {
                MentionTargetKind::Persona
            } else {
                MentionTargetKind::LiveChat
            },
            handle: conversation.execution_profile.mention_handle,
            label: conversation.title,
            detail: if conversation.kind == ConversationKind::PersonaTemplate {
                conversation
                    .execution_profile
                    .model_path
                    .as_ref()
                    .and_then(|path| path.file_stem())
                    .and_then(|value| value.to_str())
                    .unwrap_or("Default local model")
                    .to_string()
            } else {
                format!("Updated {}", conversation.updated_at)
            },
            member_count: None,
        })
        .collect::<Vec<_>>();
    candidates.extend(
        groups
            .into_iter()
            .filter(|group| {
                query.is_empty()
                    || group.mention_handle.to_ascii_lowercase().contains(&query)
                    || group.name.to_ascii_lowercase().contains(&query)
            })
            .map(|group| MentionCandidate {
                id: group.id,
                kind: MentionTargetKind::Group,
                handle: group.mention_handle,
                label: group.name,
                detail: format!("{} personas", group.persona_ids.len()),
                member_count: Some(group.persona_ids.len()),
            }),
    );
    candidates.sort_by(|left, right| {
        candidate_rank(left.kind)
            .cmp(&candidate_rank(right.kind))
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });
    Ok(CommandResult::passed(
        "mom_llama.mention_candidates",
        "contracted",
        candidates,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn chat_dispatch(
    input: MentionDispatchInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatDispatchOutput>> {
    let scope = OperationScope::for_current_product_host();
    chat_dispatch_in_scope(&scope, input, options)
}

pub fn chat_dispatch_in_scope(
    scope: &OperationScope,
    input: MentionDispatchInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatDispatchOutput>> {
    chat_dispatch_stream_in_scope(
        scope,
        input,
        options,
        None::<fn(ChatDispatchStreamEvent) -> Result<()>>,
    )
}

pub fn mention_dispatch(
    input: MentionDispatchInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatDispatchOutput>> {
    let scope = OperationScope::for_current_product_host();
    mention_dispatch_in_scope(&scope, input, options)
}

pub fn mention_dispatch_in_scope(
    scope: &OperationScope,
    input: MentionDispatchInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatDispatchOutput>> {
    let mut result = chat_dispatch_in_scope(scope, input, options)?;
    result.command = "mom_llama.mention_dispatch".to_string();
    result.receipt.command = "mom_llama.mention_dispatch".to_string();
    Ok(result)
}

pub fn chat_dispatch_stream<F>(
    input: MentionDispatchInput,
    options: ChatSendOptions,
    on_event: Option<F>,
) -> Result<CommandResult<ChatDispatchOutput>>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    let scope = OperationScope::for_current_product_host();
    chat_dispatch_stream_in_scope(&scope, input, options, on_event)
}

pub fn chat_dispatch_stream_in_scope<F>(
    scope: &OperationScope,
    mut input: MentionDispatchInput,
    options: ChatSendOptions,
    mut on_event: Option<F>,
) -> Result<CommandResult<ChatDispatchOutput>>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    let (_, selected) =
        crate::conversation_store::get_or_create_conversation(&input.conversation_id)?;
    if selected.kind == ConversationKind::PersonaTemplate {
        let instantiated = persona_instantiate(&selected.id, None)?;
        let Some(conversation) = instantiated.result else {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_dispatch",
                &instantiated.readiness,
                instantiated.blocker.unwrap_or_else(|| {
                    Blocker::new(
                        "persona_instantiate_failed",
                        "The persona could not be opened as a chat.",
                        vec!["Try again from Personas in Settings.".to_string()],
                    )
                }),
            ));
        };
        input.conversation_id = conversation.id;
    }
    let handles = parse_handles(&input.message);
    let resolution = resolve_targets(&handles, &input.conversation_id)?;
    if let Some(blocker) = ambiguous_resolution_blocker(&resolution) {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            blocker,
        ));
    }
    if !resolution.unresolved.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "mention_target_not_found",
                format!(
                    "These mentioned participants are unavailable: {}.",
                    resolution
                        .unresolved
                        .iter()
                        .map(|handle| format!("@{handle}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                vec!["Choose a current autocomplete result and retry.".to_string()],
            ),
        ));
    }
    let resolved = resolution.targets;
    if resolved.len() > MAX_TARGETS {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "mention_target_limit_exceeded",
                "A message may invite at most four distinct targets.",
                vec!["Remove one or more mentions and retry.".to_string()],
            ),
        ));
    }
    if resolved.is_empty() {
        let conversation_id = input.conversation_id.clone();
        let output = crate::chat::chat_send_stream_in_scope(
            scope,
            ChatSendInput {
                conversation_id: conversation_id.clone(),
                message: input.message,
            },
            options,
            |event| {
                if let Some(callback) = on_event.as_mut() {
                    callback(ChatDispatchStreamEvent::Chat(event))?;
                }
                Ok(())
            },
        )?;
        if output.status == "blocked" {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_dispatch",
                &output.readiness,
                output.blocker.unwrap_or_else(|| {
                    Blocker::new(
                        "chat_dispatch_blocked",
                        "The local chat request was blocked.",
                        vec!["Check the selected model.".to_string()],
                    )
                }),
            ));
        }
        return Ok(CommandResult::passed(
            "mom_llama.chat_dispatch",
            &output.readiness,
            ChatDispatchOutput::Direct {
                conversation_id,
                output: output.result.expect("passed chat result"),
            },
            output.receipt.changed_paths,
            output.receipt.artifacts_produced,
            output.receipt.real_engine_invoked,
            output.receipt.fake_fixture,
        ));
    }
    dispatch_mentions(scope, input, resolved, options, &mut on_event)
}

pub fn mention_cancel(
    invocation_id: &str,
    target_id: Option<&str>,
) -> Result<CommandResult<MentionCancelOutput>> {
    let scope = OperationScope::for_current_product_host();
    mention_cancel_in_scope(&scope, invocation_id, target_id)
}

pub fn mention_cancel_in_scope(
    scope: &OperationScope,
    invocation_id: &str,
    target_id: Option<&str>,
) -> Result<CommandResult<MentionCancelOutput>> {
    let flagged = request_mention_cancellation(scope, invocation_id, target_id);
    let native = scope.cancel_native(invocation_id, target_id);
    let count = flagged.max(native);
    if count == 0 {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_cancel",
            "stub_blocked",
            Blocker::new(
                "mention_request_not_active",
                "No matching invited response is currently running.",
                vec!["Refresh the conversation.".to_string()],
            ),
        ));
    }
    Ok(CommandResult::passed(
        "mom_llama.mention_cancel",
        "host_integrated",
        MentionCancelOutput {
            invocation_id: invocation_id.to_string(),
            target_id: target_id.map(str::to_string),
            cancelled_sequences: count,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mention_tool_approval_list(
    conversation_id: &str,
) -> Result<CommandResult<Vec<MentionToolApproval>>> {
    let now = now_ms();
    let db = RuntimeStore::current()?
        .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
        .unwrap_or_default();
    let approvals = project_visible_tool_approvals(db, conversation_id, now);
    Ok(CommandResult::passed(
        "mom_llama.mention_tool_approval_list",
        "contracted",
        approvals,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

fn project_visible_tool_approvals(
    db: MentionInvocationDb,
    conversation_id: &str,
    now: u128,
) -> Vec<MentionToolApproval> {
    let mut approvals = db
        .invocations
        .into_iter()
        .filter(|invocation| invocation.host_conversation_id == conversation_id)
        .flat_map(|stored| stored.invocation.tool_approvals)
        .filter_map(|mut approval| {
            if approval.state == MentionToolApprovalState::Pending && now >= approval.expires_at_ms
            {
                // Projection stays read-only, but an expired approval must not
                // disappear while its bounded recovery sweep is pending.
                approval.state = MentionToolApprovalState::Expired;
            }
            (matches!(
                approval.state,
                MentionToolApprovalState::Pending
                    | MentionToolApprovalState::Resuming
                    | MentionToolApprovalState::Cancelled
                    | MentionToolApprovalState::Expired
                    | MentionToolApprovalState::Failed
            ) || approval.effect_receipt_id.is_some())
            .then_some(approval)
        })
        .collect::<Vec<_>>();
    approvals.sort_by(|left, right| {
        let rank = |state| match state {
            MentionToolApprovalState::Pending => 0_u8,
            MentionToolApprovalState::Resuming => 1,
            _ => 2,
        };
        rank(left.state)
            .cmp(&rank(right.state))
            .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
    });
    approvals.truncate(MAX_VISIBLE_TOOL_APPROVALS);
    approvals
}

pub fn mention_tool_approval_decide(
    invocation_id: &str,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
) -> Result<CommandResult<MentionToolApprovalResolution>> {
    let scope = OperationScope::for_current_product_host();
    mention_tool_approval_decide_in_scope(&scope, invocation_id, approval_id, decision)
}

pub fn mention_tool_approval_decide_in_scope(
    scope: &OperationScope,
    invocation_id: &str,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
) -> Result<CommandResult<MentionToolApprovalResolution>> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (scope, invocation_id, approval_id, decision);
        Ok(unsupported_persona_tool_approval_decision())
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let settings = resolve_settings()?;
        let recovery = PersonaToolApprovalRecovery::bind(&settings.data_dir)?;
        mention_tool_approval_decide_with_recovery_in_scope(
            scope,
            invocation_id,
            approval_id,
            decision,
            &recovery,
        )
    }
}

pub fn mention_tool_approval_decide_with_recovery(
    invocation_id: &str,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
    recovery: &PersonaToolApprovalRecovery,
) -> Result<CommandResult<MentionToolApprovalResolution>> {
    let scope = OperationScope::for_current_product_host();
    mention_tool_approval_decide_with_recovery_in_scope(
        &scope,
        invocation_id,
        approval_id,
        decision,
        recovery,
    )
}

pub fn mention_tool_approval_decide_with_recovery_in_scope(
    scope: &OperationScope,
    invocation_id: &str,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
    recovery: &PersonaToolApprovalRecovery,
) -> Result<CommandResult<MentionToolApprovalResolution>> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (scope, invocation_id, approval_id, decision, recovery);
        Ok(unsupported_persona_tool_approval_decision())
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        mention_tool_approval_decide_supported_in_scope(
            scope,
            invocation_id,
            approval_id,
            decision,
            recovery,
        )
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported_persona_tool_approval_decision() -> CommandResult<MentionToolApprovalResolution> {
    let blocker = mcp_platform_blocker()
        .expect("an unsupported platform must provide its typed MCP authority blocker");
    CommandResult::blocked(
        "mom_llama.mention_tool_approval_decide",
        "blocked_platform_unsupported",
        blocker,
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn mention_tool_approval_decide_supported_in_scope(
    scope: &OperationScope,
    invocation_id: &str,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
    recovery: &PersonaToolApprovalRecovery,
) -> Result<CommandResult<MentionToolApprovalResolution>> {
    let settings = resolve_settings()?;
    recovery.ensure_data_dir(&settings.data_dir)?;
    recovery.reconcile_invocation(invocation_id)?;
    let resume_lease =
        match PersonaToolResumeLease::acquire(&settings.data_dir, invocation_id, approval_id)? {
            Ok(lease) => lease,
            Err(blocker) => {
                return Ok(CommandResult::blocked(
                    "mom_llama.mention_tool_approval_decide",
                    "stub_blocked",
                    blocker,
                ));
            }
        };
    let preflight = match persona_tool_approval_preflight(invocation_id, approval_id)? {
        Ok(preflight) => preflight,
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mention_tool_approval_decide",
                "stub_blocked",
                blocker,
            ));
        }
    };
    let target_id = preflight.target_id.clone();
    let cancellation =
        match MentionCancelLifecycle::register_target(scope, invocation_id, &target_id)? {
            Ok(cancellation) => cancellation,
            Err(blocker) => {
                return Ok(CommandResult::blocked(
                    "mom_llama.mention_tool_approval_decide",
                    "stub_blocked",
                    blocker,
                ));
            }
        };
    if cancellation.requested() {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_tool_approval_decide",
            "stub_blocked",
            cancelled_persona_tool_resume(None).blocker,
        ));
    }
    let model = match resident_model_for_frozen_config(
        &settings,
        &preflight.model_config,
        &preflight.model_fingerprint,
    ) {
        Ok(model) => model,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mention_tool_approval_decide",
                "stub_blocked",
                Blocker::new(
                    "mention_tool_approval_model_preflight_failed",
                    blocked.blocker.message,
                    vec![
                        "Restore the exact frozen local model before consuming this approval."
                            .to_string(),
                    ],
                ),
            ));
        }
    };
    if cancellation.requested() {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_tool_approval_decide",
            "stub_blocked",
            cancelled_persona_tool_resume(None).blocker,
        ));
    }
    let status = model.status();
    if status.fingerprint.as_ref() != Some(&preflight.model_fingerprint) {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_tool_approval_decide",
            "stub_blocked",
            Blocker::new(
                "mention_tool_approval_model_mismatch",
                "The loaded local model does not match the frozen Persona invocation.",
                vec![
                    "Restore the exact frozen model configuration before consuming this approval."
                        .to_string(),
                ],
            ),
        ));
    }
    let claim = match claim_persona_tool_approval(
        invocation_id,
        approval_id,
        decision,
        resume_lease.id(),
        &recovery.deadlines,
    )? {
        Ok(claim) => claim,
        Err(blocker) => {
            if blocker.code == "mention_tool_approval_expired" {
                recovery.reconcile_invocation(invocation_id)?;
            }
            return Ok(CommandResult::blocked(
                "mom_llama.mention_tool_approval_decide",
                "stub_blocked",
                blocker,
            ));
        }
    };
    let resumed = if claim.continuation.resume_lease_id.as_deref() == Some(resume_lease.id()) {
        resume_persona_tool_approval(scope, &claim, &settings)
    } else {
        Err(Blocker::new(
            "mention_tool_approval_resume_lease_mismatch",
            "The durable Persona approval claim does not match its live resume lease.",
            vec!["Do not retry the external call; review the frozen invocation.".to_string()],
        )
        .into())
    };
    let (mut result, mut approval_state, mut blocker, effect_receipt) = match resumed {
        Ok(success) => {
            let tool_receipt_ids = success
                .effect_receipt
                .as_ref()
                .map(|receipt| vec![receipt.receipt.task_id.clone()])
                .unwrap_or_default();
            let output = success.output;
            (
                MentionTargetResult {
                    target_id: claim.snapshot.target_id.clone(),
                    handle: claim.snapshot.handle.clone(),
                    label: claim.snapshot.label.clone(),
                    state: output.state,
                    text: strip_reserved_attribution_prefix(&output.text),
                    model_id: output.model_id,
                    message_id: None,
                    metrics: output.metrics,
                    cache_id: None,
                    cache_reused: false,
                    tool_receipt_ids,
                    real_engine_invoked: output.real_engine_invoked,
                    fake_fixture: output.fake_fixture,
                },
                MentionToolApprovalState::Completed,
                None,
                success.effect_receipt,
            )
        }
        Err(failure) => {
            let PersonaToolResumeFailure {
                blocker: failure_blocker,
                effect_receipt: failure_receipt,
                generation_state,
            } = failure;
            let approval_state = if generation_state == GenerationState::Cancelled {
                MentionToolApprovalState::Cancelled
            } else {
                MentionToolApprovalState::Failed
            };
            let mut result =
                blocked_target_result(&claim.snapshot, generation_state, &failure_blocker.message);
            result.tool_receipt_ids = failure_receipt
                .as_ref()
                .map(|receipt| vec![receipt.receipt.task_id.clone()])
                .unwrap_or_default();
            (
                result,
                approval_state,
                Some(failure_blocker),
                failure_receipt.map(|receipt| *receipt),
            )
        }
    };
    if cancellation.arbitrate_terminal(invocation_id, &target_id) {
        let cancellation_failure = cancelled_persona_tool_resume(effect_receipt.clone());
        result = blocked_target_result(
            &claim.snapshot,
            GenerationState::Cancelled,
            &cancellation_failure.blocker.message,
        );
        result.tool_receipt_ids = effect_receipt
            .as_ref()
            .map(|receipt| vec![receipt.receipt.task_id.clone()])
            .unwrap_or_default();
        approval_state = if cancellation_failure.generation_state == GenerationState::Cancelled {
            MentionToolApprovalState::Cancelled
        } else {
            MentionToolApprovalState::Failed
        };
        blocker = Some(cancellation_failure.blocker);
    }
    let invocation = finalize_persona_tool_approval(
        invocation_id,
        approval_id,
        decision,
        result,
        approval_state,
        effect_receipt.as_ref(),
    )?;
    if let Some(blocker) = blocker {
        return Ok(CommandResult::blocked_with_evidence(
            "mom_llama.mention_tool_approval_decide",
            "blocked_native_runtime",
            blocker,
            vec![RuntimeStore::current()?.path().display().to_string()],
            Vec::new(),
            false,
            false,
        ));
    }
    let approval = invocation
        .tool_approvals
        .iter()
        .find(|approval| approval.id == approval_id)
        .cloned()
        .ok_or_else(|| anyhow!("completed Persona tool approval disappeared"))?;
    let remaining_approvals = invocation
        .tool_approvals
        .iter()
        .filter(|candidate| candidate.state == MentionToolApprovalState::Pending)
        .cloned()
        .collect();
    Ok(CommandResult::passed(
        "mom_llama.mention_tool_approval_decide",
        "real_prompt_smoke_passed",
        MentionToolApprovalResolution {
            approval,
            invocation_id: invocation.id,
            invocation_state: invocation.state,
            remaining_approvals,
        },
        vec![RuntimeStore::current()?.path().display().to_string()],
        Vec::new(),
        true,
        false,
    ))
}

#[derive(Debug, Clone)]
struct ClaimedMentionToolApproval {
    approval: MentionToolApproval,
    snapshot: MentionTargetSnapshot,
    continuation: FrozenMentionToolContinuation,
}

struct PersonaToolResumeSuccess {
    output: GenerationOutput,
    effect_receipt: Option<CommandResult<McpCallToolOutput>>,
}

struct PersonaToolResumeFailure {
    blocker: Blocker,
    effect_receipt: Option<Box<CommandResult<McpCallToolOutput>>>,
    generation_state: GenerationState,
}

impl From<Blocker> for PersonaToolResumeFailure {
    fn from(blocker: Blocker) -> Self {
        Self {
            blocker,
            effect_receipt: None,
            generation_state: GenerationState::Failed,
        }
    }
}

fn cancelled_persona_tool_resume(
    effect_receipt: Option<CommandResult<McpCallToolOutput>>,
) -> PersonaToolResumeFailure {
    if let Some(effect_receipt) = effect_receipt.as_ref() {
        if let Some(blocker) = effect_receipt.blocker.as_ref()
            && blocker.code == "mention_tool_effect_outcome_unknown"
        {
            return PersonaToolResumeFailure {
                blocker: blocker.clone(),
                effect_receipt: Some(Box::new(effect_receipt.clone())),
                generation_state: GenerationState::Failed,
            };
        }
        return PersonaToolResumeFailure {
            blocker: Blocker::new(
                "mention_tool_followup_cancelled_after_effect",
                "The approved configured external-process tool completed, but the Persona follow-up was cancelled.",
                vec![
                    "Review the exact persisted tool receipt before starting a new invocation."
                        .to_string(),
                ],
            ),
            effect_receipt: Some(Box::new(effect_receipt.clone())),
            generation_state: GenerationState::Cancelled,
        };
    }
    PersonaToolResumeFailure {
        blocker: Blocker::new(
            "mention_tool_approval_cancelled",
            "The frozen Persona tool approval was cancelled.",
            vec!["Review the partial local evidence before starting a new invocation.".to_string()],
        ),
        effect_receipt: effect_receipt.clone().map(Box::new),
        generation_state: GenerationState::Cancelled,
    }
}

fn unknown_persona_tool_effect(
    claim: &ClaimedMentionToolApproval,
    error: impl std::fmt::Display,
) -> PersonaToolResumeFailure {
    let receipt = unknown_persona_tool_effect_receipt(&claim.approval, error);
    let blocker = receipt
        .blocker
        .clone()
        .expect("unknown-effect receipt must carry its exact blocker");
    PersonaToolResumeFailure {
        blocker,
        effect_receipt: Some(Box::new(receipt)),
        generation_state: GenerationState::Failed,
    }
}

fn unknown_persona_tool_effect_receipt(
    approval: &MentionToolApproval,
    error: impl std::fmt::Display,
) -> CommandResult<McpCallToolOutput> {
    let blocker = Blocker::new(
        "mention_tool_effect_outcome_unknown",
        format!(
            "The approved external MCP process was spawned, but Mom did not observe one exact terminal outcome. The tool request may or may not have been dispatched: {error}"
        ),
        vec![
            "Do not retry this exact call automatically; inspect the external system and the persisted receipt."
                .to_string(),
        ],
    );
    let mut receipt = CommandResult::<McpCallToolOutput>::blocked_with_evidence(
        "mom_llama.mcp_call_tool",
        "effect_outcome_unknown",
        blocker.clone(),
        Vec::new(),
        vec![
            format!("persona_tool_approval_id:{}", approval.id),
            format!("persona_tool_call_sha256:{}", approval.call_sha256),
            "external_process_spawned:yes".to_string(),
            "tool_effect_dispatch:uncertain".to_string(),
            "managed_executable_identity:reviewed_content_addressed_bytes_with_pre_post_spawn_path_checks".to_string(),
            "atomic_path_to_exec_identity:not_asserted".to_string(),
            "dynamic_dependency_identity:not_asserted".to_string(),
            "external_effect_outcome:unknown".to_string(),
        ],
        false,
        false,
    );
    if let Some(receipt_id) = approval.effect_receipt_id.clone() {
        receipt.receipt.task_id = receipt_id;
    }
    receipt
}

fn interrupted_persona_tool_effect_receipt(
    approval: &MentionToolApproval,
    error: impl std::fmt::Display,
) -> CommandResult<McpCallToolOutput> {
    let blocker = Blocker::new(
        "mention_tool_effect_outcome_unknown",
        format!(
            "The app stopped after consuming the exact approval. The MCP request may or may not have been dispatched, so its external outcome is unknown: {error}"
        ),
        vec![
            "Do not retry this exact call automatically; inspect the external system and the persisted receipt."
                .to_string(),
        ],
    );
    let mut receipt = CommandResult::<McpCallToolOutput>::blocked_with_evidence(
        "mom_llama.mcp_call_tool",
        "effect_outcome_unknown",
        blocker,
        Vec::new(),
        vec![
            format!("persona_tool_approval_id:{}", approval.id),
            format!("persona_tool_call_sha256:{}", approval.call_sha256),
            "external_effect_outcome:unknown".to_string(),
            "effect_dispatch:uncertain".to_string(),
        ],
        false,
        false,
    );
    if let Some(receipt_id) = approval.effect_receipt_id.clone() {
        receipt.receipt.task_id = receipt_id;
    }
    receipt
}

fn persona_tool_effect_outcome(
    receipt: &CommandResult<McpCallToolOutput>,
) -> MentionToolEffectOutcome {
    if receipt
        .blocker
        .as_ref()
        .is_some_and(|blocker| blocker.code == "mention_tool_effect_outcome_unknown")
    {
        MentionToolEffectOutcome::Unknown
    } else {
        MentionToolEffectOutcome::Known
    }
}

struct PendingPersonaToolApprovalPreflight {
    target_id: String,
    model_config: NativeModelConfig,
    model_fingerprint: ModelFingerprint,
}

fn persona_tool_approval_preflight(
    invocation_id: &str,
    approval_id: &str,
) -> Result<std::result::Result<PendingPersonaToolApprovalPreflight, Blocker>> {
    let db = RuntimeStore::current()?
        .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
        .unwrap_or_default();
    let Some(stored) = db
        .invocations
        .iter()
        .find(|stored| stored.id == invocation_id)
    else {
        return Ok(Err(Blocker::new(
            "mention_invocation_not_found",
            "The frozen Persona invocation was not found.",
            vec!["Refresh the conversation.".to_string()],
        )));
    };
    let Some(approval) = stored
        .tool_approvals
        .iter()
        .find(|approval| approval.id == approval_id)
    else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_not_found",
            "The exact Persona tool approval was not found.",
            vec!["Refresh the conversation.".to_string()],
        )));
    };
    if approval.state != MentionToolApprovalState::Pending {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_already_consumed",
            "The exact Persona tool approval is no longer pending.",
            vec!["Refresh the conversation.".to_string()],
        )));
    }
    let Some(snapshot) = stored
        .targets
        .iter()
        .find(|snapshot| snapshot.target_id == approval.target_id)
    else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_snapshot_missing",
            "The frozen Persona snapshot is missing.",
            vec!["Run the Persona invocation again.".to_string()],
        )));
    };
    let Some(continuation) = stored
        .frozen_tool_continuations
        .iter()
        .find(|continuation| continuation.approval_id == approval.id)
    else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_continuation_missing",
            "The frozen Persona continuation is missing.",
            vec!["Run the Persona invocation again.".to_string()],
        )));
    };
    if let Some(blocker) =
        validate_frozen_persona_tool_approval(&stored.invocation, approval, snapshot, continuation)?
    {
        return Ok(Err(blocker));
    }
    let Some(model_config) = continuation.model_config.clone() else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_frozen_model_config_missing",
            "This legacy Persona approval does not contain the exact private model configuration needed for safe resume.",
            vec!["Run a new Persona invocation.".to_string()],
        )));
    };
    Ok(Ok(PendingPersonaToolApprovalPreflight {
        target_id: approval.target_id.clone(),
        model_config,
        model_fingerprint: continuation.model_fingerprint.clone(),
    }))
}

fn claim_persona_tool_approval(
    invocation_id: &str,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
    resume_lease_id: &str,
    deadlines: &ApprovalDeadlineTracker,
) -> Result<std::result::Result<ClaimedMentionToolApproval, Blocker>> {
    let now = now_ms();
    let monotonic_now = platform_monotonic_ms()?;
    RuntimeStore::current()?.mutate_documents(
        INVOCATIONS_NAMESPACE,
        MentionInvocationDb::default,
        |db, documents| {
            let Some(stored) = db
                .invocations
                .iter_mut()
                .find(|stored| stored.id == invocation_id)
            else {
                return Ok(Err(Blocker::new(
                    "mention_invocation_not_found",
                    "The frozen Persona invocation was not found.",
                    vec!["Refresh the conversation.".to_string()],
                )));
            };
            let force_expired = stored
                .tool_approvals
                .iter()
                .find(|approval| approval.id == approval_id)
                .filter(|approval| approval.state == MentionToolApprovalState::Pending)
                .map(|approval| {
                    let persisted_clock = stored
                        .frozen_tool_continuations
                        .iter()
                        .find(|continuation| continuation.approval_id == approval.id)
                        .and_then(|continuation| continuation.deadline_clock);
                    deadlines.observe(
                        &approval.id,
                        approval.created_at_ms,
                        approval.expires_at_ms,
                        persisted_clock,
                        now,
                        monotonic_now,
                    )?;
                    deadlines.due(&approval.id, monotonic_now)
                })
                .transpose()?
                .unwrap_or(false);
            let claim = claim_stored_persona_tool_approval(
                stored,
                approval_id,
                decision,
                now,
                resume_lease_id,
                force_expired,
            )?;
            if claim.is_ok() {
                write_active_approval_index(db, documents)?;
            }
            Ok(claim)
        },
    )
}

fn claim_stored_persona_tool_approval(
    stored: &mut StoredMentionInvocation,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
    now: u128,
    resume_lease_id: &str,
    force_expired: bool,
) -> Result<std::result::Result<ClaimedMentionToolApproval, Blocker>> {
    let Some(approval_index) = stored
        .tool_approvals
        .iter()
        .position(|approval| approval.id == approval_id)
    else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_not_found",
            "The exact Persona tool approval was not found.",
            vec!["Refresh the conversation.".to_string()],
        )));
    };
    let approval = &stored.tool_approvals[approval_index];
    if approval.state == MentionToolApprovalState::Expired {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_expired",
            "The Persona tool approval expired before it was used.",
            vec!["Run the Persona invocation again.".to_string()],
        )));
    }
    if approval.state != MentionToolApprovalState::Pending {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_consumed",
            "This one-use Persona tool approval is no longer pending.",
            vec!["Review the completed or failed invocation.".to_string()],
        )));
    }
    if now >= approval.expires_at_ms || force_expired {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_expired",
            "The Persona tool approval expired before it was used.",
            vec!["Run the Persona invocation again.".to_string()],
        )));
    }
    let Some(snapshot) = stored
        .targets
        .iter()
        .find(|snapshot| snapshot.target_id == approval.target_id)
        .cloned()
    else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_snapshot_missing",
            "The frozen Persona snapshot is missing.",
            vec!["Run the Persona invocation again.".to_string()],
        )));
    };
    let Some(continuation_index) = stored
        .frozen_tool_continuations
        .iter()
        .position(|continuation| continuation.approval_id == approval_id)
    else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_continuation_missing",
            "The frozen Persona continuation is missing.",
            vec!["Run the Persona invocation again.".to_string()],
        )));
    };
    let continuation = stored.frozen_tool_continuations[continuation_index].clone();
    if let Some(blocker) = validate_frozen_persona_tool_approval(
        &stored.invocation,
        approval,
        &snapshot,
        &continuation,
    )? {
        return Ok(Err(blocker));
    }
    let effect_receipt_id =
        (decision == MentionToolApprovalDecision::Approve).then(persona_tool_effect_receipt_id);
    {
        let approval = &mut stored.tool_approvals[approval_index];
        approval.state = MentionToolApprovalState::Resuming;
        approval.decision = Some(decision);
        approval.consumed_at_ms = Some(now);
        approval.effect_receipt_id = effect_receipt_id;
        approval.effect_outcome = None;
    }
    stored.frozen_tool_continuations[continuation_index].resume_lease_id =
        Some(resume_lease_id.to_string());
    let continuation = stored.frozen_tool_continuations[continuation_index].clone();
    let approval = stored.tool_approvals[approval_index].clone();
    stored.updated_at = now.to_string();
    Ok(Ok(ClaimedMentionToolApproval {
        approval,
        snapshot,
        continuation,
    }))
}

fn validate_frozen_persona_tool_approval(
    invocation: &MentionInvocation,
    approval: &MentionToolApproval,
    snapshot: &MentionTargetSnapshot,
    continuation: &FrozenMentionToolContinuation,
) -> Result<Option<Blocker>> {
    let arguments_sha256 = sha256_json_value(&approval.arguments)?;
    let Some(model_config) = continuation.model_config.as_ref() else {
        return Ok(Some(Blocker::new(
            "mention_tool_approval_frozen_model_config_missing",
            "The exact private model configuration is missing from this legacy approval.",
            vec!["Run a new Persona invocation.".to_string()],
        )));
    };
    let model_config_sha256 = sha256_json(model_config)?;
    let Some(frozen_server_config) = continuation.mcp_server_config.as_ref() else {
        return Ok(Some(Blocker::new(
            "mention_tool_approval_frozen_server_missing",
            "The reviewed staged MCP executable is missing from this legacy approval.",
            vec!["Run a new Persona invocation.".to_string()],
        )));
    };
    let frozen_server_config_sha256 = exact_mcp_server_config_sha256(frozen_server_config)?;
    let call_sha256 = persona_tool_call_sha256(PersonaToolCallIdentity {
        invocation_id: &invocation.id,
        host_conversation_id: &invocation.host_conversation_id,
        user_message_id: &invocation.user_message_id,
        target_id: &snapshot.target_id,
        persona_version: snapshot.version,
        snapshot_sha256: &snapshot.snapshot_sha256,
        server: &approval.server,
        tool: &approval.tool,
        arguments_sha256: &arguments_sha256,
        server_config_sha256: &approval.server_config_sha256,
        frozen_server_config_sha256: &frozen_server_config_sha256,
        tool_schema_sha256: &approval.tool_schema_sha256,
        model_config_sha256: &model_config_sha256,
        model_fingerprint: &continuation.model_fingerprint,
    })?;
    let frozen_schema_sha256 = sha256_json_value(&continuation.input_schema)?;
    let exact = approval.invocation_id == invocation.id
        && approval.host_conversation_id == invocation.host_conversation_id
        && approval.user_message_id == invocation.user_message_id
        && approval.target_id == snapshot.target_id
        && approval.persona_version == snapshot.version
        && approval.snapshot_sha256 == snapshot.snapshot_sha256
        && approval.arguments_sha256 == arguments_sha256
        && approval.tool_schema_sha256 == frozen_schema_sha256
        && approval.model_config_sha256 == model_config_sha256
        && model_config.model_path.as_path() == continuation.model_path.as_path()
        && model_config.mmproj_path.as_deref() == continuation.mmproj_path.as_deref()
        && model_config.expected_model_sha256.as_deref()
            == Some(continuation.model_fingerprint.model_sha256.as_str())
        && model_config.expected_mmproj_sha256.as_deref()
            == continuation
                .model_fingerprint
                .multimodal_projector_sha256
                .as_deref()
        && !approval.server_config_sha256.is_empty()
        && !continuation.mcp_server_config_sha256.is_empty()
        && continuation.mcp_server_config_sha256 == frozen_server_config_sha256
        && approval.call_sha256 == call_sha256
        && approval.effect_receipt_id.is_none()
        && approval.effect_outcome.is_none()
        && continuation.approval_id == approval.id;
    if !exact {
        return Ok(Some(Blocker::new(
            "mention_tool_approval_mismatch",
            "The Persona approval no longer matches its frozen invocation and exact call.",
            vec!["Run the Persona invocation again.".to_string()],
        )));
    }
    Ok(validate_tool_arguments(
        &continuation.input_schema,
        &approval.arguments,
    ))
}

fn validate_current_persona_tool_binding(
    approval: &MentionToolApproval,
    continuation: &FrozenMentionToolContinuation,
    data_dir: &Path,
    should_cancel: &dyn Fn() -> bool,
) -> Result<std::result::Result<McpServerConfig, Blocker>> {
    let server_db = load_mcp_db()?;
    let Some(server) = server_db
        .servers
        .iter()
        .find(|server| server.name == approval.server && server.enabled)
    else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_server_changed",
            "The approved MCP server is missing or disabled.",
            vec!["Run the Persona invocation again after reviewing the server.".to_string()],
        )));
    };
    if exact_mcp_server_config_sha256(server)? != approval.server_config_sha256 {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_server_changed",
            "The approved MCP executable or configuration changed before execution.",
            vec!["Review the server and run a new Persona invocation.".to_string()],
        )));
    }
    if should_cancel() {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_cancelled",
            "The exact Persona tool approval was cancelled before MCP admission.",
            vec!["Review the frozen invocation before retrying.".to_string()],
        )));
    }
    let Some(frozen_server) = continuation.mcp_server_config.as_ref() else {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_frozen_server_missing",
            "The reviewed staged MCP executable is missing from this approval.",
            vec!["Run a new Persona invocation.".to_string()],
        )));
    };
    if let Err(error) = validate_frozen_mcp_server(
        frozen_server,
        data_dir,
        &continuation.mcp_server_config_sha256,
    ) {
        return Ok(Err(Blocker::new(
            "mention_tool_approval_frozen_server_changed",
            error.to_string(),
            vec!["Run a new Persona invocation after restoring managed storage.".to_string()],
        )));
    }
    Ok(Ok(frozen_server.clone()))
}

fn resume_persona_tool_approval(
    scope: &OperationScope,
    claim: &ClaimedMentionToolApproval,
    settings: &Settings,
) -> std::result::Result<PersonaToolResumeSuccess, PersonaToolResumeFailure> {
    if mention_cancellation_requested(
        scope,
        &claim.approval.invocation_id,
        &claim.approval.target_id,
    ) {
        return Err(cancelled_persona_tool_resume(None));
    }
    if claim.approval.decision == Some(MentionToolApprovalDecision::Approve)
        && tool_permission_policy(&claim.approval.server, &claim.approval.tool).map_err(
            |error| {
                Blocker::new(
                    "mention_tool_permission_unavailable",
                    error.to_string(),
                    vec!["Review the configured external-process tool permission.".to_string()],
                )
            },
        )? == ToolPermissionPolicy::Deny
    {
        return Err(Blocker::new(
            "tool_permission_denied",
            format!(
                "Tool `{}/{}` was denied after the approval was prepared.",
                claim.approval.server, claim.approval.tool
            ),
            vec!["Run the Persona invocation again if policy changes.".to_string()],
        )
        .into());
    }
    let validated_server = if claim.approval.decision == Some(MentionToolApprovalDecision::Approve)
    {
        let validation = validate_current_persona_tool_binding(
            &claim.approval,
            &claim.continuation,
            &settings.data_dir,
            &|| {
                mention_cancellation_requested(
                    scope,
                    &claim.approval.invocation_id,
                    &claim.approval.target_id,
                )
            },
        );
        let validation = match validation {
            Ok(validation) => validation,
            Err(_)
                if mention_cancellation_requested(
                    scope,
                    &claim.approval.invocation_id,
                    &claim.approval.target_id,
                ) =>
            {
                return Err(cancelled_persona_tool_resume(None));
            }
            Err(error) => {
                return Err(Blocker::new(
                    "mention_tool_approval_binding_recheck_failed",
                    error.to_string(),
                    vec!["Review the attached MCP server before retrying.".to_string()],
                )
                .into());
            }
        };
        match validation {
            Ok(server) => Some(server),
            Err(blocker) => return Err(blocker.into()),
        }
    } else {
        None
    };
    if mention_cancellation_requested(
        scope,
        &claim.approval.invocation_id,
        &claim.approval.target_id,
    ) {
        return Err(cancelled_persona_tool_resume(None));
    }
    let handle = resident_model_for_fingerprint(settings, &claim.continuation.model_fingerprint)
        .map_err(|blocked| blocked.blocker)?;
    if mention_cancellation_requested(
        scope,
        &claim.approval.invocation_id,
        &claim.approval.target_id,
    ) {
        return Err(cancelled_persona_tool_resume(None));
    }
    let status = handle.status();
    if status.fingerprint.as_ref() != Some(&claim.continuation.model_fingerprint) {
        return Err(Blocker::new(
            "mention_tool_approval_model_mismatch",
            "The resident model no longer matches the frozen Persona invocation.",
            vec!["Restore the exact model or run the Persona invocation again.".to_string()],
        )
        .into());
    }
    let (tool_content, effect_receipt) = match claim.approval.decision {
        Some(MentionToolApprovalDecision::Approve) => {
            if mention_cancellation_requested(
                scope,
                &claim.approval.invocation_id,
                &claim.approval.target_id,
            ) {
                return Err(cancelled_persona_tool_resume(None));
            }
            let server = validated_server.as_ref().ok_or_else(|| {
                Blocker::new(
                    "mention_tool_approval_server_identity_missing",
                    "The approved reviewed MCP configuration and staged-byte identity are missing.",
                    vec!["Run a new Persona invocation.".to_string()],
                )
            })?;
            if let Err(error) = validate_frozen_mcp_server(
                server,
                &settings.data_dir,
                &claim.continuation.mcp_server_config_sha256,
            ) {
                return Err(Blocker::new(
                    "mention_tool_approval_frozen_server_changed",
                    error.to_string(),
                    vec![
                        "Run a new Persona invocation after restoring managed storage.".to_string(),
                    ],
                )
                .into());
            }
            if exact_mcp_server_config_sha256(server).map_err(|error| {
                Blocker::new(
                    "mention_tool_approval_server_recheck_failed",
                    error.to_string(),
                    vec!["Review the managed MCP executable.".to_string()],
                )
            })? != claim.continuation.mcp_server_config_sha256
            {
                return Err(Blocker::new(
                    "mention_tool_approval_server_changed",
                    "The managed MCP executable changed at the spawn boundary.",
                    vec!["Review the server and run a new Persona invocation.".to_string()],
                )
                .into());
            }
            let mut call = mcp_call_tool_supervised_with_config(
                scope,
                server,
                &claim.approval.tool,
                claim.approval.arguments.clone(),
                &claim.continuation.mcp_server_config_sha256,
                &claim.approval.tool_schema_sha256,
                &|| {
                    mention_cancellation_requested(
                        scope,
                        &claim.approval.invocation_id,
                        &claim.approval.target_id,
                    )
                },
            )
            .map_err(|error| {
                if error.outcome_unknown() {
                    unknown_persona_tool_effect(claim, error)
                } else if mention_cancellation_requested(
                    scope,
                    &claim.approval.invocation_id,
                    &claim.approval.target_id,
                ) {
                    cancelled_persona_tool_resume(None)
                } else {
                    Blocker::new(
                        "mention_tool_call_failed",
                        error.to_string(),
                        vec!["Check the attached MCP server in Settings.".to_string()],
                    )
                    .into()
                }
            })?;
            let receipt_id = claim.approval.effect_receipt_id.clone().ok_or_else(|| {
                Blocker::new(
                    "mention_tool_receipt_identity_missing",
                    "The consumed Persona approval has no exact effect receipt identity.",
                    vec!["Review the frozen invocation.".to_string()],
                )
            })?;
            call.receipt.task_id = receipt_id;
            call.receipt.artifacts_produced.extend([
                format!("persona_tool_approval_id:{}", claim.approval.id),
                format!("persona_tool_call_sha256:{}", claim.approval.call_sha256),
                "managed_executable_identity:reviewed_content_addressed_bytes_with_pre_post_spawn_path_checks".to_string(),
                "atomic_path_to_exec_identity:not_asserted".to_string(),
                "dynamic_dependency_identity:not_asserted".to_string(),
            ]);
            if call.status == "blocked" {
                let blocker = call.blocker.clone().unwrap_or_else(|| {
                    Blocker::new(
                        "mention_tool_call_blocked",
                        "The approved exact configured external-process tool call was blocked.",
                        vec!["Review the MCP server and exact approval.".to_string()],
                    )
                });
                return Err(PersonaToolResumeFailure {
                    blocker,
                    effect_receipt: Some(Box::new(call)),
                    generation_state: GenerationState::Failed,
                });
            }
            (
                call.result
                    .as_ref()
                    .map(|result| result.content.clone())
                    .unwrap_or(Value::Null),
                Some(call),
            )
        }
        Some(MentionToolApprovalDecision::Deny) => (
            json!({
                "error": {
                    "code": "persona_tool_call_denied",
                    "message": "The user denied this exact tool call. Continue without calling it."
                }
            }),
            None,
        ),
        None => {
            return Err(Blocker::new(
                "mention_tool_approval_decision_missing",
                "The claimed Persona tool approval has no decision.",
                vec!["Review the frozen invocation.".to_string()],
            )
            .into());
        }
    };
    if mention_cancellation_requested(
        scope,
        &claim.approval.invocation_id,
        &claim.approval.target_id,
    ) {
        return Err(cancelled_persona_tool_resume(effect_receipt));
    }
    let mut messages = claim.continuation.messages.clone();
    messages.push(ChatMessage {
        role: ChatRole::Assistant,
        content: claim.continuation.provisional_output.text.clone(),
    });
    let tool_message = serde_json::to_string(&json!({
        "server": claim.approval.server,
        "tool": claim.approval.tool,
        "arguments": claim.approval.arguments,
        "decision": claim.approval.decision,
        "result": tool_content,
    }))
    .map_err(|error| PersonaToolResumeFailure {
        blocker: Blocker::new(
            "mention_tool_followup_input_failed",
            error.to_string(),
            vec!["Review the frozen exact call.".to_string()],
        ),
        effect_receipt: effect_receipt.clone().map(Box::new),
        generation_state: GenerationState::Failed,
    })?;
    messages.push(ChatMessage {
        role: ChatRole::Tool,
        content: tool_message,
    });
    if mention_cancellation_requested(
        scope,
        &claim.approval.invocation_id,
        &claim.approval.target_id,
    ) {
        return Err(cancelled_persona_tool_resume(effect_receipt));
    }
    // Keep the child request identity identical to the invocation/target pair
    // used by mention cancellation. The completed initial batch has already
    // released this exact route before an approval can be claimed.
    let ticket = handle
        .generate_shared_prefix(SharedPrefixBatchRequest {
            request_id: claim.approval.invocation_id.clone(),
            model_id: status.model_id,
            common_messages: Vec::new(),
            chat_template: claim.continuation.chat_template.clone(),
            branches: vec![BranchRequest {
                branch_id: claim.approval.target_id.clone(),
                label: claim.approval.label.clone(),
                instruction: String::new(),
                sampling: claim.continuation.sampling.clone(),
                messages,
                cached_prefix: None,
            }],
            cached_prefix: None,
        })
        .map_err(|error| PersonaToolResumeFailure {
            blocker: Blocker::new(
                "mention_tool_followup_failed",
                error.message,
                vec!["Retry by starting a new Persona invocation.".to_string()],
            ),
            effect_receipt: effect_receipt.clone().map(Box::new),
            generation_state: GenerationState::Failed,
        })?;
    let output = ticket
        .wait()
        .map_err(|error| PersonaToolResumeFailure {
            blocker: Blocker::new(
                "mention_tool_followup_failed",
                error.message,
                vec!["Retry by starting a new Persona invocation.".to_string()],
            ),
            effect_receipt: effect_receipt.clone().map(Box::new),
            generation_state: GenerationState::Failed,
        })?
        .into_iter()
        .next()
        .ok_or_else(|| PersonaToolResumeFailure {
            blocker: Blocker::new(
                "mention_tool_followup_missing",
                "The frozen Persona returned no final answer after the decision.",
                vec!["Run a new Persona invocation.".to_string()],
            ),
            effect_receipt: effect_receipt.clone().map(Box::new),
            generation_state: GenerationState::Failed,
        })?;
    if output.branch_id != claim.approval.target_id
        || output.state != GenerationState::Completed
        || !output.real_engine_invoked
        || output.fake_fixture
    {
        return Err(PersonaToolResumeFailure {
            blocker: Blocker::new(
                "mention_tool_followup_incomplete",
                "The frozen Persona did not complete one real final answer after the decision.",
                vec!["Review the partial result and run a new invocation.".to_string()],
            ),
            effect_receipt: effect_receipt.clone().map(Box::new),
            generation_state: GenerationState::Failed,
        });
    }
    if mention_cancellation_requested(
        scope,
        &claim.approval.invocation_id,
        &claim.approval.target_id,
    ) {
        return Err(cancelled_persona_tool_resume(effect_receipt));
    }
    Ok(PersonaToolResumeSuccess {
        output,
        effect_receipt,
    })
}

fn finalize_persona_tool_approval(
    invocation_id: &str,
    approval_id: &str,
    decision: MentionToolApprovalDecision,
    result: MentionTargetResult,
    approval_state: MentionToolApprovalState,
    effect_receipt: Option<&CommandResult<McpCallToolOutput>>,
) -> Result<MentionInvocation> {
    let migrated_conversations = load_db()?;
    RuntimeStore::current()?.mutate_documents(
        INVOCATIONS_NAMESPACE,
        MentionInvocationDb::default,
        |db, documents| {
            let mut conversations = documents
                .get(CONVERSATIONS_NAMESPACE)?
                .unwrap_or(migrated_conversations);
            let stored = db
                .invocations
                .iter_mut()
                .find(|stored| stored.id == invocation_id)
                .ok_or_else(|| anyhow!("frozen Persona invocation disappeared while resuming"))?;
            let approval_index = stored
                .tool_approvals
                .iter()
                .position(|approval| approval.id == approval_id)
                .ok_or_else(|| {
                    anyhow!("frozen Persona tool approval disappeared while resuming")
                })?;
            let approval = &stored.tool_approvals[approval_index];
            if approval.state != MentionToolApprovalState::Resuming
                || approval.decision != Some(decision)
            {
                anyhow::bail!("Persona tool approval state changed while resuming");
            }
            if stored
                .results
                .iter()
                .any(|candidate| candidate.target_id == result.target_id)
            {
                anyhow::bail!("Persona target already has a terminal result");
            }
            match effect_receipt {
                Some(effect_receipt) => {
                    if approval.effect_receipt_id.as_deref()
                        != Some(effect_receipt.receipt.task_id.as_str())
                        || result.tool_receipt_ids.as_slice()
                            != std::slice::from_ref(&effect_receipt.receipt.task_id)
                    {
                        anyhow::bail!(
                            "Persona tool result does not match its exact persisted effect receipt"
                        );
                    }
                    documents.put_receipt(
                        &effect_receipt.receipt.task_id,
                        &effect_receipt.receipt.command,
                        &effect_receipt.receipt,
                    )?;
                    stored.tool_approvals[approval_index].effect_outcome =
                        Some(persona_tool_effect_outcome(effect_receipt));
                }
                None if !result.tool_receipt_ids.is_empty() => {
                    anyhow::bail!("Persona tool result references a missing effect receipt");
                }
                None => {
                    stored.tool_approvals[approval_index].effect_receipt_id = None;
                    stored.tool_approvals[approval_index].effect_outcome = None;
                }
            }
            stored.tool_approvals[approval_index].state = approval_state;
            stored.results.push(result);
            stored
                .frozen_tool_continuations
                .retain(|continuation| continuation.approval_id != approval_id);
            finish_stored_mention_invocation(stored, &mut conversations);
            let invocation = stored.invocation.clone();
            write_active_approval_index(db, documents)?;
            crate::personas::reject_removed_conversation_writes_from_documents(
                &conversations,
                documents,
            )?;
            documents.put_bytes(
                CONVERSATIONS_NAMESPACE,
                &serde_json::to_vec(&conversations)?,
            )?;
            Ok(invocation)
        },
    )
}

#[derive(Default)]
struct ApprovalDeadlineTracker {
    deadlines: Mutex<BTreeMap<String, u128>>,
}

impl ApprovalDeadlineTracker {
    fn observe(
        &self,
        approval_id: &str,
        created_at_ms: u128,
        expires_at_ms: u128,
        persisted_clock: Option<PersonaToolDeadlineClock>,
        wall_now_ms: u128,
        monotonic_now_ms: u128,
    ) -> Result<()> {
        let durable_deadline_valid =
            expires_at_ms == created_at_ms.saturating_add(PERSONA_TOOL_APPROVAL_TTL_MS);
        let deadline = if !durable_deadline_valid || wall_now_ms < created_at_ms {
            monotonic_now_ms
        } else {
            match persisted_clock {
                Some(clock) => {
                    let current_anchor = wall_now_ms.saturating_sub(monotonic_now_ms);
                    let clock_discontinuity = monotonic_now_ms < clock.monotonic_created_ms
                        || current_anchor.abs_diff(clock.boot_wall_anchor_ms)
                            > PERSONA_TOOL_CLOCK_ANCHOR_TOLERANCE_MS;
                    if clock_discontinuity {
                        monotonic_now_ms
                    } else {
                        clock
                            .monotonic_created_ms
                            .saturating_add(PERSONA_TOOL_APPROVAL_TTL_MS)
                    }
                }
                None => monotonic_now_ms,
            }
        };
        let mut deadlines = self
            .deadlines
            .lock()
            .map_err(|_| anyhow!("Persona approval deadline tracker is unavailable"))?;
        deadlines
            .entry(approval_id.to_string())
            .and_modify(|current| *current = (*current).min(deadline))
            .or_insert(deadline);
        Ok(())
    }

    fn due(&self, approval_id: &str, monotonic_now_ms: u128) -> Result<bool> {
        let deadlines = self
            .deadlines
            .lock()
            .map_err(|_| anyhow!("Persona approval deadline tracker is unavailable"))?;
        Ok(deadlines
            .get(approval_id)
            .is_some_and(|deadline| monotonic_now_ms >= *deadline))
    }

    fn retain(&self, approval_ids: &BTreeSet<&str>) -> Result<()> {
        self.deadlines
            .lock()
            .map_err(|_| anyhow!("Persona approval deadline tracker is unavailable"))?
            .retain(|approval_id, _| approval_ids.contains(approval_id.as_str()));
        Ok(())
    }
}

fn platform_monotonic_ms() -> Result<u128> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let now = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)?;
        let seconds = u128::try_from(now.tv_sec())
            .map_err(|_| anyhow!("platform monotonic clock returned a negative second"))?;
        let nanoseconds = u128::try_from(now.tv_nsec())
            .map_err(|_| anyhow!("platform monotonic clock returned a negative nanosecond"))?;
        Ok(seconds
            .saturating_mul(1_000)
            .saturating_add(nanoseconds / 1_000_000))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    anyhow::bail!("durable monotonic Persona approval deadlines are unsupported on this platform")
}

fn capture_persona_tool_deadline_clock(wall_now_ms: u128) -> Result<PersonaToolDeadlineClock> {
    let monotonic_created_ms = platform_monotonic_ms()?;
    Ok(PersonaToolDeadlineClock {
        monotonic_created_ms,
        boot_wall_anchor_ms: wall_now_ms.saturating_sub(monotonic_created_ms),
    })
}

#[derive(Clone)]
pub struct PersonaToolApprovalRecovery {
    store: RuntimeStore,
    data_dir: PathBuf,
    deadlines: Arc<ApprovalDeadlineTracker>,
}

impl PersonaToolApprovalRecovery {
    pub fn bind(data_dir: &Path) -> Result<Self> {
        let store = RuntimeStore::open(data_dir)?;
        let data_dir = data_dir.canonicalize()?;
        if store
            .get::<ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
            .is_none()
        {
            store.mutate_documents(
                INVOCATIONS_NAMESPACE,
                MentionInvocationDb::default,
                |db, documents| write_active_approval_index(db, documents),
            )?;
        }
        Ok(Self {
            store,
            data_dir,
            deadlines: Arc::new(ApprovalDeadlineTracker::default()),
        })
    }

    fn ensure_data_dir(&self, data_dir: &Path) -> Result<()> {
        if data_dir.canonicalize()? != self.data_dir {
            anyhow::bail!("Persona approval recovery is bound to a different product store");
        }
        Ok(())
    }

    pub fn reconcile(&self) -> Result<()> {
        reconcile_persona_tool_approval_set(None, &self.data_dir, &self.store, &self.deadlines)
    }

    pub fn reconcile_invocation(&self, invocation_id: &str) -> Result<()> {
        reconcile_persona_tool_approval_set(
            Some(invocation_id),
            &self.data_dir,
            &self.store,
            &self.deadlines,
        )
    }

    pub fn observe_invocation(&self, invocation_id: &str) -> Result<()> {
        self.reconcile_invocation(invocation_id)
    }
}

pub fn reconcile_persona_tool_approvals() -> Result<()> {
    let settings = resolve_settings()?;
    PersonaToolApprovalRecovery::bind(&settings.data_dir)?.reconcile()
}

pub fn reconcile_persona_tool_approvals_command() -> Result<CommandResult<bool>> {
    let settings = resolve_settings()?;
    let recovery = PersonaToolApprovalRecovery::bind(&settings.data_dir)?;
    recovery.reconcile()?;
    Ok(CommandResult::passed(
        "mom_llama.persona_tool_approval_recover",
        "contracted",
        true,
        vec![recovery.store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn reconcile_persona_tool_approval_set(
    only_invocation_id: Option<&str>,
    data_dir: &Path,
    store: &RuntimeStore,
    deadlines: &ApprovalDeadlineTracker,
) -> Result<()> {
    let now = now_ms();
    let index = store
        .get::<ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
        .unwrap_or_default();
    if index.approvals.is_empty() {
        return Ok(());
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let monotonic_now = platform_monotonic_ms()?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let monotonic_now = 0;
    let pending_ids = index
        .approvals
        .iter()
        .filter(|approval| approval.state == MentionToolApprovalState::Pending)
        .map(|approval| approval.approval_id.as_str())
        .collect::<BTreeSet<_>>();
    deadlines.retain(&pending_ids)?;
    for approval in index
        .approvals
        .iter()
        .filter(|approval| approval.state == MentionToolApprovalState::Pending)
    {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let persisted_clock =
                match (approval.monotonic_created_ms, approval.boot_wall_anchor_ms) {
                    (Some(monotonic_created_ms), Some(boot_wall_anchor_ms)) => {
                        Some(PersonaToolDeadlineClock {
                            monotonic_created_ms,
                            boot_wall_anchor_ms,
                        })
                    }
                    (None, None) => None,
                    _ => anyhow::bail!("Persona approval deadline clock is incomplete"),
                };
            deadlines.observe(
                &approval.approval_id,
                approval.created_at_ms,
                approval.expires_at_ms,
                persisted_clock,
                now,
                monotonic_now,
            )?;
        }
    }
    let active = index
        .approvals
        .iter()
        .filter(|approval| {
            only_invocation_id.is_none_or(|invocation_id| approval.invocation_id == invocation_id)
        })
        .collect::<Vec<_>>();
    let active_invocation_count = active
        .iter()
        .map(|approval| approval.invocation_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    if active_invocation_count > MAX_ACTIVE_APPROVAL_INVOCATIONS
        || active.len() > MAX_ACTIVE_APPROVAL_INVOCATIONS.saturating_mul(MAX_TARGETS)
    {
        anyhow::bail!(
            "active Persona approvals exceed the bounded recovery limit of {} invocations",
            MAX_ACTIVE_APPROVAL_INVOCATIONS
        );
    }
    if active.is_empty() {
        return Ok(());
    }
    let mut forced_expirations = BTreeMap::<String, BTreeSet<String>>::new();
    let mut stale_resumes = BTreeMap::<String, BTreeSet<String>>::new();
    let mut transition_due = false;
    for approval in &active {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let monotonic_expired = deadlines.due(&approval.approval_id, monotonic_now)?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let monotonic_expired = true;
        if approval.state == MentionToolApprovalState::Pending
            && (now >= approval.expires_at_ms || monotonic_expired)
        {
            forced_expirations
                .entry(approval.invocation_id.clone())
                .or_default()
                .insert(approval.approval_id.clone());
            transition_due = true;
        }
        if approval.state == MentionToolApprovalState::Resuming {
            let lease_identity = approval
                .resume_lease_id
                .as_deref()
                .filter(|lease_id| !lease_id.is_empty());
            let lease_is_live = match lease_identity {
                Some(lease_id) => resume_lease_is_live_for_recovery(
                    data_dir,
                    &approval.invocation_id,
                    &approval.approval_id,
                    lease_id,
                )?,
                None => false,
            };
            if !lease_is_live {
                stale_resumes
                    .entry(approval.invocation_id.clone())
                    .or_default()
                    .insert(approval.approval_id.clone());
                transition_due = true;
            }
        }
    }
    if !transition_due {
        return Ok(());
    }
    let active_ids = active
        .iter()
        .map(|approval| approval.invocation_id.clone())
        .collect::<BTreeSet<_>>();
    let migrated_conversations = store
        .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
        .unwrap_or_default();
    store.mutate_documents(
        INVOCATIONS_NAMESPACE,
        MentionInvocationDb::default,
        |db, documents| {
            let mut conversations = documents
                .get(CONVERSATIONS_NAMESPACE)?
                .unwrap_or(migrated_conversations);
            let mut changed = false;
            for stored in db
                .invocations
                .iter_mut()
                .filter(|stored| active_ids.contains(&stored.id))
            {
                let outcome = reconcile_stored_persona_tool_approvals(
                    stored,
                    now,
                    forced_expirations
                        .get(&stored.id)
                        .cloned()
                        .unwrap_or_default(),
                    stale_resumes.get(&stored.id).cloned().unwrap_or_default(),
                )?;
                if !outcome.changed {
                    continue;
                }
                for receipt in outcome.effect_receipts {
                    documents.put_receipt(
                        &receipt.receipt.task_id,
                        &receipt.receipt.command,
                        &receipt.receipt,
                    )?;
                }
                finish_stored_mention_invocation(stored, &mut conversations);
                changed = true;
            }
            if changed {
                write_active_approval_index(db, documents)?;
                crate::personas::reject_removed_conversation_writes_from_documents(
                    &conversations,
                    documents,
                )?;
                documents.put_bytes(
                    CONVERSATIONS_NAMESPACE,
                    &serde_json::to_vec(&conversations)?,
                )?;
            }
            Ok(())
        },
    )
}

struct ApprovalReconciliation {
    changed: bool,
    effect_receipts: Vec<CommandResult<McpCallToolOutput>>,
}

fn reconcile_stored_persona_tool_approvals(
    stored: &mut StoredMentionInvocation,
    now: u128,
    forced_expirations: BTreeSet<String>,
    stale_resumes: BTreeSet<String>,
) -> Result<ApprovalReconciliation> {
    let transitions = stored
        .tool_approvals
        .iter()
        .filter_map(|approval| match approval.state {
            MentionToolApprovalState::Pending
                if now >= approval.expires_at_ms
                    || forced_expirations.contains(&approval.id) =>
            {
                Some((
                    approval.id.clone(),
                    approval.target_id.clone(),
                    MentionToolApprovalState::Expired,
                    "The exact Persona tool approval expired before it was used.".to_string(),
                ))
            }
            MentionToolApprovalState::Resuming if stale_resumes.contains(&approval.id) => Some((
                approval.id.clone(),
                approval.target_id.clone(),
                MentionToolApprovalState::Failed,
                "The app stopped while the exact Persona tool call was resuming. Its external outcome is unknown, so Mom will not execute it again.".to_string(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    if transitions.is_empty() {
        return Ok(ApprovalReconciliation {
            changed: false,
            effect_receipts: Vec::new(),
        });
    }
    let mut effect_receipts = Vec::new();
    for (approval_id, target_id, state, message) in &transitions {
        let approval_index = stored
            .tool_approvals
            .iter()
            .position(|approval| approval.id == *approval_id)
            .expect("reconciled approval");
        let approval = &mut stored.tool_approvals[approval_index];
        approval.state = *state;
        let mut current_receipt_id = None;
        if *state == MentionToolApprovalState::Expired {
            approval.consumed_at_ms = Some(now);
            approval.effect_receipt_id = None;
            approval.effect_outcome = None;
        } else if approval.decision == Some(MentionToolApprovalDecision::Approve) {
            if approval.effect_receipt_id.is_none() {
                approval.effect_receipt_id = Some(persona_tool_effect_receipt_id());
            }
            let receipt_id = approval
                .effect_receipt_id
                .clone()
                .expect("approved stale resume receipt identity");
            let receipt = interrupted_persona_tool_effect_receipt(approval, message);
            if receipt.receipt.task_id != receipt_id {
                anyhow::bail!("stale Persona effect receipt identity changed");
            }
            current_receipt_id = Some(receipt_id);
            approval.effect_outcome = Some(MentionToolEffectOutcome::Unknown);
            effect_receipts.push(receipt);
        } else {
            approval.effect_receipt_id = None;
            approval.effect_outcome = None;
        }
        if !stored
            .results
            .iter()
            .any(|result| result.target_id == *target_id)
        {
            let snapshot = stored
                .targets
                .iter()
                .find(|snapshot| snapshot.target_id == *target_id)
                .cloned()
                .ok_or_else(|| anyhow!("reconciled Persona snapshot disappeared"))?;
            let mut result = blocked_target_result(&snapshot, GenerationState::Failed, message);
            if let Some(receipt_id) = current_receipt_id {
                result.tool_receipt_ids = vec![receipt_id];
            }
            stored.results.push(result);
        }
    }
    let terminal_ids = transitions
        .iter()
        .map(|(approval_id, _, _, _)| approval_id.as_str())
        .collect::<BTreeSet<_>>();
    stored
        .frozen_tool_continuations
        .retain(|continuation| !terminal_ids.contains(continuation.approval_id.as_str()));
    Ok(ApprovalReconciliation {
        changed: true,
        effect_receipts,
    })
}

fn finish_stored_mention_invocation(
    stored: &mut StoredMentionInvocation,
    conversations: &mut ConversationDb,
) {
    let target_order = stored
        .targets
        .iter()
        .enumerate()
        .map(|(index, target)| (target.target_id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    stored.results.sort_by_key(|result| {
        target_order
            .get(&result.target_id)
            .copied()
            .unwrap_or(usize::MAX)
    });
    stored.state = if stored.tool_approvals.iter().any(|approval| {
        matches!(
            approval.state,
            MentionToolApprovalState::Pending | MentionToolApprovalState::Resuming
        )
    }) {
        MentionInvocationState::AwaitingApproval
    } else {
        invocation_state(&stored.results)
    };
    if stored.state != MentionInvocationState::AwaitingApproval
        && let Some(host) = conversations
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == stored.host_conversation_id)
            .filter(|host| {
                host.messages
                    .iter()
                    .any(|message| message.id == stored.user_message_id)
            })
    {
        append_attributed_results(host, stored);
    }
    stored.updated_at = now_ms().to_string();
}

pub fn mention_synthesize(invocation_id: &str) -> Result<CommandResult<MentionSynthesisOutput>> {
    let store = RuntimeStore::current()?;
    let invocation = store
        .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
        .unwrap_or_default()
        .invocations
        .into_iter()
        .find(|invocation| invocation.id == invocation_id);
    let Some(invocation) = invocation else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_invocation_not_found",
                "The invited responses are no longer available.",
                vec!["Run the Persona group again.".to_string()],
            ),
        ));
    };
    if invocation.synthesis_message_id.is_some() {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_synthesis_already_exists",
                "This set of invited responses already has a synthesis.",
                vec!["Edit or regenerate the existing synthesis message.".to_string()],
            ),
        ));
    }
    if invocation.state == MentionInvocationState::AwaitingApproval
        || invocation.tool_approvals.iter().any(|approval| {
            matches!(
                approval.state,
                MentionToolApprovalState::Pending | MentionToolApprovalState::Resuming
            )
        })
    {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_synthesis_approval_pending",
                "Every exact Persona tool approval must reach a terminal state before synthesis.",
                vec!["Approve, deny, cancel, or wait for pending approvals to expire.".to_string()],
            ),
        ));
    }
    let completed = invocation
        .results
        .iter()
        .filter(|result| {
            result.state == GenerationState::Completed
                && result.real_engine_invoked
                && !result.fake_fixture
                && !result.text.trim().is_empty()
        })
        .collect::<Vec<_>>();
    if completed.len() < 2 {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_synthesis_sources_incomplete",
                "At least two completed local-model responses are required for synthesis.",
                vec!["Wait for another invited response to finish.".to_string()],
            ),
        ));
    }

    let settings = resolve_settings()?;
    let db = load_db()?;
    let Some(host) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == invocation.host_conversation_id)
        .cloned()
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_host_conversation_not_found",
                "The host chat no longer exists.",
                vec!["Open another chat and run the group again.".to_string()],
            ),
        ));
    };
    let model_path = host
        .execution_profile
        .model_path
        .as_deref()
        .or(host.selected_model_path.as_deref())
        .or(settings.model_path.as_deref());
    let Some(model_path) = model_path else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "blocked_missing_model",
            Blocker::new(
                "model_path_missing",
                "No local model is configured for this chat.",
                vec!["Choose a model in Settings.".to_string()],
            ),
        ));
    };
    let handle = match resident_model_for_profile(
        &settings,
        model_path,
        host.execution_profile.mmproj_path.as_deref(),
    ) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mention_synthesize",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let sources = completed
        .iter()
        .map(|result| format!("## {} (@{})\n{}", result.label, result.handle, result.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let messages = vec![
        ChatMessage {
            role: ChatRole::System,
            content: "Synthesize the invited local-model responses into one useful answer. Preserve material disagreements, distinguish evidence from uncertainty, and do not invent consensus or facts. Do not claim to be any invited Persona.".to_string(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: format!(
                "Original addressed message:\n{}\n\nInvited responses:\n{}",
                invocation.addressed_message, sources
            ),
        },
    ];
    let template = profile_chat_template(&host.execution_profile);
    let prompt_tokens = handle
        .tokenize_messages_with_template(messages.clone(), template.clone())
        .map_err(|error| anyhow!(error))?
        .token_ids
        .len();
    let sampling = host
        .execution_profile
        .sampling
        .clone()
        .unwrap_or_else(|| settings.sampling_config());
    if prompt_tokens.saturating_add(sampling.max_tokens as usize) > settings.context_tokens as usize
    {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_synthesis_context_too_large",
                "The completed invited responses do not fit the synthesis model context.",
                vec!["Reduce Persona output limits and run the group again.".to_string()],
            ),
        ));
    }
    let request_id = format!("{invocation_id}:synthesis");
    let output = handle
        .generate(GenerationRequest {
            request_id: request_id.clone(),
            model_id: handle.status().model_id,
            input: GenerationInput::Chat { messages, template },
            sampling,
            media: Vec::new(),
            cached_prefix: None,
        })
        .map_err(|error| anyhow!(error))?
        .wait()
        .map_err(|error| anyhow!(error))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("native mention synthesis returned no output"))?;
    if output.state != GenerationState::Completed
        || !output.real_engine_invoked
        || output.fake_fixture
        || output.text.trim().is_empty()
    {
        return Ok(CommandResult::blocked_with_evidence(
            "mom_llama.mention_synthesize",
            "blocked_native_runtime",
            Blocker::new(
                "mention_synthesis_not_completed",
                "The local synthesis model did not complete a real response.",
                vec!["Retry synthesis or check the chat's selected model.".to_string()],
            ),
            vec![store.path().display().to_string()],
            Vec::new(),
            output.real_engine_invoked,
            output.fake_fixture,
        ));
    }

    let mut db = load_db()?;
    let Some(host) = db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == invocation.host_conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_host_conversation_not_found",
                "The host chat was removed before synthesis completed.",
                vec!["Open another chat and run the group again.".to_string()],
            ),
        ));
    };
    let message_id = Uuid::new_v4().to_string();
    host.messages.push(Message {
        id: message_id.clone(),
        conversation_id: host.id.clone(),
        role: MessageRole::Assistant,
        content: output.text.clone(),
        created_at: now_ms().to_string(),
        parent_id: active_leaf_id(host),
        model: Some(output.model_id.clone()),
        receipt_id: Some(format!("mom_llama.mention_synthesize:{invocation_id}")),
        prompt_tokens: Some(output.metrics.prompt_tokens),
        completion_tokens: Some(output.metrics.completion_tokens),
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: Some(MessageAttribution {
            kind: MessageSpeakerKind::Synthesis,
            source_id: invocation_id.to_string(),
            handle: "synthesis".to_string(),
            label: "Synthesis".to_string(),
            version: 1,
            invocation_id: invocation_id.to_string(),
            target_order: completed.len(),
        }),
        attachment_ids: Vec::new(),
    });
    host.active_leaf_message_id = Some(message_id.clone());
    host.updated_at = now_ms().to_string();
    let updated_host = host.clone();
    let conversation_path = upsert_conversation(db, updated_host)?;
    let synthesis_sha256 = format!("{:x}", Sha256::digest(output.text.as_bytes()));
    store.mutate(INVOCATIONS_NAMESPACE, MentionInvocationDb::default, |db| {
        if let Some(stored) = db
            .invocations
            .iter_mut()
            .find(|stored| stored.id == invocation_id)
        {
            stored.synthesis_message_id = Some(message_id.clone());
            stored.synthesis_sha256 = Some(synthesis_sha256.clone());
            stored.updated_at = now_ms().to_string();
        }
        Ok(())
    })?;
    let result = MentionSynthesisOutput {
        invocation_id: invocation_id.to_string(),
        host_conversation_id: invocation.host_conversation_id.clone(),
        message_id,
        text: output.text,
        model_id: output.model_id,
        source_message_ids: completed
            .iter()
            .filter_map(|result| result.message_id.clone())
            .collect(),
        source_content_sha256: completed
            .iter()
            .map(|result| format!("{:x}", Sha256::digest(result.text.as_bytes())))
            .collect(),
        metrics: output.metrics,
    };
    Ok(CommandResult::passed(
        "mom_llama.mention_synthesize",
        "real_prompt_smoke_passed",
        result,
        vec![
            conversation_path.display().to_string(),
            store.path().display().to_string(),
        ],
        Vec::new(),
        true,
        false,
    ))
}

fn dispatch_mentions<F>(
    scope: &OperationScope,
    input: MentionDispatchInput,
    targets: Vec<ResolvedTarget>,
    options: ChatSendOptions,
    on_event: &mut Option<F>,
) -> Result<CommandResult<ChatDispatchOutput>>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    let settings = resolve_settings()?;
    let mut db = load_db()?;
    let Some(host_index) = db
        .conversations
        .iter()
        .position(|conversation| conversation.id == input.conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "host_conversation_not_found",
                "The host conversation no longer exists.",
                vec!["Open another chat and retry.".to_string()],
            ),
        ));
    };
    let host_snapshot = active_path_messages(&db.conversations[host_index]);
    let attachment_context =
        match prepare_chat_attachments(&input.conversation_id, &host_snapshot, None)? {
            Ok(context) => context,
            Err(blocked) => {
                return Ok(CommandResult::blocked(
                    "mom_llama.chat_dispatch",
                    &blocked.readiness,
                    blocked.blocker,
                ));
            }
        };
    if attachment_context
        .draft_snapshot
        .as_ref()
        .is_some_and(|draft| draft.message != input.message)
    {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "draft_changed_before_send",
                "The saved draft changed before this exact Persona message could be admitted.",
                vec!["Refresh the composer and send the current draft.".to_string()],
            ),
        ));
    }
    if !attachment_context.media.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "mention_multimodal_attachment_unsupported",
                "Image and audio attachments are not yet accepted by the shared-prefix mention dispatcher.",
                vec!["Send the attachment to one chat directly, or remove it before invoking Personas.".to_string()],
            ),
        ));
    }
    let invocation_id = Uuid::new_v4().to_string();
    let user_message_id = Uuid::new_v4().to_string();
    let user_message = Message {
        id: user_message_id.clone(),
        conversation_id: input.conversation_id.clone(),
        role: MessageRole::User,
        content: input.message.clone(),
        created_at: now_ms().to_string(),
        parent_id: active_leaf_id(&db.conversations[host_index]),
        model: None,
        receipt_id: None,
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: None,
        attachment_ids: attachment_context.staged_ids.clone(),
    };
    let expected_active_leaf = user_message.parent_id.clone();

    let snapshots = targets
        .iter()
        .map(snapshot_target)
        .collect::<Result<Vec<_>>>()?;
    let _cancel_lifecycle =
        match MentionCancelLifecycle::register_snapshots(scope, &invocation_id, &snapshots)? {
            Ok(lifecycle) => lifecycle,
            Err(blocker) => {
                return Ok(CommandResult::blocked(
                    "mom_llama.chat_dispatch",
                    "stub_blocked",
                    blocker,
                ));
            }
        };
    let now = now_ms().to_string();
    let mut invocation = MentionInvocation {
        id: invocation_id.clone(),
        host_conversation_id: input.conversation_id.clone(),
        user_message_id: user_message_id.clone(),
        addressed_message: input.message.clone(),
        host_context: host_snapshot.clone(),
        targets: snapshots.clone(),
        results: Vec::new(),
        tool_approvals: Vec::new(),
        synthesis_message_id: None,
        synthesis_sha256: None,
        state: MentionInvocationState::Running,
        created_at: now.clone(),
        updated_at: now,
    };

    if options.fake_fixture {
        for snapshot in &snapshots {
            emit(
                on_event,
                MentionStreamEvent {
                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                    invocation_id: invocation_id.clone(),
                    target_id: snapshot.target_id.clone(),
                    handle: snapshot.handle.clone(),
                    label: snapshot.label.clone(),
                    event: "started".to_string(),
                    delta: None,
                    state: Some(GenerationState::Queued),
                    real_engine_invoked: false,
                    fake_fixture: true,
                },
            )?;
            let text = format!("Fixture response from @{}", snapshot.handle);
            emit(
                on_event,
                MentionStreamEvent {
                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                    invocation_id: invocation_id.clone(),
                    target_id: snapshot.target_id.clone(),
                    handle: snapshot.handle.clone(),
                    label: snapshot.label.clone(),
                    event: "delta".to_string(),
                    delta: Some(text.clone()),
                    state: None,
                    real_engine_invoked: false,
                    fake_fixture: true,
                },
            )?;
            invocation.results.push(MentionTargetResult {
                target_id: snapshot.target_id.clone(),
                handle: snapshot.handle.clone(),
                label: snapshot.label.clone(),
                state: GenerationState::Completed,
                text,
                model_id: "fake_fixture".to_string(),
                message_id: None,
                metrics: GenerationMetrics::default(),
                cache_id: None,
                cache_reused: false,
                tool_receipt_ids: Vec::new(),
                real_engine_invoked: false,
                fake_fixture: true,
            });
        }
        let host = &mut db.conversations[host_index];
        host.messages.push(user_message);
        host.active_leaf_message_id = Some(user_message_id.clone());
        host.updated_at = now_ms().to_string();
        append_attributed_results(host, &mut invocation);
        invocation.state = invocation_state(&invocation.results);
        invocation.updated_at = now_ms().to_string();
        let generated_message_ids = std::iter::once(user_message_id.clone())
            .chain(
                invocation
                    .results
                    .iter()
                    .filter_map(|result| result.message_id.clone()),
            )
            .collect::<Vec<_>>();
        let host = host.clone();
        let (conversation_path, ()) = commit_generated_exchange_with_journal(
            db,
            host,
            expected_active_leaf.as_deref(),
            &generated_message_ids,
            &attachment_context.staged_ids,
            &user_message_id,
            attachment_context.draft_snapshot.as_ref(),
            INVOCATIONS_NAMESPACE,
            MentionInvocationDb::default,
            |invocations| {
                upsert_invocation_with_continuations(invocations, &invocation, Vec::new())
            },
            write_active_approval_index,
        )?;
        return Ok(CommandResult::passed(
            "mom_llama.chat_dispatch",
            "fake_fixture_exercised",
            ChatDispatchOutput::Mention {
                conversation_id: input.conversation_id,
                invocation,
            },
            vec![conversation_path.display().to_string()],
            Vec::new(),
            false,
            true,
        ));
    }

    let addressed = append_attachment_context(
        &strip_handles(&input.message, &targets),
        &attachment_context.current_text,
    );
    let participant_names = targets
        .iter()
        .map(|target| format!("@{}", target.conversation.execution_profile.mention_handle))
        .collect::<Vec<_>>()
        .join(", ");
    let mut planned = Vec::new();
    for (order, target) in targets.iter().enumerate() {
        let snapshot = &snapshots[order];
        let model_path = snapshot
            .profile
            .model_path
            .as_deref()
            .or(target.conversation.selected_model_path.as_deref())
            .or(settings.model_path.as_deref());
        let Some(model_path) = model_path else {
            invocation.results.push(blocked_target_result(
                snapshot,
                GenerationState::Failed,
                "No model is configured for this invited chat.",
            ));
            continue;
        };
        let model_config = match model_configuration_for_profile(
            &settings,
            model_path,
            snapshot.profile.mmproj_path.as_deref(),
        ) {
            Ok(config) => config,
            Err(blocked) => {
                invocation.results.push(blocked_target_result(
                    snapshot,
                    GenerationState::Failed,
                    &blocked.blocker.message,
                ));
                continue;
            }
        };
        let handle = match resident_model_for_configuration(&settings, &model_config) {
            Ok(handle) => handle,
            Err(blocked) => {
                invocation.results.push(blocked_target_result(
                    snapshot,
                    GenerationState::Failed,
                    &blocked.blocker.message,
                ));
                continue;
            }
        };
        let tools = match resolve_mention_tools(&snapshot.profile.tool_bindings) {
            Ok(tools) => tools,
            Err(blocker) => {
                invocation.results.push(blocked_target_result(
                    snapshot,
                    GenerationState::Failed,
                    &blocker.message,
                ));
                continue;
            }
        };
        let handoff = match handoff_messages(
            &handle,
            &settings,
            snapshot,
            &host_snapshot,
            &addressed,
            &participant_names,
            &tools,
        ) {
            Ok(handoff) => handoff,
            Err(blocker) => {
                invocation.results.push(blocked_target_result(
                    snapshot,
                    GenerationState::Failed,
                    &blocker.message,
                ));
                continue;
            }
        };
        let cache_use = match snapshot.profile.chat_template {
            crate::conversation_store::ChatTemplatePolicy::ModelDefault => ensure_persona_prefix(
                &handle,
                &cache_owner(snapshot),
                &format!("Invited context for @{}", snapshot.handle),
                &handoff.stable_prefix,
                &handoff.messages,
            )?,
            crate::conversation_store::ChatTemplatePolicy::FrozenSource(_) => None,
        };
        planned.push(PlannedTarget {
            snapshot: snapshot.clone(),
            model_path: model_path.to_path_buf(),
            model_config,
            handle,
            messages: handoff.messages,
            cache_id: cache_use.as_ref().map(|cache| cache.cache_id.clone()),
            cache_reused: cache_use.as_ref().is_some_and(|cache| cache.reused),
            cached_prefix: cache_use.map(|cache| cache.sequence),
            tools,
        });
    }

    let mut groups = BTreeMap::<String, Vec<PlannedTarget>>::new();
    for target in planned {
        let template_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&target.snapshot.profile.chat_template)?)
        );
        let key = format!(
            "{}|{}|{}",
            target.model_path.display(),
            target
                .snapshot
                .profile
                .mmproj_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            template_hash
        );
        groups.entry(key).or_default().push(target);
    }
    let mut tickets = Vec::new();
    let mut pending_continuations = Vec::new();
    for targets in groups.into_values() {
        for target in &targets {
            emit(
                on_event,
                MentionStreamEvent {
                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                    invocation_id: invocation_id.clone(),
                    target_id: target.snapshot.target_id.clone(),
                    handle: target.snapshot.handle.clone(),
                    label: target.snapshot.label.clone(),
                    event: "started".to_string(),
                    delta: None,
                    state: Some(GenerationState::Queued),
                    real_engine_invoked: false,
                    fake_fixture: false,
                },
            )?;
        }
        let handle = targets[0].handle.clone();
        let status = handle.status();
        let branches = targets
            .iter()
            .map(|target| BranchRequest {
                branch_id: target.snapshot.target_id.clone(),
                label: target.snapshot.label.clone(),
                instruction: String::new(),
                sampling: target
                    .snapshot
                    .profile
                    .sampling
                    .clone()
                    .unwrap_or_else(|| settings.sampling_config()),
                messages: target.messages.clone(),
                cached_prefix: target.cached_prefix.clone(),
            })
            .collect();
        let ticket = handle
            .generate_shared_prefix(SharedPrefixBatchRequest {
                request_id: invocation_id.clone(),
                model_id: status.model_id,
                common_messages: Vec::new(),
                chat_template: match &targets[0].snapshot.profile.chat_template {
                    crate::conversation_store::ChatTemplatePolicy::ModelDefault => {
                        ChatTemplateChoice::ModelDefault
                    }
                    crate::conversation_store::ChatTemplatePolicy::FrozenSource(template) => {
                        ChatTemplateChoice::Override(template.clone())
                    }
                },
                branches,
                cached_prefix: None,
            })
            .map_err(|error| anyhow!(error))?;
        tickets.push((ticket, targets));
    }
    let started = Instant::now();
    let timeout = Duration::from_secs_f64(options.timeout_s.max(0.001));
    let mut disconnected = vec![false; tickets.len()];
    while disconnected.iter().any(|done| !done) {
        if started.elapsed() >= timeout {
            for (ticket, _) in &tickets {
                ticket.cancel_all();
            }
        }
        let mut progress = false;
        for (index, (ticket, targets)) in tickets.iter().enumerate() {
            if disconnected[index] {
                continue;
            }
            loop {
                match ticket.events.try_recv() {
                    Ok(event) => {
                        progress = true;
                        let target = targets
                            .iter()
                            .find(|target| target.snapshot.target_id == event.branch_id);
                        if let Some(target) = target {
                            let (name, delta, state) = match event.event {
                                GenerationEventKind::Delta { text } => ("delta", Some(text), None),
                                GenerationEventKind::State { state } => {
                                    ("state", None, Some(state))
                                }
                                GenerationEventKind::Warning { message, .. } => {
                                    ("warning", Some(message), None)
                                }
                            };
                            emit(
                                on_event,
                                MentionStreamEvent {
                                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                                    invocation_id: invocation_id.clone(),
                                    target_id: target.snapshot.target_id.clone(),
                                    handle: target.snapshot.handle.clone(),
                                    label: target.snapshot.label.clone(),
                                    event: name.to_string(),
                                    delta,
                                    state,
                                    real_engine_invoked: name == "delta"
                                        || state == Some(GenerationState::Completed),
                                    fake_fixture: false,
                                },
                            )?;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected[index] = true;
                        break;
                    }
                }
            }
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    for (ticket, targets) in tickets {
        match ticket.wait() {
            Ok(outputs) => {
                for output in outputs {
                    if let Some(target) = targets
                        .iter()
                        .find(|target| target.snapshot.target_id == output.branch_id)
                    {
                        let (output, tool_receipt_ids) = if output.state
                            == GenerationState::Completed
                            && !target.tools.is_empty()
                        {
                            match finish_tool_bound_mention(
                                scope,
                                target,
                                output,
                                &invocation_id,
                                &invocation.host_conversation_id,
                                &invocation.user_message_id,
                                &settings,
                            ) {
                                Ok(ToolBoundMentionFinish::Final {
                                    output,
                                    tool_receipt_ids,
                                }) => (output, tool_receipt_ids),
                                Ok(ToolBoundMentionFinish::Pending {
                                    approval,
                                    continuation,
                                }) => {
                                    invocation.tool_approvals.push(approval);
                                    pending_continuations.push(*continuation);
                                    continue;
                                }
                                Err(blocker) => {
                                    let state = if mention_cancellation_requested(
                                        scope,
                                        &invocation_id,
                                        &target.snapshot.target_id,
                                    ) {
                                        GenerationState::Cancelled
                                    } else {
                                        GenerationState::Failed
                                    };
                                    let mut blocked = blocked_target_result(
                                        &target.snapshot,
                                        state,
                                        &blocker.message,
                                    );
                                    blocked.real_engine_invoked = true;
                                    invocation.results.push(blocked);
                                    continue;
                                }
                            }
                        } else {
                            (output, Vec::new())
                        };
                        invocation.results.push(MentionTargetResult {
                            target_id: target.snapshot.target_id.clone(),
                            handle: target.snapshot.handle.clone(),
                            label: target.snapshot.label.clone(),
                            state: output.state,
                            text: strip_reserved_attribution_prefix(&output.text),
                            model_id: output.model_id,
                            message_id: None,
                            metrics: output.metrics,
                            cache_id: target.cache_id.clone(),
                            cache_reused: target.cache_reused,
                            tool_receipt_ids,
                            real_engine_invoked: output.real_engine_invoked,
                            fake_fixture: output.fake_fixture,
                        });
                    }
                }
            }
            Err(error) => {
                for target in targets {
                    invocation.results.push(blocked_target_result(
                        &target.snapshot,
                        GenerationState::Failed,
                        &error.message,
                    ));
                }
            }
        }
    }
    invocation.results.sort_by_key(|result| {
        snapshots
            .iter()
            .position(|target| target.target_id == result.target_id)
            .unwrap_or(usize::MAX)
    });
    invocation.state = if invocation
        .tool_approvals
        .iter()
        .any(|approval| approval.state == MentionToolApprovalState::Pending)
    {
        MentionInvocationState::AwaitingApproval
    } else {
        invocation_state(&invocation.results)
    };
    invocation.updated_at = now_ms().to_string();
    let real_engine_invoked = invocation.results.iter().any(|result| {
        result.real_engine_invoked
            && !result.fake_fixture
            && result.state == GenerationState::Completed
            && !result.text.trim().is_empty()
    }) || pending_continuations.iter().any(|continuation| {
        continuation.provisional_output.real_engine_invoked
            && !continuation.provisional_output.fake_fixture
            && continuation.provisional_output.state == GenerationState::Completed
    });
    if !real_engine_invoked {
        save_invocation(&invocation)?;
        return Ok(CommandResult::blocked_with_evidence(
            "mom_llama.chat_dispatch",
            "blocked_native_runtime",
            Blocker::new(
                "mention_targets_failed",
                "None of the invited local models completed a response.",
                vec!["Check the target Personas' model profiles and context budgets.".to_string()],
            ),
            vec![RuntimeStore::current()?.path().display().to_string()],
            Vec::new(),
            false,
            false,
        ));
    }
    let mut commit_db = load_db()?;
    let Some(host) = commit_db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == input.conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "host_conversation_not_found",
                "The host conversation was removed while invited models were responding.",
                vec!["Open another chat and retry.".to_string()],
            ),
        ));
    };
    host.messages.push(user_message);
    host.active_leaf_message_id = Some(user_message_id.clone());
    host.updated_at = now_ms().to_string();
    if pending_continuations.is_empty() {
        append_attributed_results(host, &mut invocation);
    }
    let generated_message_ids = std::iter::once(user_message_id.clone())
        .chain(
            invocation
                .results
                .iter()
                .filter_map(|result| result.message_id.clone()),
        )
        .collect::<Vec<_>>();
    let host = host.clone();
    let (conversation_path, ()) = commit_generated_exchange_with_journal(
        commit_db,
        host,
        expected_active_leaf.as_deref(),
        &generated_message_ids,
        &attachment_context.staged_ids,
        &user_message_id,
        attachment_context.draft_snapshot.as_ref(),
        INVOCATIONS_NAMESPACE,
        MentionInvocationDb::default,
        |invocations| {
            upsert_invocation_with_continuations(invocations, &invocation, pending_continuations)
        },
        write_active_approval_index,
    )?;
    Ok(CommandResult::passed(
        "mom_llama.chat_dispatch",
        "real_prompt_smoke_passed",
        ChatDispatchOutput::Mention {
            conversation_id: input.conversation_id,
            invocation,
        },
        vec![conversation_path.display().to_string()],
        Vec::new(),
        real_engine_invoked,
        false,
    ))
}

#[derive(Clone)]
struct ResolvedTarget {
    kind: MentionTargetKind,
    conversation: Conversation,
}

struct TargetResolution {
    targets: Vec<ResolvedTarget>,
    unresolved: Vec<String>,
    ambiguous: Vec<String>,
}

fn ambiguous_resolution_blocker(resolution: &TargetResolution) -> Option<Blocker> {
    (!resolution.ambiguous.is_empty()).then(|| {
        Blocker::new(
            "mention_target_ambiguous",
            format!(
                "These mentions match more than one saved participant: {}.",
                resolution
                    .ambiguous
                    .iter()
                    .map(|handle| format!("@{handle}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            vec![
                "Rename one of the conflicting handles in Personas or Consult groups.".to_string(),
            ],
        )
    })
}

struct PlannedTarget {
    snapshot: MentionTargetSnapshot,
    model_path: PathBuf,
    model_config: NativeModelConfig,
    handle: NativeModelHandle,
    messages: Vec<ChatMessage>,
    cache_id: Option<String>,
    cache_reused: bool,
    cached_prefix: Option<llama_native_types::SequenceStateBlob>,
    tools: Vec<BoundMentionTool>,
}

enum ToolBoundMentionFinish {
    Final {
        output: GenerationOutput,
        tool_receipt_ids: Vec<String>,
    },
    Pending {
        approval: MentionToolApproval,
        continuation: Box<FrozenMentionToolContinuation>,
    },
}

struct MentionCancelLifecycle {
    scope: OperationScope,
    registrations: Vec<(MentionCancelKey, Arc<MentionCancelControl>)>,
}

impl MentionCancelLifecycle {
    fn register_snapshots(
        scope: &OperationScope,
        invocation_id: &str,
        targets: &[MentionTargetSnapshot],
    ) -> Result<std::result::Result<Self, Blocker>> {
        scope.with_mention_registry(|registry, quiescing| {
            let persona_ids = targets
                .iter()
                .filter(|target| target.kind == MentionTargetKind::Persona)
                .map(|target| target.target_id.clone())
                .collect::<Vec<_>>();
            let removed = crate::personas::persona_ids_are_removed(&persona_ids)?;
            if !removed.is_empty() {
                return Ok(Err(Blocker::new(
                    "persona_removed_before_invocation_admission",
                    "A selected Persona was removed from the library before this invocation could be admitted.",
                    vec!["Refresh mentions and choose a discoverable Persona.".to_string()],
                )));
            }
            let mut registrations = Vec::with_capacity(targets.len());
            for target in targets {
                let key = (invocation_id.to_string(), target.target_id.clone());
                let flag = Arc::new(MentionCancelControl::running(quiescing));
                registry.insert(key.clone(), Arc::clone(&flag));
                registrations.push((key, flag));
            }
            Ok(Ok(Self {
                scope: scope.clone(),
                registrations,
            }))
        })
    }

    fn register_target(
        scope: &OperationScope,
        invocation_id: &str,
        target_id: &str,
    ) -> Result<std::result::Result<Self, Blocker>> {
        scope.with_mention_registry(|registry, quiescing| {
            let key = (invocation_id.to_string(), target_id.to_string());
            let flag = Arc::new(MentionCancelControl::running(quiescing));
            let mut registrations = Vec::new();
            if registry.contains_key(&key) {
                return Ok(Err(Blocker::new(
                    "mention_tool_approval_target_active",
                    "This Persona target already has an active operation.",
                    vec!["Wait for the active operation to finish.".to_string()],
                )));
            }
            registry.insert(key.clone(), Arc::clone(&flag));
            registrations.push((key, flag));
            Ok(Ok(Self {
                scope: scope.clone(),
                registrations,
            }))
        })
    }

    fn arbitrate_terminal(&self, invocation_id: &str, target_id: &str) -> bool {
        self.registrations
            .iter()
            .find(|(key, _)| key.0 == invocation_id && key.1 == target_id)
            .is_some_and(|(_, control)| control.arbitrate_terminal())
    }

    fn requested(&self) -> bool {
        self.registrations
            .iter()
            .any(|(_, control)| control.cancellation_requested())
    }
}

impl Drop for MentionCancelLifecycle {
    fn drop(&mut self) {
        let _ = self.scope.with_mention_registry(|registry, _| {
            unregister_exact_mentions(registry, &self.registrations);
            Ok(())
        });
    }
}

fn unregister_exact_mentions(
    registry: &mut MentionCancelRegistry,
    registrations: &[(MentionCancelKey, Arc<MentionCancelControl>)],
) {
    for (key, flag) in registrations {
        if registry
            .get(key)
            .is_some_and(|registered| Arc::ptr_eq(registered, flag))
        {
            registry.remove(key);
        }
    }
}

fn request_mention_cancellation(
    scope: &OperationScope,
    invocation_id: &str,
    target_id: Option<&str>,
) -> usize {
    scope
        .with_mention_registry(|registry, _| {
            Ok(registry
                .iter()
                .filter(|((candidate_invocation, candidate_target), _)| {
                    candidate_invocation == invocation_id
                        && target_id.is_none_or(|target| candidate_target == target)
                })
                .map(|(_, control)| usize::from(control.request_cancel()))
                .sum())
        })
        .unwrap_or_default()
}

fn mention_cancellation_requested(
    scope: &OperationScope,
    invocation_id: &str,
    target_id: &str,
) -> bool {
    scope
        .with_mention_registry(|registry, _| {
            Ok(registry
                .get(&(invocation_id.to_string(), target_id.to_string()))
                .cloned())
        })
        .ok()
        .flatten()
        .is_some_and(|control| control.cancellation_requested())
}

#[derive(Debug, Clone)]
struct BoundMentionTool {
    contract: McpTool,
    server: String,
    server_config: McpServerConfig,
    policy: ToolPermissionPolicy,
    server_config_sha256: String,
    frozen_server_config_sha256: String,
    tool_schema_sha256: String,
}

fn resolve_targets(handles: &[String], host_id: &str) -> Result<TargetResolution> {
    let (conversations, groups) = conversation_and_group_handles()?;
    Ok(resolve_targets_from_registry(
        handles,
        host_id,
        &conversations,
        &groups,
    ))
}

fn resolve_targets_from_registry(
    handles: &[String],
    host_id: &str,
    conversations: &[Conversation],
    groups: &[crate::personas::PersonaGroup],
) -> TargetResolution {
    let mut by_handle = BTreeMap::<String, Vec<&Conversation>>::new();
    for conversation in conversations {
        by_handle
            .entry(
                conversation
                    .execution_profile
                    .mention_handle
                    .to_ascii_lowercase(),
            )
            .or_default()
            .push(conversation);
    }
    let mut groups_by_handle = BTreeMap::<String, Vec<&crate::personas::PersonaGroup>>::new();
    for group in groups {
        groups_by_handle
            .entry(group.mention_handle.to_ascii_lowercase())
            .or_default()
            .push(group);
    }
    let by_id = conversations
        .iter()
        .map(|conversation| (conversation.id.as_str(), conversation))
        .collect::<BTreeMap<_, _>>();
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    let mut ambiguous = Vec::new();
    let mut seen = BTreeSet::new();
    for handle in handles {
        let conversation_matches = by_handle.get(handle).map(Vec::as_slice).unwrap_or_default();
        let group_matches = groups_by_handle
            .get(handle)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if conversation_matches.len() + group_matches.len() > 1 {
            ambiguous.push(handle.clone());
            continue;
        }
        if let Some(group) = group_matches.first() {
            let mut group_targets = Vec::new();
            let mut group_seen = BTreeSet::new();
            let mut valid = !group.persona_ids.is_empty();
            for id in &group.persona_ids {
                if let Some(conversation) = by_id.get(id.as_str())
                    && conversation.kind == ConversationKind::PersonaTemplate
                    && group_seen.insert(conversation.id.clone())
                {
                    group_targets.push(ResolvedTarget {
                        kind: MentionTargetKind::Persona,
                        conversation: (*conversation).clone(),
                    });
                } else {
                    valid = false;
                }
            }
            if valid {
                for target in group_targets {
                    if seen.insert(target.conversation.id.clone()) {
                        resolved.push(target);
                    }
                }
            } else {
                unresolved.push(handle.clone());
            }
        } else if let Some(conversation) = conversation_matches.first()
            && conversation.id != host_id
            && seen.insert(conversation.id.clone())
        {
            resolved.push(ResolvedTarget {
                kind: if conversation.kind == ConversationKind::PersonaTemplate {
                    MentionTargetKind::Persona
                } else {
                    MentionTargetKind::LiveChat
                },
                conversation: (*conversation).clone(),
            });
        } else {
            unresolved.push(handle.clone());
        }
    }
    TargetResolution {
        targets: resolved,
        unresolved,
        ambiguous,
    }
}

fn snapshot_target(target: &ResolvedTarget) -> Result<MentionTargetSnapshot> {
    let source_messages = active_path_messages(&target.conversation);
    let encoded = serde_json::to_vec(&(
        &target.conversation.id,
        &target.conversation.execution_profile,
        &target.conversation.active_leaf_message_id,
        &source_messages,
    ))?;
    Ok(MentionTargetSnapshot {
        target_id: target.conversation.id.clone(),
        kind: target.kind,
        handle: target.conversation.execution_profile.mention_handle.clone(),
        label: target.conversation.title.clone(),
        version: target.conversation.execution_profile.version,
        source_leaf_message_id: target.conversation.active_leaf_message_id.clone(),
        profile: target.conversation.execution_profile.clone(),
        source_messages,
        snapshot_sha256: format!("{:x}", Sha256::digest(encoded)),
    })
}

#[derive(Debug)]
struct HandoffMessages {
    stable_prefix: Vec<ChatMessage>,
    messages: Vec<ChatMessage>,
}

fn resolve_mention_tools(
    bindings: &[crate::conversation_store::ToolBinding],
) -> std::result::Result<Vec<BoundMentionTool>, Blocker> {
    if bindings.len() > MAX_PERSONA_TOOL_BINDINGS {
        return Err(Blocker::new(
            "persona_tool_binding_limit_exceeded",
            format!("A Persona may attach at most {MAX_PERSONA_TOOL_BINDINGS} external tools."),
            vec!["Remove tool bindings before running this Persona.".to_string()],
        ));
    }
    let mut tools = Vec::with_capacity(bindings.len());
    let mut seen = BTreeSet::new();
    for binding in bindings {
        if !seen.insert((binding.server.as_str(), binding.tool.as_str())) {
            return Err(Blocker::new(
                "mention_tool_binding_duplicate",
                format!(
                    "The Persona attaches `{}/{}` more than once.",
                    binding.server, binding.tool
                ),
                vec!["Remove the duplicate tool binding in Settings.".to_string()],
            ));
        }
        let reviewed = cached_persona_mcp_tool_contract(&binding.server, &binding.tool)
            .map_err(|error| {
                Blocker::new(
                    "mention_tool_contract_failed",
                    error.to_string(),
                    vec!["Check this Persona's attached tools in Settings.".to_string()],
                )
            })?
            .map_err(|(_, blocker)| blocker)?;
        let contract = reviewed.tool;
        let policy = effective_persona_tool_policy(
            tool_permission_policy(&binding.server, &binding.tool).map_err(|error| {
                Blocker::new(
                    "mention_tool_permission_failed",
                    error.to_string(),
                    vec![
                        "Review configured external-process tool permissions in Settings."
                            .to_string(),
                    ],
                )
            })?,
        );
        let tool_schema_sha256 = sha256_json_value(&contract.input_schema).map_err(|error| {
            Blocker::new(
                "mention_tool_schema_identity_failed",
                error.to_string(),
                vec!["Review the attached MCP tool schema.".to_string()],
            )
        })?;
        tools.push(BoundMentionTool {
            contract,
            server: binding.server.clone(),
            server_config: reviewed.frozen_server_config,
            policy,
            server_config_sha256: reviewed.server_config_sha256,
            frozen_server_config_sha256: reviewed.frozen_server_config_sha256,
            tool_schema_sha256,
        });
    }
    validate_resolved_mention_tools(&tools)?;
    Ok(tools)
}

fn validate_resolved_mention_tools(tools: &[BoundMentionTool]) -> std::result::Result<(), Blocker> {
    if tools.len() > MAX_PERSONA_TOOL_BINDINGS {
        return Err(Blocker::new(
            "persona_tool_binding_limit_exceeded",
            format!("A Persona may attach at most {MAX_PERSONA_TOOL_BINDINGS} external tools."),
            vec!["Remove tool bindings before running this Persona.".to_string()],
        ));
    }
    let mut aggregate_schema_bytes = 0usize;
    for tool in tools {
        let schema_bytes = serde_json::to_vec(&tool.contract.input_schema).map_err(|error| {
            Blocker::new(
                "mention_tool_schema_invalid",
                error.to_string(),
                vec!["Review the attached MCP tool input schema.".to_string()],
            )
        })?;
        if schema_bytes.len() > MAX_PERSONA_TOOL_INPUT_SCHEMA_BYTES {
            return Err(Blocker::new(
                "mention_tool_schema_too_large",
                format!(
                    "Tool `{}/{}` exceeds the {} byte input-schema limit.",
                    tool.server, tool.contract.name, MAX_PERSONA_TOOL_INPUT_SCHEMA_BYTES
                ),
                vec!["Attach a smaller, bounded tool schema.".to_string()],
            ));
        }
        aggregate_schema_bytes = aggregate_schema_bytes
            .checked_add(schema_bytes.len())
            .ok_or_else(|| {
                Blocker::new(
                    "mention_tool_manifest_too_large",
                    "The attached tool input-schema byte count overflowed.",
                    vec!["Remove tool bindings before running this Persona.".to_string()],
                )
            })?;
        if aggregate_schema_bytes > MAX_PERSONA_TOOL_MANIFEST_BYTES {
            return Err(Blocker::new(
                "mention_tool_manifest_too_large",
                format!(
                    "Attached tool input schemas exceed the {} byte aggregate limit.",
                    MAX_PERSONA_TOOL_MANIFEST_BYTES
                ),
                vec!["Remove or reduce attached tool schemas.".to_string()],
            ));
        }
    }
    for (label, value) in [
        ("manifest", json!(mention_tool_manifest(tools))),
        ("decision schema", persona_tool_decision_schema(tools)),
    ] {
        let bytes = serde_json::to_vec(&value).map_err(|error| {
            Blocker::new(
                "mention_tool_manifest_invalid",
                error.to_string(),
                vec!["Review the attached MCP tool contracts.".to_string()],
            )
        })?;
        if bytes.len() > MAX_PERSONA_TOOL_MANIFEST_BYTES {
            return Err(Blocker::new(
                "mention_tool_manifest_too_large",
                format!(
                    "The Persona tool {label} exceeds the {} byte limit.",
                    MAX_PERSONA_TOOL_MANIFEST_BYTES
                ),
                vec!["Remove or reduce attached tool schemas.".to_string()],
            ));
        }
    }
    Ok(())
}

fn mention_tool_instructions(tools: &[BoundMentionTool]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    format!(
        " Attached configured external-process tools are restricted to this manifest: {}. A denied tool is unavailable. Draft the answer as ordinary prose. Do not encode or imitate a tool call in the answer; a separate constrained decision phase decides whether one exact attached call is necessary.",
        serde_json::to_string(&mention_tool_manifest(tools)).unwrap_or_else(|_| "[]".to_string())
    )
}

fn mention_tool_manifest(tools: &[BoundMentionTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "server": tool.server,
                "tool": tool.contract.name,
                "description": tool.contract.description,
                "input_schema": tool.contract.input_schema,
                "permission": match effective_persona_tool_policy(tool.policy) {
                    ToolPermissionPolicy::Ask => "ask",
                    ToolPermissionPolicy::AlwaysAllow => unreachable!("Persona policy is normalized"),
                    ToolPermissionPolicy::Deny => "deny",
                }
            })
        })
        .collect()
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum PersonaToolDecision {
    Call {
        server: String,
        tool: String,
        arguments: Value,
    },
    Final {},
}

fn persona_tool_decision_schema(tools: &[BoundMentionTool]) -> Value {
    let mut variants = vec![json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action"],
        "properties": {
            "action": {"const": "final"}
        }
    })];
    variants.extend(tools.iter().map(|binding| {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["action", "server", "tool", "arguments"],
            "properties": {
                "action": {"const": "call"},
                "server": {"const": binding.server},
                "tool": {"const": binding.contract.name},
                "arguments": binding.contract.input_schema
            }
        })
    }));
    json!({
        "title": "PersonaToolDecisionV1",
        "oneOf": variants
    })
}

fn run_persona_tool_decision(
    scope: &OperationScope,
    target: &PlannedTarget,
    invocation_id: &str,
    settings: &Settings,
) -> std::result::Result<PersonaToolDecision, Blocker> {
    if mention_cancellation_requested(scope, invocation_id, &target.snapshot.target_id) {
        return Err(Blocker::new(
            "mention_tool_decision_cancelled",
            "The Persona tool decision was cancelled before admission.",
            vec!["Retry the Persona response if the call is still needed.".to_string()],
        ));
    }
    let schema =
        serde_json::to_string(&persona_tool_decision_schema(&target.tools)).map_err(|error| {
            Blocker::new(
                "mention_tool_decision_schema_failed",
                format!("The exact Persona tool decision schema could not be encoded: {error}"),
                vec!["Review the Persona's attached tool schemas.".to_string()],
            )
        })?;
    let schema_sha256 = format!("{:x}", Sha256::digest(schema.as_bytes()));
    let schema_len = u32::try_from(schema.len()).map_err(|_| {
        Blocker::new(
            "mention_tool_decision_schema_too_large",
            "The exact Persona tool decision schema exceeds the Native artifact bound.",
            vec!["Remove or simplify attached tool schemas.".to_string()],
        )
    })?;
    let reference = ConstraintArtifactReference::new(
        format!("persona-tool-decision-v1-{}", &schema_sha256[..16]),
        schema_sha256,
        schema_len,
    )
    .map_err(|error| {
        Blocker::new(
            "mention_tool_decision_schema_invalid",
            error.message,
            vec!["Review the Persona's attached tool schemas.".to_string()],
        )
    })?;
    let manifest =
        serde_json::to_string(&mention_tool_manifest(&target.tools)).map_err(|error| {
            Blocker::new(
                "mention_tool_manifest_failed",
                format!("The attached tool manifest could not be encoded: {error}"),
                vec!["Review the Persona's attached tools.".to_string()],
            )
        })?;
    let decision_input = serde_json::to_string(&json!({
        "addressed_message": target
            .messages
            .last()
            .map(|message| message.content.as_str())
            .unwrap_or_default(),
    }))
    .map_err(|error| {
        Blocker::new(
            "mention_tool_decision_input_failed",
            format!("The Persona tool decision input could not be encoded: {error}"),
            vec!["Retry the Persona response.".to_string()],
        )
    })?;
    let messages = vec![
        ChatMessage {
            role: ChatRole::System,
            content: format!(
                "You are a constrained control phase, not the Persona answer. Return exactly one schema-valid decision object. You never receive or inspect the Persona's answer prose. Choose final unless one exact attached configured external-process tool call is necessary to answer the addressed message. At most one call is permitted. Attached manifest: {manifest}"
            ),
        },
        ChatMessage {
            role: ChatRole::User,
            content: decision_input,
        },
    ];
    let template = profile_chat_template(&target.snapshot.profile);
    let mut prepared = target
        .handle
        .prepare_input(GenerationInput::Chat { messages, template })
        .map_err(|error| {
            Blocker::new(
                "mention_tool_decision_prompt_failed",
                error.message,
                vec!["Check the target model's chat template.".to_string()],
            )
        })?;
    if prepared.len() != 1 {
        return Err(Blocker::new(
            "mention_tool_decision_prompt_ambiguous",
            "Native prompt preparation did not return exactly one Persona tool decision prompt.",
            vec!["Check the target model's chat template.".to_string()],
        ));
    }
    let prepared = prepared.pop().expect("length checked");
    let writer = target
        .handle
        .controlled_model_identity("persona-tool-decision")
        .map_err(|error| {
            Blocker::new(
                "mention_tool_decision_identity_failed",
                error.message,
                vec!["Retry after reloading the target model.".to_string()],
            )
        })?;
    let control = ControlProgram::new(
        writer,
        Vec::new(),
        Some(StructuredConstraint::JsonSchema { reference }),
        Vec::new(),
        ExtendedSamplerProgram::default(),
        TerminalSelector::Distribution,
        DistributionObservationPolicy::default(),
        Vec::new(),
    )
    .map_err(|error| {
        Blocker::new(
            "mention_tool_decision_control_failed",
            error.message,
            vec!["Review the target model and attached tool schemas.".to_string()],
        )
    })?;
    let mut sampling = target
        .snapshot
        .profile
        .sampling
        .clone()
        .unwrap_or_else(|| settings.sampling_config());
    sampling.max_tokens = sampling
        .max_tokens
        .clamp(64, PERSONA_TOOL_DECISION_MAX_TOKENS);
    sampling.temperature = sampling.temperature.min(0.2);
    sampling.dynamic_temperature_range = 0.0;
    sampling.stop.clear();
    let case = ControlledGenerationCase::new(
        target.snapshot.target_id.clone(),
        ExactTokenPrompt::new(prepared.token_ids).map_err(|error| {
            Blocker::new(
                "mention_tool_decision_prompt_invalid",
                error.message,
                vec!["Reduce the addressed message or Persona context.".to_string()],
            )
        })?,
        None,
        sampling,
    )
    .map_err(|error| {
        Blocker::new(
            "mention_tool_decision_case_invalid",
            error.message,
            vec!["Review the target model and Persona settings.".to_string()],
        )
    })?;
    let request =
        ControlledGenerationBatchRequest::new(invocation_id.to_string(), vec![case], control)
            .map_err(|error| {
                Blocker::new(
                    "mention_tool_decision_request_invalid",
                    error.message,
                    vec!["Reduce the addressed message or Persona context.".to_string()],
                )
            })?;
    let submission =
        ControlledGenerationSubmission::new(request, Some(schema)).map_err(|error| {
            Blocker::new(
                "mention_tool_decision_constraint_invalid",
                error.message,
                vec!["Review the Persona's attached tool schemas.".to_string()],
            )
        })?;
    let verified = target
        .handle
        .generate_controlled(submission)
        .map_err(|error| {
            Blocker::new(
                "mention_tool_decision_admission_failed",
                error.message,
                vec!["Retry the Persona response.".to_string()],
            )
        })?
        .wait_verified()
        .map_err(|error| {
            Blocker::new(
                "mention_tool_decision_failed",
                error.message,
                vec!["Retry the Persona response.".to_string()],
            )
        })?;
    let cases = verified.output().cases();
    if cases.len() != 1 || cases[0].case_id() != target.snapshot.target_id {
        return Err(Blocker::new(
            "mention_tool_decision_output_ambiguous",
            "Native returned a Persona tool decision for the wrong target.",
            vec!["Retry after reloading the target model.".to_string()],
        ));
    }
    let generation = cases[0].generation();
    if !generation.real_engine_invoked || generation.fake_fixture {
        return Err(Blocker::new(
            "mention_tool_decision_not_real",
            "The Persona tool decision lacks real Native execution evidence.",
            vec!["Run with the configured local model.".to_string()],
        ));
    }
    serde_json::from_str(generation.text.trim()).map_err(|error| {
        Blocker::new(
            "mention_tool_decision_invalid",
            format!("The constrained Persona tool decision was not complete: {error}"),
            vec!["Retry the Persona response.".to_string()],
        )
    })
}

fn finish_tool_bound_mention(
    scope: &OperationScope,
    target: &PlannedTarget,
    output: GenerationOutput,
    invocation_id: &str,
    host_conversation_id: &str,
    user_message_id: &str,
    settings: &Settings,
) -> std::result::Result<ToolBoundMentionFinish, Blocker> {
    let decision = run_persona_tool_decision(scope, target, invocation_id, settings)?;
    let PersonaToolDecision::Call {
        server,
        tool,
        arguments,
    } = decision
    else {
        return Ok(ToolBoundMentionFinish::Final {
            output,
            tool_receipt_ids: Vec::new(),
        });
    };
    let binding = validate_bound_tool(
        &target.tools,
        &server,
        &tool,
        &arguments,
        &target.snapshot.handle,
    )?;
    match effective_persona_tool_policy(binding.policy) {
        ToolPermissionPolicy::Deny => Err(Blocker::new(
            "tool_permission_denied",
            format!("Tool `{server}/{tool}` is denied by local policy."),
            vec!["Change or remove the tool permission in Settings.".to_string()],
        )),
        ToolPermissionPolicy::Ask | ToolPermissionPolicy::AlwaysAllow => {
            if target.snapshot.kind != MentionTargetKind::Persona {
                return Err(Blocker::new(
                    "mention_tool_approval_persona_required",
                    "Exact resumable approval is available only for a frozen Persona invocation.",
                    vec!["Use a Persona target or change the binding policy.".to_string()],
                ));
            }
            prepare_persona_tool_approval(
                target,
                output,
                binding,
                invocation_id,
                host_conversation_id,
                user_message_id,
                server,
                tool,
                arguments,
                settings,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_persona_tool_approval(
    target: &PlannedTarget,
    provisional_output: GenerationOutput,
    binding: &BoundMentionTool,
    invocation_id: &str,
    host_conversation_id: &str,
    user_message_id: &str,
    server: String,
    tool: String,
    arguments: Value,
    settings: &Settings,
) -> std::result::Result<ToolBoundMentionFinish, Blocker> {
    let status = target.handle.status();
    let Some(model_fingerprint) = status.fingerprint else {
        return Err(Blocker::new(
            "mention_tool_approval_model_identity_missing",
            "The frozen Persona invocation has no exact model fingerprint.",
            vec!["Reload the local model and retry the Persona response.".to_string()],
        ));
    };
    let mut frozen_model_config = target.model_config.clone();
    frozen_model_config.expected_model_sha256 = Some(model_fingerprint.model_sha256.clone());
    frozen_model_config.expected_mmproj_sha256 =
        model_fingerprint.multimodal_projector_sha256.clone();
    let model_config_sha256 = sha256_json(&frozen_model_config).map_err(|error| {
        Blocker::new(
            "mention_tool_approval_model_config_invalid",
            error.to_string(),
            vec!["Reload the local model and retry the Persona response.".to_string()],
        )
    })?;
    let arguments_sha256 = sha256_json_value(&arguments).map_err(|error| {
        Blocker::new(
            "mention_tool_approval_arguments_invalid",
            error.to_string(),
            vec!["Review the exact tool arguments.".to_string()],
        )
    })?;
    let now = now_ms();
    let deadline_clock = capture_persona_tool_deadline_clock(now).map_err(|error| {
        Blocker::new(
            "mention_tool_approval_deadline_unavailable",
            error.to_string(),
            vec!["Persona tool approvals are unavailable on this platform.".to_string()],
        )
    })?;
    validate_frozen_mcp_server(
        &binding.server_config,
        &settings.data_dir,
        &binding.frozen_server_config_sha256,
    )
    .map_err(|error| {
        Blocker::new(
            "mention_tool_approval_executable_not_frozen",
            error.to_string(),
            vec!["Refresh the reviewed MCP tool catalog.".to_string()],
        )
    })?;
    let frozen_mcp_server_config = binding.server_config.clone();
    let frozen_mcp_server_config_sha256 = binding.frozen_server_config_sha256.clone();
    let approval_id = Uuid::new_v4().to_string();
    let call_sha256 = persona_tool_call_sha256(PersonaToolCallIdentity {
        invocation_id,
        host_conversation_id,
        user_message_id,
        target_id: &target.snapshot.target_id,
        persona_version: target.snapshot.version,
        snapshot_sha256: &target.snapshot.snapshot_sha256,
        server: &server,
        tool: &tool,
        arguments_sha256: &arguments_sha256,
        server_config_sha256: &binding.server_config_sha256,
        frozen_server_config_sha256: &frozen_mcp_server_config_sha256,
        tool_schema_sha256: &binding.tool_schema_sha256,
        model_config_sha256: &model_config_sha256,
        model_fingerprint: &model_fingerprint,
    })
    .map_err(|error| {
        Blocker::new(
            "mention_tool_approval_hash_failed",
            error.to_string(),
            vec!["Retry the Persona response.".to_string()],
        )
    })?;
    let approval = MentionToolApproval {
        id: approval_id.clone(),
        invocation_id: invocation_id.to_string(),
        host_conversation_id: host_conversation_id.to_string(),
        user_message_id: user_message_id.to_string(),
        target_id: target.snapshot.target_id.clone(),
        handle: target.snapshot.handle.clone(),
        label: target.snapshot.label.clone(),
        persona_version: target.snapshot.version,
        snapshot_sha256: target.snapshot.snapshot_sha256.clone(),
        server,
        tool,
        arguments,
        arguments_sha256,
        server_config_sha256: binding.server_config_sha256.clone(),
        tool_schema_sha256: binding.tool_schema_sha256.clone(),
        model_config_sha256,
        call_sha256,
        effect_receipt_id: None,
        effect_outcome: None,
        created_at_ms: now,
        expires_at_ms: now.saturating_add(PERSONA_TOOL_APPROVAL_TTL_MS),
        consumed_at_ms: None,
        decision: None,
        state: MentionToolApprovalState::Pending,
    };
    let continuation = FrozenMentionToolContinuation {
        approval_id,
        model_path: target.model_path.clone(),
        mmproj_path: target.snapshot.profile.mmproj_path.clone(),
        model_config: Some(frozen_model_config),
        model_fingerprint,
        chat_template: profile_chat_template(&target.snapshot.profile),
        sampling: target
            .snapshot
            .profile
            .sampling
            .clone()
            .unwrap_or_else(|| settings.sampling_config()),
        messages: target.messages.clone(),
        provisional_output,
        input_schema: binding.contract.input_schema.clone(),
        mcp_server_config: Some(frozen_mcp_server_config),
        mcp_server_config_sha256: frozen_mcp_server_config_sha256,
        deadline_clock: Some(deadline_clock),
        resume_lease_id: None,
    };
    Ok(ToolBoundMentionFinish::Pending {
        approval,
        continuation: Box::new(continuation),
    })
}

struct PersonaToolCallIdentity<'a> {
    invocation_id: &'a str,
    host_conversation_id: &'a str,
    user_message_id: &'a str,
    target_id: &'a str,
    persona_version: u64,
    snapshot_sha256: &'a str,
    server: &'a str,
    tool: &'a str,
    arguments_sha256: &'a str,
    server_config_sha256: &'a str,
    frozen_server_config_sha256: &'a str,
    tool_schema_sha256: &'a str,
    model_config_sha256: &'a str,
    model_fingerprint: &'a ModelFingerprint,
}

fn persona_tool_call_sha256(identity: PersonaToolCallIdentity<'_>) -> Result<String> {
    let canonical = serde_json::to_vec(&json!({
        "schema": "mom_llama.persona_tool_call_identity.v3",
        "invocation_id": identity.invocation_id,
        "host_conversation_id": identity.host_conversation_id,
        "user_message_id": identity.user_message_id,
        "target_id": identity.target_id,
        "persona_version": identity.persona_version,
        "snapshot_sha256": identity.snapshot_sha256,
        "server": identity.server,
        "tool": identity.tool,
        "arguments_sha256": identity.arguments_sha256,
        "server_config_sha256": identity.server_config_sha256,
        "frozen_server_config_sha256": identity.frozen_server_config_sha256,
        "tool_schema_sha256": identity.tool_schema_sha256,
        "model_config_sha256": identity.model_config_sha256,
        "model_fingerprint": identity.model_fingerprint,
    }))?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

fn sha256_json_value(value: &Value) -> Result<String> {
    sha256_json(value)
}

fn sha256_json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn persona_tool_effect_receipt_id() -> String {
    format!("mom_llama.persona_tool_effect:{}", Uuid::new_v4())
}

fn validate_bound_tool<'a>(
    tools: &'a [BoundMentionTool],
    server: &str,
    tool: &str,
    arguments: &Value,
    handle: &str,
) -> std::result::Result<&'a BoundMentionTool, Blocker> {
    let Some(binding) = tools
        .iter()
        .find(|candidate| candidate.server == server && candidate.contract.name == tool)
    else {
        return Err(Blocker::new(
            "mention_tool_not_attached",
            format!("@{handle} requested a tool that is not attached to this Persona."),
            vec!["Review the Persona's attached tools in Settings.".to_string()],
        ));
    };
    if let Some(blocker) = validate_tool_arguments(&binding.contract.input_schema, arguments) {
        return Err(blocker);
    }
    Ok(binding)
}

fn effective_persona_tool_policy(policy: ToolPermissionPolicy) -> ToolPermissionPolicy {
    // Persona calls carry frozen model and conversation authority across a user-visible
    // decision boundary. Until an automatic durable intent/claim path exists, the generic
    // tool-loop AlwaysAllow setting cannot bypass that boundary.
    match policy {
        ToolPermissionPolicy::AlwaysAllow => ToolPermissionPolicy::Ask,
        policy => policy,
    }
}

#[cfg(test)]
fn authorize_bound_tool<'a>(
    tools: &'a [BoundMentionTool],
    server: &str,
    tool: &str,
    arguments: &Value,
    handle: &str,
) -> std::result::Result<&'a BoundMentionTool, Blocker> {
    let binding = validate_bound_tool(tools, server, tool, arguments, handle)?;
    match effective_persona_tool_policy(binding.policy) {
        ToolPermissionPolicy::Deny => Err(Blocker::new(
            "tool_permission_denied",
            format!("Tool `{server}/{tool}` is denied by local policy."),
            vec!["Change or remove the tool permission in Settings.".to_string()],
        )),
        ToolPermissionPolicy::Ask => Err(Blocker::new(
            "mention_tool_approval_required",
            format!(
                "Tool `{server}/{tool}` needs explicit approval before this Persona can continue."
            ),
            vec!["Approve or deny this exact Persona tool call.".to_string()],
        )),
        ToolPermissionPolicy::AlwaysAllow => unreachable!("Persona policy is normalized"),
    }
}

fn handoff_messages(
    handle: &NativeModelHandle,
    settings: &Settings,
    snapshot: &MentionTargetSnapshot,
    host: &[Message],
    addressed: &str,
    participants: &str,
    tools: &[BoundMentionTool],
) -> std::result::Result<HandoffMessages, Blocker> {
    let system = snapshot
        .profile
        .system_message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
        .map(|content| ChatMessage {
            role: ChatRole::System,
            content: content.to_string(),
        });
    let source_messages =
        attachment_enriched_messages(&snapshot.target_id, &snapshot.source_messages)?;
    let host_conversation_id = host
        .first()
        .map(|message| message.conversation_id.as_str())
        .unwrap_or_default();
    let host_messages = attachment_enriched_messages(host_conversation_id, host)?;
    let source_candidates = source_messages
        .iter()
        .flat_map(|message| native_context_messages(message, false))
        .collect::<Vec<_>>();
    let host_candidates = host_messages
        .iter()
        .flat_map(|message| native_context_messages(message, false))
        .collect::<Vec<_>>();
    let template = profile_chat_template(&snapshot.profile);
    let source = recent_within_budget(
        handle,
        source_candidates,
        snapshot.profile.source_history_tokens,
        &template,
    );
    let mut host = recent_within_budget(
        handle,
        host_candidates,
        snapshot.profile.host_context_tokens,
        &template,
    );
    let boundary = ChatMessage {
        role: ChatRole::System,
        content: format!(
            "You are @{}, temporarily invited from a separate local conversation. Your source history above is an immutable snapshot and will not be changed by this reply. Recent host context follows. Reply directly to the final addressed message as your established perspective. Do not claim access to omitted history. Addressed participants: {}.{}",
            snapshot.handle,
            participants,
            mention_tool_instructions(tools)
        ),
    };
    let final_message = ChatMessage {
        role: ChatRole::User,
        content: addressed.trim().to_string(),
    };
    let mut messages = Vec::new();
    if let Some(system) = system {
        messages.push(system);
    }
    messages.extend(source.clone());
    messages.push(boundary.clone());
    messages.append(&mut host);
    messages.push(final_message.clone());
    let output_reserve = snapshot
        .profile
        .sampling
        .as_ref()
        .map(|sampling| sampling.max_tokens)
        .unwrap_or(settings.default_max_tokens) as usize;
    fit_handoff_to_context(
        messages,
        &boundary,
        output_reserve,
        settings.context_tokens as usize,
        |candidate| {
            handle
                .tokenize_messages_with_template(candidate.to_vec(), template.clone())
                .map(|tokens| tokens.token_ids.len())
                .map_err(|error| {
                    Blocker::new(
                        "mention_context_tokenization_failed",
                        error.message,
                        vec!["Check the target model's chat template.".to_string()],
                    )
                })
        },
    )
}

fn attachment_enriched_messages(
    conversation_id: &str,
    messages: &[Message],
) -> std::result::Result<Vec<Message>, Blocker> {
    if conversation_id.is_empty()
        || messages
            .iter()
            .all(|message| message.attachment_ids.is_empty())
    {
        return Ok(messages.to_vec());
    }
    let context = prepare_chat_attachments(conversation_id, messages, Some("__snapshot__"))
        .map_err(|_| {
            Blocker::new(
                "mention_attachment_context_failed",
                "An invited conversation's attachment context could not be loaded.",
                vec!["Open the source chat and inspect its attachments.".to_string()],
            )
        })?
        .map_err(|blocked| blocked.blocker)?;
    if !context.media.is_empty() {
        return Err(Blocker::new(
            "mention_source_multimodal_attachment_unsupported",
            "An invited source contains image or audio context that the shared-prefix mention dispatcher cannot preserve yet.",
            vec!["Use a text-only source branch for this Persona invocation.".to_string()],
        ));
    }
    Ok(messages
        .iter()
        .cloned()
        .map(|mut message| {
            if let Some(attachment_text) = context.text_by_message_id.get(&message.id) {
                message.content = append_attachment_context(&message.content, attachment_text);
            }
            message
        })
        .collect())
}

fn fit_handoff_to_context(
    mut messages: Vec<ChatMessage>,
    boundary: &ChatMessage,
    output_reserve: usize,
    context_tokens: usize,
    mut token_count: impl FnMut(&[ChatMessage]) -> std::result::Result<usize, Blocker>,
) -> std::result::Result<HandoffMessages, Blocker> {
    loop {
        let tokens = token_count(&messages)?;
        if tokens.saturating_add(output_reserve) <= context_tokens {
            let boundary_index = messages
                .iter()
                .position(|message| message == boundary)
                .unwrap_or_default();
            return Ok(HandoffMessages {
                stable_prefix: messages[..boundary_index].to_vec(),
                messages,
            });
        }
        let boundary_index = messages
            .iter()
            .position(|message| message == boundary)
            .unwrap_or_default();
        if messages.len() > boundary_index + 2 {
            messages.remove(boundary_index + 1);
            continue;
        }
        let first_non_system = usize::from(
            messages
                .first()
                .is_some_and(|message| message.role == ChatRole::System),
        );
        if boundary_index > first_non_system {
            messages.remove(first_non_system);
            continue;
        }
        return Err(Blocker::new(
            "mention_context_too_large",
            "The persona instructions and addressed message do not fit the target model context.",
            vec!["Reduce the persona system message or output-token setting.".to_string()],
        ));
    }
}

fn recent_within_budget(
    handle: &NativeModelHandle,
    messages: Vec<ChatMessage>,
    budget: u32,
    chat_template: &ChatTemplateChoice,
) -> Vec<ChatMessage> {
    if budget == 0 {
        return Vec::new();
    }
    let mut selected = Vec::new();
    for message in messages.into_iter().rev() {
        let mut candidate = vec![message];
        candidate.extend(selected.clone());
        let fits = handle
            .tokenize_messages_with_template(candidate.clone(), chat_template.clone())
            .map(|tokens| tokens.token_ids.len() <= budget as usize)
            .unwrap_or(false);
        if !fits {
            break;
        }
        selected = candidate;
    }
    selected
}

fn profile_chat_template(profile: &ConversationExecutionProfile) -> ChatTemplateChoice {
    match &profile.chat_template {
        crate::conversation_store::ChatTemplatePolicy::ModelDefault => {
            ChatTemplateChoice::ModelDefault
        }
        crate::conversation_store::ChatTemplatePolicy::FrozenSource(template) => {
            ChatTemplateChoice::Override(template.clone())
        }
    }
}

fn append_attributed_results(host: &mut Conversation, invocation: &mut MentionInvocation) {
    let advance_active_leaf =
        host.active_leaf_message_id.as_deref() == Some(invocation.user_message_id.as_str())
            || host
                .active_leaf_message_id
                .as_deref()
                .is_some_and(|active_leaf| {
                    host.messages.iter().any(|message| {
                        message.id == active_leaf
                            && message.attribution.as_ref().is_some_and(|attribution| {
                                attribution.invocation_id == invocation.id
                            })
                    })
                });
    let mut parent = Some(invocation.user_message_id.clone());
    for (order, result) in invocation.results.iter_mut().enumerate() {
        if result.state != GenerationState::Completed || result.text.trim().is_empty() {
            continue;
        }
        let snapshot = invocation
            .targets
            .iter()
            .find(|target| target.target_id == result.target_id)
            .expect("mention result target snapshot");
        if let Some(message_id) = result
            .message_id
            .as_ref()
            .filter(|message_id| {
                host.messages
                    .iter()
                    .any(|message| message.id.as_str() == message_id.as_str())
            })
            .cloned()
        {
            parent = Some(message_id);
            continue;
        }
        let message_id = result
            .message_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        host.messages.push(Message {
            id: message_id.clone(),
            conversation_id: host.id.clone(),
            role: MessageRole::Assistant,
            content: result.text.clone(),
            created_at: now_ms().to_string(),
            parent_id: parent.clone(),
            model: Some(result.model_id.clone()),
            receipt_id: Some(format!("mom_llama.mention_dispatch:{}", invocation.id)),
            prompt_tokens: Some(result.metrics.prompt_tokens),
            completion_tokens: Some(result.metrics.completion_tokens),
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: Some(MessageAttribution {
                kind: match snapshot.kind {
                    MentionTargetKind::Persona => MessageSpeakerKind::Persona,
                    MentionTargetKind::LiveChat => MessageSpeakerKind::LiveChat,
                    MentionTargetKind::Group => MessageSpeakerKind::Persona,
                },
                source_id: snapshot.target_id.clone(),
                handle: snapshot.handle.clone(),
                label: snapshot.label.clone(),
                version: snapshot.version,
                invocation_id: invocation.id.clone(),
                target_order: order,
            }),
            attachment_ids: Vec::new(),
        });
        result.message_id = Some(message_id.clone());
        parent = Some(message_id);
    }
    if advance_active_leaf && parent.as_deref() != Some(invocation.user_message_id.as_str()) {
        host.active_leaf_message_id = parent;
    }
    host.updated_at = now_ms().to_string();
}

fn save_invocation(invocation: &MentionInvocation) -> Result<()> {
    RuntimeStore::current()?.mutate_documents(
        INVOCATIONS_NAMESPACE,
        MentionInvocationDb::default,
        |db, documents| {
            if let Some(stored) = db
                .invocations
                .iter_mut()
                .find(|stored| stored.id == invocation.id)
            {
                stored.invocation = invocation.clone();
            } else {
                db.invocations.insert(
                    0,
                    StoredMentionInvocation {
                        invocation: invocation.clone(),
                        frozen_tool_continuations: Vec::new(),
                    },
                );
            }
            write_active_approval_index(db, documents)
        },
    )
}

fn upsert_invocation_with_continuations(
    db: &mut MentionInvocationDb,
    invocation: &MentionInvocation,
    continuations: Vec<FrozenMentionToolContinuation>,
) -> Result<()> {
    let stored = if let Some(stored) = db
        .invocations
        .iter_mut()
        .find(|stored| stored.id == invocation.id)
    {
        stored
    } else {
        db.invocations.insert(
            0,
            StoredMentionInvocation {
                invocation: invocation.clone(),
                frozen_tool_continuations: Vec::new(),
            },
        );
        &mut db.invocations[0]
    };
    let mut frozen = std::mem::take(&mut stored.frozen_tool_continuations);
    for continuation in continuations {
        if frozen
            .iter()
            .any(|candidate| candidate.approval_id == continuation.approval_id)
        {
            anyhow::bail!(
                "duplicate frozen Persona tool continuation {}",
                continuation.approval_id
            );
        }
        frozen.push(continuation);
    }
    stored.invocation = invocation.clone();
    stored.frozen_tool_continuations = frozen;
    validate_active_approval_bound(db)?;
    Ok(())
}

fn validate_active_approval_bound(db: &MentionInvocationDb) -> Result<()> {
    let active = db
        .invocations
        .iter()
        .filter(|stored| {
            stored.tool_approvals.iter().any(|approval| {
                matches!(
                    approval.state,
                    MentionToolApprovalState::Pending | MentionToolApprovalState::Resuming
                )
            })
        })
        .count();
    if active > MAX_ACTIVE_APPROVAL_INVOCATIONS {
        anyhow::bail!(
            "active Persona approval invocations exceed the bounded limit {}",
            MAX_ACTIVE_APPROVAL_INVOCATIONS
        );
    }
    Ok(())
}

fn active_approval_index(db: &MentionInvocationDb) -> Result<ActivePersonaToolApprovalIndex> {
    validate_active_approval_bound(db)?;
    let mut approvals = Vec::new();
    for stored in &db.invocations {
        for approval in &stored.tool_approvals {
            if !matches!(
                approval.state,
                MentionToolApprovalState::Pending | MentionToolApprovalState::Resuming
            ) {
                continue;
            }
            let resume_lease_id = stored
                .frozen_tool_continuations
                .iter()
                .find(|continuation| continuation.approval_id == approval.id)
                .and_then(|continuation| continuation.resume_lease_id.clone());
            let deadline_clock = stored
                .frozen_tool_continuations
                .iter()
                .find(|continuation| continuation.approval_id == approval.id)
                .and_then(|continuation| continuation.deadline_clock);
            approvals.push(ActivePersonaToolApproval {
                invocation_id: stored.id.clone(),
                approval_id: approval.id.clone(),
                target_id: approval.target_id.clone(),
                created_at_ms: approval.created_at_ms,
                expires_at_ms: approval.expires_at_ms,
                monotonic_created_ms: deadline_clock.map(|clock| clock.monotonic_created_ms),
                boot_wall_anchor_ms: deadline_clock.map(|clock| clock.boot_wall_anchor_ms),
                state: approval.state,
                decision: approval.decision,
                resume_lease_id,
            });
        }
    }
    approvals.sort_by(|left, right| {
        left.invocation_id
            .cmp(&right.invocation_id)
            .then_with(|| left.approval_id.cmp(&right.approval_id))
    });
    Ok(ActivePersonaToolApprovalIndex { approvals })
}

fn write_active_approval_index(
    db: &MentionInvocationDb,
    documents: &mut DocumentMutations<'_, '_, '_>,
) -> Result<()> {
    let index = active_approval_index(db)?;
    documents.put_bytes(ACTIVE_APPROVALS_NAMESPACE, &serde_json::to_vec(&index)?)
}

fn blocked_target_result(
    snapshot: &MentionTargetSnapshot,
    state: GenerationState,
    message: &str,
) -> MentionTargetResult {
    MentionTargetResult {
        target_id: snapshot.target_id.clone(),
        handle: snapshot.handle.clone(),
        label: snapshot.label.clone(),
        state,
        text: message.to_string(),
        model_id: String::new(),
        message_id: None,
        metrics: GenerationMetrics::default(),
        cache_id: None,
        cache_reused: false,
        tool_receipt_ids: Vec::new(),
        real_engine_invoked: false,
        fake_fixture: false,
    }
}

fn invocation_state(results: &[MentionTargetResult]) -> MentionInvocationState {
    let completed = results
        .iter()
        .filter(|result| result.state == GenerationState::Completed)
        .count();
    let cancelled = results
        .iter()
        .filter(|result| result.state == GenerationState::Cancelled)
        .count();
    if completed == results.len() && completed > 0 {
        MentionInvocationState::Completed
    } else if completed > 0 {
        MentionInvocationState::PartiallyCompleted
    } else if cancelled == results.len() && cancelled > 0 {
        MentionInvocationState::Cancelled
    } else {
        MentionInvocationState::Failed
    }
}

fn parse_handles(message: &str) -> Vec<String> {
    let mut handles = Vec::new();
    for token in mention_tokens(message) {
        let handle = token.handle.to_ascii_lowercase();
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    handles
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MentionToken<'a> {
    start: usize,
    end: usize,
    handle: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeDelimiter {
    marker: char,
    width: usize,
}

fn mention_tokens(message: &str) -> Vec<MentionToken<'_>> {
    let chars = message.char_indices().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut code_delimiter: Option<CodeDelimiter> = None;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index].1;
        if character == '`' || character == '~' {
            let marker = character;
            let mut width = 1;
            while index + width < chars.len() && chars[index + width].1 == marker {
                width += 1;
            }
            match code_delimiter {
                Some(open) if open.marker == marker && open.width == width => {
                    code_delimiter = None;
                }
                None if marker == '`' || width >= 3 => {
                    code_delimiter = Some(CodeDelimiter { marker, width });
                }
                _ => {}
            }
            index += width;
            continue;
        }
        let explicit_boundary = index == 0 || chars[index - 1].1.is_whitespace();
        if code_delimiter.is_none()
            && character == '@'
            && explicit_boundary
            && !is_indented_code_position(message, chars[index].0)
        {
            let start = chars[index].0 + 1;
            let mut end = start;
            index += 1;
            while index < chars.len()
                && (chars[index].1.is_ascii_alphanumeric() || chars[index].1 == '-')
            {
                end = chars[index].0 + chars[index].1.len_utf8();
                index += 1;
            }
            if end > start {
                tokens.push(MentionToken {
                    start: start - 1,
                    end,
                    handle: &message[start..end],
                });
            }
        } else {
            index += 1;
        }
    }
    tokens
}

fn is_indented_code_position(message: &str, byte_index: usize) -> bool {
    let line_start = message[..byte_index]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let prefix = &message[line_start..byte_index];
    prefix.starts_with('\t') || (prefix.len() >= 4 && prefix.bytes().all(|byte| byte == b' '))
}

fn strip_handles(message: &str, targets: &[ResolvedTarget]) -> String {
    let target_handles = targets
        .iter()
        .map(|target| {
            target
                .conversation
                .execution_profile
                .mention_handle
                .to_ascii_lowercase()
        })
        .collect::<BTreeSet<_>>();
    let removals = mention_tokens(message)
        .into_iter()
        .filter(|token| target_handles.contains(&token.handle.to_ascii_lowercase()))
        .map(|token| {
            let mut end = token.end;
            for (offset, character) in message[token.end..].char_indices() {
                if character.is_whitespace() || character.is_ascii_alphanumeric() {
                    break;
                }
                end = token.end + offset + character.len_utf8();
            }
            token.start..end
        })
        .collect::<Vec<_>>();
    let mut stripped = String::with_capacity(message.len());
    let mut cursor = 0;
    for removal in removals {
        stripped.push_str(&message[cursor..removal.start]);
        cursor = removal.end;
    }
    stripped.push_str(&message[cursor..]);
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn append_attachment_context(message: &str, attachment_text: &str) -> String {
    if attachment_text.is_empty() {
        message.to_string()
    } else if message.is_empty() {
        attachment_text.to_string()
    } else {
        format!("{message}\n\n{attachment_text}")
    }
}

fn cache_owner(snapshot: &MentionTargetSnapshot) -> String {
    format!(
        "mention:{}:{}:{}",
        snapshot.target_id, snapshot.version, snapshot.snapshot_sha256
    )
}

fn emit<F>(callback: &mut Option<F>, event: MentionStreamEvent) -> Result<()>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    if let Some(callback) = callback.as_mut() {
        callback(ChatDispatchStreamEvent::Mention(event))?;
    }
    Ok(())
}

const fn candidate_rank(kind: MentionTargetKind) -> u8 {
    match kind {
        MentionTargetKind::Persona => 0,
        MentionTargetKind::Group => 1,
        MentionTargetKind::LiveChat => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ACTIVE_APPROVALS_NAMESPACE, INVOCATIONS_NAMESPACE, MentionInvocationDb,
        MentionToolEffectOutcome, PersonaToolApprovalRecovery,
        upsert_invocation_with_continuations, write_active_approval_index,
    };
    use super::{
        ApprovalDeadlineTracker, BoundMentionTool, FrozenMentionToolContinuation,
        MAX_PERSONA_TOOL_INPUT_SCHEMA_BYTES, MAX_PERSONA_TOOL_MANIFEST_BYTES, MentionCancelControl,
        MentionInvocation, MentionInvocationState, MentionTargetKind, MentionTargetSnapshot,
        MentionToolApproval, MentionToolApprovalDecision, MentionToolApprovalResolution,
        MentionToolApprovalState, PersonaToolCallIdentity, PersonaToolDecision,
        StoredMentionInvocation, active_approval_index, ambiguous_resolution_blocker,
        authorize_bound_tool, finish_stored_mention_invocation, fit_handoff_to_context,
        mention_tool_instructions, mention_tool_manifest, parse_handles, persona_tool_call_sha256,
        persona_tool_decision_schema, reconcile_stored_persona_tool_approvals,
        resolve_targets_from_registry, sha256_json_value, unknown_persona_tool_effect_receipt,
        unregister_exact_mentions, validate_resolved_mention_tools,
    };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use super::{
        PersonaToolResumeLease, claim_stored_persona_tool_approval,
        persona_tool_resume_lease_is_live, project_visible_tool_approvals,
    };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use crate::conversation_store::{CONVERSATIONS_NAMESPACE, Message, MessageRole};
    use crate::conversation_store::{
        Conversation, ConversationDb, ConversationExecutionProfile, ConversationKind,
    };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use crate::mcp::exact_mcp_server_config_sha256;
    use crate::mcp::{McpServerConfig, McpTool};
    use crate::store::RuntimeStore;
    use crate::tool_loop::ToolPermissionPolicy;
    use llama_native_types::{
        ChatMessage, ChatRole, ChatTemplateChoice, GenerationMetrics, GenerationOutput,
        GenerationState, ModelFingerprint, NativeModelConfig, NativeTransport, SamplingConfig,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct RemoveTestDir(PathBuf);

    impl Drop for RemoveTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn handoff_message(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_string(),
        }
    }

    fn mention_conversation(id: &str, handle: &str) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: id.to_string(),
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
            kind: ConversationKind::PersonaTemplate,
            execution_profile: ConversationExecutionProfile {
                mention_handle: handle.to_string(),
                ..ConversationExecutionProfile::default()
            },
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
        }
    }

    fn model_fingerprint(model_id: &str) -> ModelFingerprint {
        ModelFingerprint {
            model_id: model_id.to_string(),
            model_size: 42,
            model_sha256: "a".repeat(64),
            tokenizer_sha256: "b".repeat(64),
            chat_template_sha256: "c".repeat(64),
            multimodal_projector_sha256: None,
            binding_version: "binding-v1".to_string(),
            build_id: "build-v1".to_string(),
            backend: "test".to_string(),
            context_tokens: 4096,
            batch_tokens: 512,
            max_sequences: 1,
            rope_config_sha256: "d".repeat(64),
            kv_layout_sha256: "e".repeat(64),
        }
    }

    fn frozen_approval_record(expires_at_ms: u128) -> StoredMentionInvocation {
        let fingerprint = model_fingerprint("model");
        let mut model_config = NativeModelConfig::local(PathBuf::from("model.gguf"));
        model_config.expected_model_sha256 = Some(fingerprint.model_sha256.clone());
        model_config.context_tokens = fingerprint.context_tokens;
        model_config.batch_tokens = fingerprint.batch_tokens;
        model_config.max_sequences = fingerprint.max_sequences;
        let model_config_sha256 = super::sha256_json(&model_config).expect("model config hash");
        let arguments = json!({"query": "exact"});
        let arguments_sha256 = sha256_json_value(&arguments).expect("arguments hash");
        let frozen_server_config = McpServerConfig {
            name: "local".to_string(),
            command: std::env::current_exe().expect("test executable path"),
            args: Vec::new(),
            enabled: true,
        };
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let server_config_sha256 =
            exact_mcp_server_config_sha256(&frozen_server_config).expect("test server config hash");
        // Process-supervised MCP and its executable identity authority are
        // deliberately unavailable on other platforms. State-only approval
        // tests still need a stable opaque identity to bind into their call
        // hashes; tests of the real executable validator are gated below.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let server_config_sha256 =
            super::sha256_json(&frozen_server_config).expect("opaque test server config hash");
        let frozen_server_config_sha256 = server_config_sha256.clone();
        let input_schema = json!({
            "type": "object",
            "required": ["query"],
            "properties": {"query": {"type": "string"}}
        });
        let tool_schema_sha256 = sha256_json_value(&input_schema).expect("tool schema hash");
        let call_sha256 = persona_tool_call_sha256(PersonaToolCallIdentity {
            invocation_id: "invocation",
            host_conversation_id: "host",
            user_message_id: "turn",
            target_id: "persona",
            persona_version: 7,
            snapshot_sha256: "snapshot",
            server: "local",
            tool: "lookup",
            arguments_sha256: &arguments_sha256,
            server_config_sha256: &server_config_sha256,
            frozen_server_config_sha256: &frozen_server_config_sha256,
            tool_schema_sha256: &tool_schema_sha256,
            model_config_sha256: &model_config_sha256,
            model_fingerprint: &fingerprint,
        })
        .expect("call hash");
        let approval = MentionToolApproval {
            id: "approval".to_string(),
            invocation_id: "invocation".to_string(),
            host_conversation_id: "host".to_string(),
            user_message_id: "turn".to_string(),
            target_id: "persona".to_string(),
            handle: "researcher".to_string(),
            label: "Researcher".to_string(),
            persona_version: 7,
            snapshot_sha256: "snapshot".to_string(),
            server: "local".to_string(),
            tool: "lookup".to_string(),
            arguments,
            arguments_sha256,
            server_config_sha256,
            tool_schema_sha256,
            model_config_sha256,
            call_sha256,
            effect_receipt_id: None,
            effect_outcome: None,
            created_at_ms: 10,
            expires_at_ms,
            consumed_at_ms: None,
            decision: None,
            state: MentionToolApprovalState::Pending,
        };
        StoredMentionInvocation {
            invocation: MentionInvocation {
                id: "invocation".to_string(),
                host_conversation_id: "host".to_string(),
                user_message_id: "turn".to_string(),
                addressed_message: "question".to_string(),
                host_context: Vec::new(),
                targets: vec![MentionTargetSnapshot {
                    target_id: "persona".to_string(),
                    kind: MentionTargetKind::Persona,
                    handle: "researcher".to_string(),
                    label: "Researcher".to_string(),
                    version: 7,
                    source_leaf_message_id: None,
                    profile: ConversationExecutionProfile::default(),
                    source_messages: Vec::new(),
                    snapshot_sha256: "snapshot".to_string(),
                }],
                results: Vec::new(),
                tool_approvals: vec![approval],
                synthesis_message_id: None,
                synthesis_sha256: None,
                state: MentionInvocationState::AwaitingApproval,
                created_at: "10".to_string(),
                updated_at: "10".to_string(),
            },
            frozen_tool_continuations: vec![FrozenMentionToolContinuation {
                approval_id: "approval".to_string(),
                model_path: PathBuf::from("model.gguf"),
                mmproj_path: None,
                model_config: Some(model_config),
                model_fingerprint: fingerprint,
                chat_template: ChatTemplateChoice::ModelDefault,
                sampling: SamplingConfig::default(),
                messages: Vec::new(),
                provisional_output: GenerationOutput {
                    request_id: "invocation".to_string(),
                    branch_id: "persona".to_string(),
                    input_index: 0,
                    model_id: "model".to_string(),
                    text: "provisional".to_string(),
                    generated_token_ids: Vec::new(),
                    token_observations: None,
                    state: GenerationState::Completed,
                    finish_reason: "stop".to_string(),
                    metrics: GenerationMetrics::default(),
                    real_engine_invoked: true,
                    fake_fixture: false,
                    transport: NativeTransport::InProcess,
                },
                input_schema,
                mcp_server_config: Some(frozen_server_config),
                mcp_server_config_sha256: frozen_server_config_sha256,
                deadline_clock: None,
                resume_lease_id: None,
            }],
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn unsupported_platform_blocks_approval_before_store_or_lease_authority() {
        let direct = super::mention_tool_approval_decide(
            "missing-invocation",
            "missing-approval",
            MentionToolApprovalDecision::Approve,
        )
        .expect("typed unsupported approval result");

        let data_dir = std::env::temp_dir().join(format!(
            "mom-persona-unsupported-decision-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&data_dir).expect("temporary unsupported decision directory");
        let _remove_data_dir = RemoveTestDir(data_dir.clone());
        let recovery = PersonaToolApprovalRecovery::bind(&data_dir)
            .expect("bind unsupported-platform recovery authority");
        let scoped = super::mention_tool_approval_decide_with_recovery_in_scope(
            &crate::operation_scope::OperationScope::detached(),
            "missing-invocation",
            "missing-approval",
            MentionToolApprovalDecision::Approve,
            &recovery,
        )
        .expect("typed scoped unsupported approval result");

        for result in [&direct, &scoped] {
            assert_eq!(result.status, "blocked");
            assert_eq!(result.readiness, "blocked_platform_unsupported");
            assert_eq!(
                result.blocker.as_ref().map(|blocker| blocker.code.as_str()),
                Some("mcp_platform_unsupported")
            );
            assert!(result.result.is_none());
            assert_eq!(result.receipt.readiness, result.readiness);
        }
        assert!(
            !data_dir.join("operation-leases").exists(),
            "an unsupported approval command must not create lease authority"
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn unsupported_platform_recovery_terminalizes_without_lease_io() -> anyhow::Result<()> {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-persona-unsupported-recovery-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&data_dir)?;
        let _remove_data_dir = RemoveTestDir(data_dir.clone());
        let lease_io_trap = data_dir.join("operation-leases");
        std::fs::write(&lease_io_trap, b"unsupported-platform-no-lease-io")?;
        let store = RuntimeStore::open(&data_dir)?;
        let mut stored = frozen_approval_record(u128::from(u64::MAX));

        let mut resuming_approval = stored.tool_approvals[0].clone();
        resuming_approval.id = "resuming-approval".to_string();
        resuming_approval.state = MentionToolApprovalState::Resuming;
        resuming_approval.decision = Some(MentionToolApprovalDecision::Approve);
        resuming_approval.consumed_at_ms = Some(20);
        stored.tool_approvals.push(resuming_approval);
        let mut resuming_continuation = stored.frozen_tool_continuations[0].clone();
        resuming_continuation.approval_id = "resuming-approval".to_string();
        resuming_continuation.resume_lease_id = Some("unsupported-platform-lease".to_string());
        stored.frozen_tool_continuations.push(resuming_continuation);

        let invocation = stored.invocation.clone();
        let continuations = stored.frozen_tool_continuations.clone();
        store.mutate_documents(
            INVOCATIONS_NAMESPACE,
            MentionInvocationDb::default,
            |db, documents| {
                upsert_invocation_with_continuations(db, &invocation, continuations)?;
                write_active_approval_index(db, documents)
            },
        )?;
        drop(store);

        let recovery = PersonaToolApprovalRecovery::bind(&data_dir)?;
        recovery.reconcile()?;
        // A second sweep proves the terminal projection cannot reacquire or
        // retry the interrupted external effect.
        recovery.reconcile()?;
        drop(recovery);
        assert_eq!(
            std::fs::read(&lease_io_trap)?,
            b"unsupported-platform-no-lease-io",
            "unsupported recovery must not probe or mutate process-lease storage"
        );

        let reopened = RuntimeStore::open(&data_dir)?;
        let terminal_db = reopened
            .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
            .expect("terminalized invocation database");
        let terminal = terminal_db
            .invocations
            .iter()
            .find(|candidate| candidate.id == "invocation")
            .expect("terminalized invocation");
        let expired = terminal
            .tool_approvals
            .iter()
            .find(|approval| approval.id == "approval")
            .expect("expired pending approval");
        assert_eq!(expired.state, MentionToolApprovalState::Expired);
        assert!(expired.consumed_at_ms.is_some());
        assert_eq!(expired.effect_receipt_id, None);
        assert_eq!(expired.effect_outcome, None);

        let interrupted = terminal
            .tool_approvals
            .iter()
            .find(|approval| approval.id == "resuming-approval")
            .expect("terminalized resuming approval");
        assert_eq!(interrupted.state, MentionToolApprovalState::Failed);
        assert_eq!(
            interrupted.effect_outcome,
            Some(MentionToolEffectOutcome::Unknown)
        );
        assert!(interrupted.effect_receipt_id.is_some());
        assert!(terminal.frozen_tool_continuations.is_empty());
        assert_eq!(terminal.state, MentionInvocationState::Failed);
        assert!(
            reopened
                .get::<super::ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
                .expect("terminal active approval index")
                .approvals
                .is_empty(),
            "terminal unsupported-platform approvals must never be retried"
        );
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn persona_tool_approval_is_exact_one_use_and_decision_bound() {
        let mut stored = frozen_approval_record(1_000);
        let claimed = claim_stored_persona_tool_approval(
            &mut stored,
            "approval",
            MentionToolApprovalDecision::Approve,
            100,
            "lease-1",
            false,
        )
        .expect("claim validation")
        .expect("pending exact approval");
        assert_eq!(claimed.approval.state, MentionToolApprovalState::Resuming);
        assert_eq!(
            claimed.approval.decision,
            Some(MentionToolApprovalDecision::Approve)
        );
        assert_eq!(claimed.approval.consumed_at_ms, Some(100));
        assert_eq!(claimed.snapshot.version, 7);

        let blocker = claim_stored_persona_tool_approval(
            &mut stored,
            "approval",
            MentionToolApprovalDecision::Deny,
            101,
            "lease-2",
            false,
        )
        .expect("second claim validation")
        .expect_err("one-use approval must not be claimed twice");
        assert_eq!(blocker.code, "mention_tool_approval_consumed");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn persona_tool_approval_rejects_tampering_and_expires_monotonically() {
        let mut tampered = frozen_approval_record(1_000);
        tampered.tool_approvals[0].arguments = json!({"query": "changed"});
        let blocker = claim_stored_persona_tool_approval(
            &mut tampered,
            "approval",
            MentionToolApprovalDecision::Approve,
            100,
            "lease-1",
            false,
        )
        .expect("tamper validation")
        .expect_err("changed arguments must invalidate the exact call hash");
        assert_eq!(blocker.code, "mention_tool_approval_mismatch");
        assert_eq!(
            tampered.tool_approvals[0].state,
            MentionToolApprovalState::Pending
        );

        let mut expired = frozen_approval_record(99);
        let blocker = claim_stored_persona_tool_approval(
            &mut expired,
            "approval",
            MentionToolApprovalDecision::Approve,
            100,
            "lease-1",
            false,
        )
        .expect("expiry validation")
        .expect_err("expired approval must fail closed");
        assert_eq!(blocker.code, "mention_tool_approval_expired");
        assert_eq!(
            expired.tool_approvals[0].state,
            MentionToolApprovalState::Pending
        );

        let mut monotonic_expired = frozen_approval_record(1_000);
        let blocker = claim_stored_persona_tool_approval(
            &mut monotonic_expired,
            "approval",
            MentionToolApprovalDecision::Approve,
            100,
            "lease-1",
            true,
        )
        .expect("monotonic expiry validation")
        .expect_err("elapsed monotonic deadline must fail closed despite wall rollback");
        assert_eq!(blocker.code, "mention_tool_approval_expired");
        assert_eq!(
            monotonic_expired.tool_approvals[0].state,
            MentionToolApprovalState::Pending
        );
    }

    #[test]
    fn active_approval_index_is_bounded_derived_state_for_pending_and_resuming_only() {
        let mut stored = frozen_approval_record(1_000);
        let mut db = super::MentionInvocationDb {
            invocations: vec![stored.clone()],
        };
        let pending = active_approval_index(&db).expect("pending active index");
        assert_eq!(pending.approvals.len(), 1);
        assert_eq!(
            pending.approvals[0].state,
            MentionToolApprovalState::Pending
        );
        assert_eq!(pending.approvals[0].resume_lease_id, None);

        stored.tool_approvals[0].state = MentionToolApprovalState::Resuming;
        stored.frozen_tool_continuations[0].resume_lease_id = Some("lease-1".to_string());
        db.invocations[0] = stored;
        let resuming = active_approval_index(&db).expect("resuming active index");
        assert_eq!(resuming.approvals.len(), 1);
        assert_eq!(
            resuming.approvals[0].state,
            MentionToolApprovalState::Resuming
        );
        assert_eq!(
            resuming.approvals[0].resume_lease_id.as_deref(),
            Some("lease-1")
        );

        db.invocations[0].tool_approvals[0].state = MentionToolApprovalState::Failed;
        db.invocations[0].frozen_tool_continuations.clear();
        assert!(
            active_approval_index(&db)
                .expect("terminal active index")
                .approvals
                .is_empty()
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn persona_approval_persists_one_crash_terminal_and_never_retries_after_relaunch()
    -> anyhow::Result<()> {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-persona-approval-relaunch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&data_dir)?;
        let _remove_data_dir = RemoveTestDir(data_dir.clone());
        let store = RuntimeStore::open(&data_dir)?;
        let frozen = frozen_approval_record(1_000);
        let invocation = frozen.invocation.clone();
        let continuations = frozen.frozen_tool_continuations.clone();

        // The initial invocation and its derived active index are one fact. A
        // failure after staging the projection must expose neither document.
        let rolled_back_create: anyhow::Result<()> = store.mutate_documents(
            INVOCATIONS_NAMESPACE,
            MentionInvocationDb::default,
            |db, documents| {
                upsert_invocation_with_continuations(db, &invocation, continuations.clone())?;
                write_active_approval_index(db, documents)?;
                Err(anyhow::anyhow!(
                    "inject failure after pending approval index projection"
                ))
            },
        );
        assert!(rolled_back_create.is_err());
        assert!(
            store
                .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
                .is_none()
        );
        assert!(
            store
                .get::<super::ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
                .is_none()
        );

        store.mutate_documents(
            INVOCATIONS_NAMESPACE,
            MentionInvocationDb::default,
            |db, documents| {
                upsert_invocation_with_continuations(db, &invocation, continuations.clone())?;
                write_active_approval_index(db, documents)
            },
        )?;
        let pending_db = store
            .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
            .expect("pending invocation document");
        let pending_index = store
            .get::<super::ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
            .expect("pending active index");
        assert_eq!(pending_db.invocations.len(), 1);
        assert_eq!(pending_index.approvals.len(), 1);
        assert_eq!(
            pending_db.invocations[0].tool_approvals[0].state,
            MentionToolApprovalState::Pending
        );
        assert_eq!(
            pending_index.approvals[0].approval_id,
            pending_db.invocations[0].tool_approvals[0].id
        );
        assert_eq!(
            pending_index.approvals[0].state,
            MentionToolApprovalState::Pending
        );

        // Keep the host turn but deliberately omit every current Persona
        // template. The exact claim must be derived from the frozen snapshot,
        // so library removal cannot retarget or invalidate already-frozen work.
        store.put(
            CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![Conversation {
                    id: "host".to_string(),
                    title: "Host".to_string(),
                    created_at: "1".to_string(),
                    updated_at: "1".to_string(),
                    kind: ConversationKind::Chat,
                    execution_profile: ConversationExecutionProfile::default(),
                    selected_model_path: None,
                    source_conversation_id: None,
                    source_message_id: None,
                    branch_root_message_id: None,
                    active_leaf_message_id: Some("turn".to_string()),
                    current_skill_ids: Vec::new(),
                    messages: vec![Message {
                        id: "turn".to_string(),
                        conversation_id: "host".to_string(),
                        role: MessageRole::User,
                        content: "question".to_string(),
                        created_at: "1".to_string(),
                        parent_id: None,
                        model: None,
                        receipt_id: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        reasoning_content: None,
                        reasoning_incomplete: false,
                        branch_index: None,
                        branch_count: None,
                        attribution: None,
                        attachment_ids: Vec::new(),
                    }],
                }],
                selected_conversation_id: Some("host".to_string()),
            },
        )?;

        // A claim and the Resuming index transition also share one rollback
        // boundary. No receipt intent may leak from a failed transaction.
        let rolled_back_claim: anyhow::Result<()> = store.mutate_documents(
            INVOCATIONS_NAMESPACE,
            MentionInvocationDb::default,
            |db, documents| {
                let stored = db
                    .invocations
                    .iter_mut()
                    .find(|stored| stored.id == "invocation")
                    .expect("persisted invocation");
                claim_stored_persona_tool_approval(
                    stored,
                    "approval",
                    MentionToolApprovalDecision::Approve,
                    100,
                    "crashed-lease",
                    false,
                )?
                .expect("exact frozen approval claim");
                write_active_approval_index(db, documents)?;
                Err(anyhow::anyhow!(
                    "inject failure after resuming approval index projection"
                ))
            },
        );
        assert!(rolled_back_claim.is_err());
        let after_claim_rollback = store
            .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
            .expect("rolled-back invocation");
        assert_eq!(
            after_claim_rollback.invocations[0].tool_approvals[0].state,
            MentionToolApprovalState::Pending
        );
        assert!(
            after_claim_rollback.invocations[0].tool_approvals[0]
                .effect_receipt_id
                .is_none()
        );
        assert_eq!(
            store
                .get::<super::ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
                .expect("rolled-back active index")
                .approvals[0]
                .state,
            MentionToolApprovalState::Pending
        );

        let live_lease = PersonaToolResumeLease::acquire(&data_dir, "invocation", "approval")?
            .expect("simulated process lease acquisition");
        let crashed_lease_id = live_lease.id().to_string();
        let claimed = store.mutate_documents(
            INVOCATIONS_NAMESPACE,
            MentionInvocationDb::default,
            |db, documents| {
                let stored = db
                    .invocations
                    .iter_mut()
                    .find(|stored| stored.id == "invocation")
                    .expect("persisted invocation");
                let claimed = claim_stored_persona_tool_approval(
                    stored,
                    "approval",
                    MentionToolApprovalDecision::Approve,
                    100,
                    &crashed_lease_id,
                    false,
                )?
                .expect("exact frozen approval claim");
                write_active_approval_index(db, documents)?;
                Ok(claimed)
            },
        )?;
        assert_eq!(claimed.snapshot.version, 7);
        let call_sha256 = claimed.approval.call_sha256.clone();
        let receipt_intent = claimed
            .approval
            .effect_receipt_id
            .clone()
            .expect("approved claim receipt intent");
        assert!(receipt_intent.starts_with("mom_llama.persona_tool_effect:"));
        assert!(!receipt_intent.contains(&claimed.approval.id));
        assert!(!receipt_intent.contains(&call_sha256));
        let resuming_db = store
            .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
            .expect("resuming invocation");
        let resuming_index = store
            .get::<super::ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
            .expect("resuming active index");
        assert_eq!(
            resuming_db.invocations[0].tool_approvals[0]
                .effect_receipt_id
                .as_deref(),
            Some(receipt_intent.as_str())
        );
        assert_eq!(
            resuming_index.approvals[0].state,
            MentionToolApprovalState::Resuming
        );
        assert_eq!(
            resuming_index.approvals[0].resume_lease_id.as_deref(),
            Some(crashed_lease_id.as_str())
        );
        assert!(
            persona_tool_resume_lease_is_live(
                &data_dir,
                "invocation",
                "approval",
                &crashed_lease_id,
            )?,
            "the simulated process owns the exact live generation before crashing"
        );
        drop(live_lease);
        assert!(
            super::persona_tool_resume_lease_path(&data_dir, "invocation", "approval").is_file(),
            "lease identity keeps its stable inode after the process owner disappears"
        );
        assert!(
            !persona_tool_resume_lease_is_live(
                &data_dir,
                "invocation",
                "approval",
                &crashed_lease_id,
            )?,
            "the relaunched process must observe the persisted claim without a live owner"
        );

        // Drop every in-process owner. A fresh recovery authority sees the
        // durable Resuming state but no live lease, and must emit exactly one
        // outcome-unknown terminal without dispatching the tool again.
        drop(store);
        let relaunched = PersonaToolApprovalRecovery::bind(&data_dir)?;
        relaunched.reconcile()?;
        drop(relaunched);
        let reopened = RuntimeStore::open(&data_dir)?;
        let terminal_db = reopened
            .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
            .expect("terminal invocation");
        let terminal = &terminal_db.invocations[0];
        let terminal_approval = &terminal.tool_approvals[0];
        assert_eq!(terminal_approval.state, MentionToolApprovalState::Failed);
        assert_eq!(
            terminal_approval.effect_outcome,
            Some(MentionToolEffectOutcome::Unknown)
        );
        assert_eq!(
            terminal_approval.effect_receipt_id.as_deref(),
            Some(receipt_intent.as_str())
        );
        assert_eq!(terminal.results.len(), 1);
        assert_eq!(
            terminal.results[0].tool_receipt_ids,
            vec![receipt_intent.clone()]
        );
        assert!(terminal.frozen_tool_continuations.is_empty());
        assert_eq!(terminal.state, MentionInvocationState::Failed);
        assert!(
            reopened
                .get::<super::ActivePersonaToolApprovalIndex>(ACTIVE_APPROVALS_NAMESPACE)?
                .expect("terminal active index")
                .approvals
                .is_empty()
        );

        let receipt_connection = rusqlite::Connection::open(reopened.path())?;
        let (receipt_command, receipt_ciphertext): (String, Vec<u8>) = receipt_connection
            .query_row(
                "SELECT command_id, ciphertext FROM receipts WHERE receipt_id = ?1",
                [receipt_intent.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        assert_eq!(receipt_command, "mom_llama.mcp_call_tool");
        assert!(
            !receipt_ciphertext
                .windows(call_sha256.len())
                .any(|window| window == call_sha256.as_bytes()),
            "exact call identity must remain encrypted at rest"
        );
        let receipt_count = |connection: &rusqlite::Connection| -> anyhow::Result<i64> {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM receipts WHERE receipt_id = ?1",
                [receipt_intent.as_str()],
                |row| row.get(0),
            )?)
        };
        assert_eq!(receipt_count(&receipt_connection)?, 1);
        drop(receipt_connection);

        let terminal_conversations = reopened
            .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
            .expect("terminal conversation projection");
        assert!(
            terminal_conversations
                .conversations
                .iter()
                .all(|conversation| conversation.kind != ConversationKind::PersonaTemplate)
        );
        assert_eq!(
            terminal_conversations.conversations[0].messages.len(),
            1,
            "an outcome-unknown failure stays in the approval surface"
        );

        drop(reopened);
        let second_relaunch = PersonaToolApprovalRecovery::bind(&data_dir)?;
        second_relaunch.reconcile()?;
        drop(second_relaunch);
        let idempotent_store = RuntimeStore::open(&data_dir)?;
        assert_eq!(
            idempotent_store
                .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
                .expect("idempotent terminal invocation"),
            terminal_db
        );
        assert_eq!(
            idempotent_store
                .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
                .expect("idempotent terminal conversation"),
            terminal_conversations
        );
        let idempotent_connection = rusqlite::Connection::open(idempotent_store.path())?;
        assert_eq!(receipt_count(&idempotent_connection)?, 1);
        drop(idempotent_connection);

        let listed = project_visible_tool_approvals(terminal_db.clone(), "host", u128::MAX);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, MentionToolApprovalState::Failed);
        assert_eq!(
            listed[0].effect_outcome,
            Some(MentionToolEffectOutcome::Unknown)
        );
        assert_eq!(
            listed[0].effect_receipt_id.as_deref(),
            Some(receipt_intent.as_str())
        );
        Ok(())
    }

    #[test]
    fn expiry_and_stale_resume_reconcile_to_one_terminal_without_retry() {
        let mut expired = frozen_approval_record(100);
        let expired_outcome = reconcile_stored_persona_tool_approvals(
            &mut expired,
            100,
            BTreeSet::new(),
            BTreeSet::new(),
        )
        .expect("expiry reconciliation");
        assert!(expired_outcome.changed);
        assert!(expired_outcome.effect_receipts.is_empty());
        finish_stored_mention_invocation(&mut expired, &mut ConversationDb::default());
        assert_eq!(
            expired.tool_approvals[0].state,
            MentionToolApprovalState::Expired
        );
        assert_eq!(expired.tool_approvals[0].consumed_at_ms, Some(100));
        assert_eq!(expired.results.len(), 1);
        assert_eq!(expired.results[0].state, GenerationState::Failed);
        assert!(expired.frozen_tool_continuations.is_empty());
        assert_eq!(expired.state, MentionInvocationState::Failed);

        let mut interrupted = frozen_approval_record(1_000);
        interrupted.tool_approvals[0].state = MentionToolApprovalState::Resuming;
        interrupted.tool_approvals[0].decision = Some(MentionToolApprovalDecision::Approve);
        interrupted.frozen_tool_continuations[0].resume_lease_id = Some("stale-lease".to_string());
        let stale_resumes = BTreeSet::from(["approval".to_string()]);
        let interrupted_outcome = reconcile_stored_persona_tool_approvals(
            &mut interrupted,
            200,
            BTreeSet::new(),
            stale_resumes,
        )
        .expect("interrupted reconciliation");
        assert!(interrupted_outcome.changed);
        assert_eq!(interrupted_outcome.effect_receipts.len(), 1);
        assert_eq!(
            interrupted.tool_approvals[0].state,
            MentionToolApprovalState::Failed
        );
        assert!(
            interrupted.results[0]
                .text
                .contains("will not execute it again")
        );
        assert_eq!(
            interrupted.results[0].tool_receipt_ids,
            vec![
                interrupted_outcome.effect_receipts[0]
                    .receipt
                    .task_id
                    .clone()
            ]
        );
        assert!(interrupted.frozen_tool_continuations.is_empty());
    }

    #[test]
    fn unknown_effect_receipt_marks_spawn_boundary_and_identity_limits() {
        let stored = frozen_approval_record(1_000);
        let approval = &stored.tool_approvals[0];
        let receipt = unknown_persona_tool_effect_receipt(
            approval,
            "managed MCP pathname identity drifted across process spawn",
        );
        assert_eq!(
            receipt
                .blocker
                .as_ref()
                .expect("unknown outcome blocker")
                .code,
            "mention_tool_effect_outcome_unknown"
        );
        for artifact in [
            "external_process_spawned:yes",
            "tool_effect_dispatch:uncertain",
            "atomic_path_to_exec_identity:not_asserted",
            "dynamic_dependency_identity:not_asserted",
            "external_effect_outcome:unknown",
        ] {
            assert!(
                receipt
                    .receipt
                    .artifacts_produced
                    .iter()
                    .any(|value| value == artifact),
                "unknown-effect receipt omitted {artifact}"
            );
        }
    }

    #[test]
    fn approval_deadline_uses_the_earlier_monotonic_or_durable_wall_boundary() {
        let tracker = ApprovalDeadlineTracker::default();
        let clock = super::PersonaToolDeadlineClock {
            monotonic_created_ms: 100_000,
            boot_wall_anchor_ms: 900_000,
        };
        tracker
            .observe(
                "approval",
                1_000_000,
                1_300_000,
                Some(clock),
                1_000_000,
                100_000,
            )
            .expect("initial deadline");
        assert!(
            !tracker
                .due("approval", 399_999)
                .expect("deadline observation")
        );
        assert!(
            tracker
                .due("approval", 400_000)
                .expect("deadline observation")
        );

        // Four elapsed minutes followed by a three-minute wall rollback still
        // leaves wall time after creation. The boot anchor exposes that
        // discontinuity and fails closed instead of granting three more
        // minutes.
        let relaunched = ApprovalDeadlineTracker::default();
        relaunched
            .observe(
                "approval",
                1_000_000,
                1_300_000,
                Some(clock),
                1_060_000,
                340_000,
            )
            .expect("same-boot rollback observation");
        assert!(
            relaunched
                .due("approval", 340_000)
                .expect("same-boot rollback fails closed")
        );

        // A second observation can only shorten the deadline already bound in
        // this process.
        tracker
            .observe(
                "approval",
                1_000_000,
                1_300_000,
                Some(clock),
                999_999,
                100_001,
            )
            .expect("rollback observation");
        assert!(
            tracker
                .due("approval", 100_001)
                .expect("rollback fails closed")
        );

        // Legacy approvals without a durable monotonic clock are not granted
        // a fresh five-minute window during migration.
        let legacy = ApprovalDeadlineTracker::default();
        legacy
            .observe("approval", 1_000_000, 1_300_000, None, 1_000_001, 500_000)
            .expect("legacy deadline observation");
        assert!(
            legacy
                .due("approval", 500_000)
                .expect("legacy approval fails closed")
        );
    }

    #[test]
    fn forced_monotonic_expiry_terminalizes_even_when_wall_time_moves_backward() {
        let mut stored = frozen_approval_record(1_000);
        let outcome = reconcile_stored_persona_tool_approvals(
            &mut stored,
            500,
            BTreeSet::from(["approval".to_string()]),
            BTreeSet::new(),
        )
        .expect("forced expiry reconciliation");
        assert!(outcome.changed);
        assert!(outcome.effect_receipts.is_empty());
        assert_eq!(
            stored.tool_approvals[0].state,
            MentionToolApprovalState::Expired
        );
    }

    #[test]
    fn live_resume_is_not_reconciled_out_from_under_the_running_command() {
        let mut stored = frozen_approval_record(1_000);
        stored.tool_approvals[0].state = MentionToolApprovalState::Resuming;
        stored.frozen_tool_continuations[0].resume_lease_id = Some("live-lease".to_string());
        let outcome = reconcile_stored_persona_tool_approvals(
            &mut stored,
            200,
            BTreeSet::new(),
            BTreeSet::new(),
        )
        .expect("live-lease reconciliation");
        assert!(!outcome.changed);
        assert!(outcome.effect_receipts.is_empty());
        assert_eq!(
            stored.tool_approvals[0].state,
            MentionToolApprovalState::Resuming
        );
        assert!(stored.results.is_empty());
        assert_eq!(stored.frozen_tool_continuations.len(), 1);
    }

    #[test]
    fn public_approval_resolution_never_serializes_the_frozen_continuation() {
        let stored = frozen_approval_record(1_000);
        let resolution = MentionToolApprovalResolution {
            approval: stored.tool_approvals[0].clone(),
            invocation_id: stored.id.clone(),
            invocation_state: stored.state,
            remaining_approvals: stored.tool_approvals.clone(),
        };
        let encoded = serde_json::to_string(&resolution).expect("public approval result");
        assert!(!encoded.contains("_frozen_tool_continuations"));
        assert!(!encoded.contains("model.gguf"));
        assert!(!encoded.contains("provisional"));
    }

    #[test]
    fn cancellation_lifecycle_removes_only_the_exact_registered_target_and_generation() {
        let first_key = ("invocation".to_string(), "first".to_string());
        let second_key = ("invocation".to_string(), "second".to_string());
        let first = Arc::new(MentionCancelControl::running(false));
        let replacement = Arc::new(MentionCancelControl::running(false));
        let second = Arc::new(MentionCancelControl::running(false));
        let mut registry = BTreeMap::from([
            (first_key.clone(), Arc::clone(&replacement)),
            (second_key.clone(), Arc::clone(&second)),
        ]);
        unregister_exact_mentions(&mut registry, &[(first_key.clone(), first)]);
        assert!(Arc::ptr_eq(
            registry.get(&first_key).expect("replacement remains"),
            &replacement
        ));
        unregister_exact_mentions(&mut registry, &[(second_key.clone(), Arc::clone(&second))]);
        assert!(!registry.contains_key(&second_key));
        assert!(registry.contains_key(&first_key));
    }

    #[test]
    fn cancellation_terminal_arbitration_is_monotonic() {
        let cancelled = MentionCancelControl::running(false);
        assert!(cancelled.request_cancel());
        assert!(cancelled.arbitrate_terminal());
        assert!(!cancelled.request_cancel());

        let completed = MentionCancelControl::running(false);
        assert!(!completed.arbitrate_terminal());
        assert!(!completed.request_cancel());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn persona_resume_lease_keeps_a_stable_inode_and_binds_its_generation() {
        let data_dir =
            std::env::temp_dir().join(format!("mom-persona-resume-lease-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("temporary lease directory");
        let lease_path =
            super::persona_tool_resume_lease_path(&data_dir, "invocation", "approval-a");
        let first = PersonaToolResumeLease::acquire(&data_dir, "invocation", "approval-a")
            .expect("first lease acquisition")
            .expect("first lease must be free");
        assert!(lease_path.is_file());
        assert!(
            persona_tool_resume_lease_is_live(&data_dir, "invocation", "approval-a", first.id(),)
                .expect("live lease probe")
        );
        assert!(
            !persona_tool_resume_lease_is_live(
                &data_dir,
                "invocation",
                "approval-a",
                "different-generation",
            )
            .expect("mismatched lease probe")
        );
        assert!(
            PersonaToolResumeLease::acquire(&data_dir, "invocation", "approval-b")
                .expect("contending lease acquisition")
                .is_err(),
            "one invocation serializes sibling approval resumes"
        );
        let first_id = first.id().to_string();
        drop(first);
        assert!(lease_path.is_file(), "lease inode must never be unlinked");
        assert!(
            !persona_tool_resume_lease_is_live(&data_dir, "invocation", "approval-a", &first_id,)
                .expect("released lease probe")
        );
        let second = PersonaToolResumeLease::acquire(&data_dir, "invocation", "approval-b")
            .expect("second lease acquisition")
            .expect("released lease must be reusable");
        assert_ne!(second.id(), first_id);
        drop(second);
        std::fs::remove_dir_all(&data_dir).expect("temporary lease cleanup");
    }

    #[test]
    fn persona_tool_call_hash_binds_turn_snapshot_arguments_and_model() {
        let fingerprint = model_fingerprint("model");
        let hash = |turn: &str,
                    snapshot: &str,
                    arguments: &str,
                    server_config: &str,
                    tool_schema: &str,
                    model_config: &str,
                    model: &ModelFingerprint| {
            persona_tool_call_sha256(PersonaToolCallIdentity {
                invocation_id: "invocation",
                host_conversation_id: "host",
                user_message_id: turn,
                target_id: "persona",
                persona_version: 7,
                snapshot_sha256: snapshot,
                server: "local",
                tool: "lookup",
                arguments_sha256: arguments,
                server_config_sha256: server_config,
                frozen_server_config_sha256: server_config,
                tool_schema_sha256: tool_schema,
                model_config_sha256: model_config,
                model_fingerprint: model,
            })
            .expect("call hash")
        };
        let baseline = hash(
            "turn",
            "snapshot",
            "arguments",
            "server-config",
            "tool-schema",
            "model-config",
            &fingerprint,
        );
        assert_ne!(
            baseline,
            hash(
                "other-turn",
                "snapshot",
                "arguments",
                "server-config",
                "tool-schema",
                "model-config",
                &fingerprint,
            )
        );
        assert_ne!(
            baseline,
            hash(
                "turn",
                "other-snapshot",
                "arguments",
                "server-config",
                "tool-schema",
                "model-config",
                &fingerprint,
            )
        );
        assert_ne!(
            baseline,
            hash(
                "turn",
                "snapshot",
                "other-arguments",
                "server-config",
                "tool-schema",
                "model-config",
                &fingerprint,
            )
        );
        assert_ne!(
            baseline,
            hash(
                "turn",
                "snapshot",
                "arguments",
                "other-server-config",
                "tool-schema",
                "model-config",
                &fingerprint,
            )
        );
        assert_ne!(
            baseline,
            hash(
                "turn",
                "snapshot",
                "arguments",
                "server-config",
                "other-tool-schema",
                "model-config",
                &fingerprint,
            )
        );
        assert_ne!(
            baseline,
            hash(
                "turn",
                "snapshot",
                "arguments",
                "server-config",
                "tool-schema",
                "other-model-config",
                &fingerprint,
            )
        );
        assert_ne!(
            baseline,
            hash(
                "turn",
                "snapshot",
                "arguments",
                "server-config",
                "tool-schema",
                "model-config",
                &model_fingerprint("other-model")
            )
        );
    }

    #[test]
    fn legacy_mention_invocation_records_migrate_with_no_continuation_authority() {
        let stored = frozen_approval_record(1_000);
        let mut legacy = serde_json::to_value(&stored).expect("legacy invocation JSON");
        let object = legacy.as_object_mut().expect("invocation object");
        object.remove("tool_approvals");
        object.remove("_frozen_tool_continuations");
        let migrated: StoredMentionInvocation =
            serde_json::from_value(legacy).expect("legacy invocation migration");
        assert_eq!(migrated.id, stored.id);
        assert_eq!(migrated.targets, stored.targets);
        assert!(migrated.tool_approvals.is_empty());
        assert!(migrated.frozen_tool_continuations.is_empty());
    }

    #[test]
    fn mention_parser_is_stable_and_deduplicates_handles() {
        assert_eq!(
            parse_handles("Ask @evidence-lens and @whole-person, then @evidence-lens."),
            vec!["evidence-lens", "whole-person"]
        );
    }

    #[test]
    fn mention_parser_requires_the_explicit_composer_boundary() {
        assert_eq!(
            parse_handles("@leading then\t@after-tab and\n@after-newline"),
            vec!["leading", "after-tab", "after-newline"]
        );
        assert!(parse_handles("mail@example.com prefix@embedded (@parenthesized)").is_empty());
    }

    #[test]
    fn mention_parser_ignores_markdown_code() {
        assert!(
            parse_handles(
                "`@inline` and ``@wide-inline``\n```text\n@fenced\n```\n~~~\n@tilde-fenced\n~~~\n    @indented"
            )
            .is_empty()
        );
    }

    #[test]
    fn duplicate_case_insensitive_registry_handles_are_ambiguous_not_last_wins() {
        let conversations = vec![
            mention_conversation("first", "same-lens"),
            mention_conversation("second", "SAME-LENS"),
        ];
        let resolution =
            resolve_targets_from_registry(&["same-lens".to_string()], "host", &conversations, &[]);
        assert!(resolution.targets.is_empty());
        assert!(resolution.unresolved.is_empty());
        assert_eq!(resolution.ambiguous, vec!["same-lens"]);
        assert_eq!(
            ambiguous_resolution_blocker(&resolution)
                .expect("ambiguous registry must produce a blocker")
                .code,
            "mention_target_ambiguous"
        );
    }

    #[test]
    fn mention_tool_calls_must_match_an_attached_allowlisted_binding_and_schema() {
        let tool = |policy| BoundMentionTool {
            server: "local".to_string(),
            server_config: McpServerConfig {
                name: "local".to_string(),
                command: std::env::current_exe().expect("test executable path"),
                args: Vec::new(),
                enabled: true,
            },
            policy,
            server_config_sha256: "server-config".to_string(),
            frozen_server_config_sha256: "frozen-server-config".to_string(),
            tool_schema_sha256: "tool-schema".to_string(),
            contract: McpTool {
                name: "lookup".to_string(),
                description: None,
                input_schema: json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {"query": {"type": "string"}}
                }),
            },
        };
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::AlwaysAllow)],
                "local",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .expect_err("Persona AlwaysAllow must fail closed to exact approval")
            .code,
            "mention_tool_approval_required"
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::Ask)],
                "local",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .expect_err("ask policy must require explicit approval")
            .code,
            "mention_tool_approval_required"
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::Deny)],
                "local",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .expect_err("deny policy must reject the tool call")
            .code,
            "tool_permission_denied"
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::AlwaysAllow)],
                "other",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .expect_err("an unattached tool binding must be rejected")
            .code,
            "mention_tool_not_attached"
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::AlwaysAllow)],
                "local",
                "lookup",
                &json!({}),
                "researcher",
            )
            .expect_err("missing required tool arguments must be rejected")
            .code,
            "tool_loop_required_argument_missing"
        );
    }

    #[test]
    fn persona_tool_decision_requires_one_complete_strict_control_object() {
        assert!(
            serde_json::from_str::<PersonaToolDecision>("Please call local/lookup now").is_err()
        );
        assert_eq!(
            serde_json::from_str::<PersonaToolDecision>(
                r#"{"action":"call","server":"local","tool":"lookup","arguments":{"query":"x"}}"#
            )
            .expect("complete call decision"),
            PersonaToolDecision::Call {
                server: "local".to_string(),
                tool: "lookup".to_string(),
                arguments: json!({"query": "x"}),
            }
        );
        assert_eq!(
            serde_json::from_str::<PersonaToolDecision>(r#"{"action":"final"}"#)
                .expect("complete final decision"),
            PersonaToolDecision::Final {}
        );
        let call =
            r#"{"action":"call","server":"local","tool":"lookup","arguments":{"query":"x"}}"#;
        let prose = format!("Here is an example, not a request: {call} Please explain it.");
        let fenced = format!("A configuration example:\n```json\n{call}\n```");

        assert!(
            serde_json::from_str::<PersonaToolDecision>(&prose).is_err(),
            "answer prose must never be scanned for an embedded control object"
        );
        assert!(
            serde_json::from_str::<PersonaToolDecision>(&fenced).is_err(),
            "fenced display JSON must remain inert answer prose"
        );
        assert!(
            serde_json::from_str::<PersonaToolDecision>(
                r#"{"action":"final","unexpected":"field"}"#
            )
            .is_err(),
            "unknown fields must invalidate the complete decision"
        );
    }

    #[test]
    fn persona_tool_decision_schema_binds_each_call_to_one_attached_contract() {
        let tools = vec![BoundMentionTool {
            server: "local".to_string(),
            server_config: McpServerConfig {
                name: "local".to_string(),
                command: std::env::current_exe().expect("test executable path"),
                args: Vec::new(),
                enabled: true,
            },
            policy: ToolPermissionPolicy::AlwaysAllow,
            server_config_sha256: "server-config".to_string(),
            frozen_server_config_sha256: "frozen-server-config".to_string(),
            tool_schema_sha256: "tool-schema".to_string(),
            contract: McpTool {
                name: "lookup".to_string(),
                description: Some("Look up one exact query".to_string()),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["query"],
                    "properties": {"query": {"type": "string"}}
                }),
            },
        }];
        let schema = persona_tool_decision_schema(&tools);
        let variants = schema["oneOf"].as_array().expect("oneOf variants");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0]["properties"]["action"]["const"], "final");
        assert_eq!(variants[1]["properties"]["action"]["const"], "call");
        assert_eq!(variants[1]["properties"]["server"]["const"], "local");
        assert_eq!(variants[1]["properties"]["tool"]["const"], "lookup");
        assert_eq!(
            variants[1]["properties"]["arguments"],
            tools[0].contract.input_schema
        );
        assert_eq!(variants[1]["additionalProperties"], false);
        assert!(mention_tool_instructions(&tools).contains("Draft the answer as ordinary prose"));
        assert!(!mention_tool_instructions(&tools).contains("return only"));
        assert_eq!(mention_tool_manifest(&tools)[0]["permission"], "ask");
    }

    #[test]
    fn persona_tool_contracts_are_count_and_byte_bounded_before_prompt_construction() {
        let tool = |index: usize, payload_bytes: usize| BoundMentionTool {
            server: format!("server-{index}"),
            server_config: McpServerConfig {
                name: format!("server-{index}"),
                command: std::env::current_exe().expect("test executable path"),
                args: Vec::new(),
                enabled: true,
            },
            policy: ToolPermissionPolicy::Ask,
            server_config_sha256: "server-config".to_string(),
            frozen_server_config_sha256: "frozen-server-config".to_string(),
            tool_schema_sha256: "tool-schema".to_string(),
            contract: McpTool {
                name: format!("tool-{index}"),
                description: None,
                input_schema: json!({
                    "type": "object",
                    "description": "x".repeat(payload_bytes),
                }),
            },
        };

        let too_many = (0..=crate::personas::MAX_PERSONA_TOOL_BINDINGS)
            .map(|index| tool(index, 0))
            .collect::<Vec<_>>();
        assert_eq!(
            validate_resolved_mention_tools(&too_many)
                .expect_err("over-limit tool count must fail")
                .code,
            "persona_tool_binding_limit_exceeded"
        );

        let oversized = vec![tool(0, MAX_PERSONA_TOOL_INPUT_SCHEMA_BYTES)];
        assert_eq!(
            validate_resolved_mention_tools(&oversized)
                .expect_err("oversized input schema must fail")
                .code,
            "mention_tool_schema_too_large"
        );

        let aggregate = (0..3)
            .map(|index| tool(index, MAX_PERSONA_TOOL_MANIFEST_BYTES / 3))
            .collect::<Vec<_>>();
        assert_eq!(
            validate_resolved_mention_tools(&aggregate)
                .expect_err("oversized aggregate manifest must fail")
                .code,
            "mention_tool_manifest_too_large"
        );
    }

    #[test]
    fn context_trimming_removes_whole_messages_and_never_drops_the_addressed_message() {
        let boundary = handoff_message(ChatRole::System, "invitation-boundary");
        let final_message = handoff_message(ChatRole::User, "mandatory-addressed-message");
        let messages = vec![
            handoff_message(ChatRole::System, "persona-system"),
            handoff_message(ChatRole::User, "older-source"),
            handoff_message(ChatRole::Assistant, "newer-source"),
            boundary.clone(),
            handoff_message(ChatRole::User, "older-host"),
            handoff_message(ChatRole::Assistant, "newer-host"),
            final_message.clone(),
        ];
        let fitted = fit_handoff_to_context(messages.clone(), &boundary, 2, 5, |candidate| {
            Ok(candidate.len())
        })
        .expect("the mandatory handoff should fit after whole-message trimming");
        assert_eq!(
            fitted.messages,
            vec![messages[0].clone(), boundary.clone(), final_message.clone()],
            "host context is trimmed before source context and no message is split"
        );
        assert_eq!(fitted.messages.last(), Some(&final_message));

        let blocker = fit_handoff_to_context(
            vec![messages[0].clone(), boundary.clone(), final_message.clone()],
            &boundary,
            2,
            4,
            |candidate| Ok(candidate.len()),
        )
        .expect_err("mandatory system, boundary, addressed message, and reserve cannot be trimmed");
        assert_eq!(blocker.code, "mention_context_too_large");
    }
}
