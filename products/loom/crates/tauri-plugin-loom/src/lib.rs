#![forbid(unsafe_code)]

mod model_download;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::io::Read as _;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use loom_backend_llama::{
    ContinuationCase, DownloadCancellation, DownloadControl, DownloadError,
    ExactContinuationRequest, ExactContinuationResult, GgufDownloadRequest, GgufHeaderStatus,
    JoinedLlamaGeneration, JoinedLlamaRuntime, LlamaBackend, LlamaBackendError,
    LlamaGenerationControl, LlamaGenerationHandle, LocalModelProfile, MAX_MODEL_DOWNLOAD_BYTES,
    ModelDiscoveryOptions, ModelRelease, NativeHostRuntime, ProcessExitJoinedLlamaRuntime,
    SamplerKind, SamplingConfig, Sha256Digest, VerifiedModelDescriptor, discover_gguf_models,
    download_gguf, model_environment_from_verified, validate_candidate_receipt_binding,
    validate_gguf_download_request,
};
use loom_document::{DocumentContent, MergeError, MergeOutcome, three_way_merge};
use loom_host::{
    AgencyGate, BranchCancellation, DEFAULT_MAX_ACTIVE_GENERATION_BRANCHES,
    ForegroundCommandBinding, ForegroundCommandChallenge, ForegroundCommandRegistry,
    ForegroundWindowId, GenerationFamilyIdentity, GenerationRegistry, GenerationRegistryError,
    MAX_PENDING_FOREGROUND_COMMANDS, NativeWindowFocusSample,
};
use loom_research_types::{
    MixedAuthorshipAssemblyRecord, PromotionCommandRequest, PromotionSubject,
};
use loom_store::{
    AdmittedCandidateProjection, BranchPageCursor, DocumentReconciliationSnapshot,
    ExternalReconciliationOutcome, ExternalReconciliationRequest, IdempotentSaveOutcome,
    LoadedDocument, MAX_BRANCH_BODY_BYTES, MixedAuthorshipAdmission, ProjectStore,
    PromotionSubjectLease, RecordedPromotionRequest, StoredBranchBody, StoredBranchRecord,
    StoredBranchStatus, StoredBranchSummary, TerminalCandidateInput, TerminalEvidenceInput,
    TerminalGenerationInput, TransientDraft, VisibleProjectionState,
};
use loom_types::{
    AuthorityPolicy, BlobId, BranchId, BuildModelPolicy, BuildModelPolicyIdentity,
    BuildWriterProfileId, ByteRange, CancelGenerationCommand, CandidateId, CommandId,
    CommandReceipt, ContextRecipe, DocumentId, DocumentKind, GenerationEventKind, GenerationRunId,
    GenerationStart, GenerationTerminalStatus, LoomEvent, ModelEnvironment, ModelRole, ProjectId,
    PromptMode, PromptRecipe, RevisionId, SelectionDecision, derive_weave_case_ids, now_unix_ms,
};
use same_file::Handle as FileIdentityHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;

use crate::model_download::{
    ModelDownloadRegistry, ModelDownloadRegistryError, ModelDownloadSnapshot, ModelDownloadSpec,
    ModelLibraryError, ReservationOutcome, model_target_path, prepare_model_library,
};

const INITIAL_DOCUMENT: &str = "manuscript/Untitled.md";
const DEFAULT_PROJECT_DIRECTORY: &str = "writing";
const PROJECT_CLOSE_GENERATION_WAIT: Duration = Duration::from_secs(3);
const MAX_MODEL_DOWNLOAD_URL_BYTES: usize = 16 * 1024;
const POLICY_MODEL_HASH_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_TRACKED_GENERATION_WORKERS: usize = DEFAULT_MAX_ACTIVE_GENERATION_BRANCHES;
const FOREGROUND_PROMOTION_TTL: Duration = Duration::from_secs(30);
const MAX_RESEARCH_PROMOTION_PREVIEW_BYTES: usize = 64 * 1024;
const MAX_RESEARCH_PROMOTION_PACKET_BYTES: usize = 256 * 1024;
const RESEARCH_PROMOTION_PACKET_SCHEMA: &str = "loom.research-promotion-packet.v1";
pub const APPLICATION_QUIT_MENU_ID: &str = "loom.application.quit";

#[derive(Debug)]
pub enum PendingPromotionSubject {
    CandidateProjection(AdmittedCandidateProjection),
    MixedAuthorship(MixedAuthorshipAdmission),
}

impl PendingPromotionSubject {
    fn lease(&self) -> PromotionSubjectLease<'_> {
        match self {
            Self::CandidateProjection(value) => PromotionSubjectLease::CandidateProjection(value),
            Self::MixedAuthorship(value) => PromotionSubjectLease::MixedAuthorship(value),
        }
    }
}

#[derive(Debug)]
struct PendingResearchPromotion {
    project_id: ProjectId,
    session_id: CommandId,
    request: PromotionCommandRequest,
    recorded_request: RecordedPromotionRequest,
    subject: PendingPromotionSubject,
    challenge: ForegroundCommandChallenge,
    result_text: String,
}

#[derive(Debug, Default)]
struct PendingResearchPromotions {
    by_command: BTreeMap<CommandId, PendingResearchPromotion>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ApplicationPhase {
    #[default]
    Running,
    Closing,
    ExitAuthorized,
}

struct ApplicationCloseAttempt<'a> {
    state: &'a PluginState,
    phase: MutexGuard<'a, ApplicationPhase>,
    authorized: bool,
}

#[derive(Debug)]
struct ApplicationShutdownProof {
    native_runtime: ApplicationNativeShutdown,
    desktop_workers: DesktopWorkersJoined,
}

#[derive(Debug)]
enum ApplicationNativeShutdown {
    Graceful(JoinedLlamaRuntime),
    ProcessExit(ProcessExitJoinedLlamaRuntime),
}

#[derive(Debug)]
struct ReadyToExit {
    proof: ApplicationShutdownProof,
}

#[derive(Debug)]
struct WorkerRegistryIdentity;

#[derive(Debug)]
struct DesktopWorkersJoined {
    model_loads: ModelLoadsDrained,
    generation_workers: GenerationWorkersJoined,
    download_workers: DownloadWorkersJoined,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SessionPhase {
    #[default]
    Closed,
    Choosing,
    Open,
}

#[derive(Debug, Default)]
struct Session {
    phase: SessionPhase,
    store: Option<ProjectStore>,
    active_session_id: Option<CommandId>,
    agency: AgencyGate,
    last_close: Option<ProjectCloseReceipt>,
}

const AUTOMATIC_FAMILY_BUDGET_PER_REVISION_V2: u8 = 2;
const MAX_TRACKED_AUTOMATIC_REVISION_BUDGETS_V2: usize = 16_384;
const AUTOMATIC_TOKEN_BUDGET_PER_REVISION_V2: u32 = AUTOMATIC_WEAVE_BRANCH_COUNT_V2
    * AUTOMATIC_WEAVE_MAX_TOKENS_V2
    * AUTOMATIC_FAMILY_BUDGET_PER_REVISION_V2 as u32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AutomaticBudgetScope {
    project: ProjectId,
    session: CommandId,
    document: DocumentId,
    source_revision: RevisionId,
}

#[derive(Debug, Default)]
struct AutomaticBudgetLedger {
    active_session: Option<(ProjectId, CommandId)>,
    families_by_scope: BTreeMap<AutomaticBudgetScope, u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutomaticBudgetError {
    Capacity,
    Exhausted,
    Poisoned,
}

#[derive(Debug, Default)]
struct AutomaticBudgetAuthority {
    ledger: Mutex<AutomaticBudgetLedger>,
}

#[derive(Debug)]
struct AutomaticBudgetReservation<'authority> {
    authority: &'authority AutomaticBudgetAuthority,
    scope: AutomaticBudgetScope,
    committed: bool,
}

impl AutomaticBudgetAuthority {
    fn reserve(
        &self,
        _writer: &PolicyBoundAutomaticWriter,
        scope: AutomaticBudgetScope,
    ) -> Result<AutomaticBudgetReservation<'_>, AutomaticBudgetError> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| AutomaticBudgetError::Poisoned)?;
        let session = (scope.project, scope.session);
        if ledger.active_session != Some(session) {
            ledger.active_session = Some(session);
            ledger.families_by_scope.clear();
        }
        // Only the current immutable revision of each document can request
        // automatic work. Dropping superseded revisions keeps the ledger
        // bounded by the number of project documents, not edit count.
        ledger.families_by_scope.retain(|candidate, _| {
            candidate.document != scope.document
                || candidate.source_revision == scope.source_revision
        });
        if !ledger.families_by_scope.contains_key(&scope)
            && ledger.families_by_scope.len() >= MAX_TRACKED_AUTOMATIC_REVISION_BUDGETS_V2
        {
            return Err(AutomaticBudgetError::Capacity);
        }
        let spent = ledger.families_by_scope.entry(scope).or_default();
        if *spent >= AUTOMATIC_FAMILY_BUDGET_PER_REVISION_V2 {
            return Err(AutomaticBudgetError::Exhausted);
        }
        *spent += 1;
        drop(ledger);
        Ok(AutomaticBudgetReservation {
            authority: self,
            scope,
            committed: false,
        })
    }

    fn refund(&self, scope: AutomaticBudgetScope) {
        let Ok(mut ledger) = self.ledger.lock() else {
            // Poisoning fails closed: never mint replacement authority when
            // the exact prior reservation state cannot be proven.
            return;
        };
        let Some(spent) = ledger.families_by_scope.get_mut(&scope) else {
            return;
        };
        *spent = spent.saturating_sub(1);
        if *spent == 0 {
            ledger.families_by_scope.remove(&scope);
        }
    }
}

impl AutomaticBudgetReservation<'_> {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for AutomaticBudgetReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.authority.refund(self.scope);
        }
    }
}

#[derive(Debug)]
pub struct PluginState {
    close_requested: AtomicBool,
    exit_authorized: AtomicBool,
    application: Mutex<ApplicationPhase>,
    session: Mutex<Session>,
    native_runtime: Arc<NativeHostRuntime>,
    backend: Arc<LlamaBackend>,
    model: Mutex<ModelRegistry>,
    model_lifecycle: Mutex<()>,
    user_model_paths: Mutex<BTreeSet<PathBuf>>,
    automatic_budget: AutomaticBudgetAuthority,
    foreground_commands: ForegroundCommandRegistry,
    research_promotions: Mutex<PendingResearchPromotions>,
    generations: GenerationRegistry,
    generation_workers: GenerationWorkerRegistry,
    model_loads: Arc<ModelLoadRegistry>,
    downloads: Arc<ModelDownloadRegistry>,
    download_workers: DownloadWorkerRegistry,
    model_library_root: Option<PathBuf>,
    build_model_policy: BuildModelPolicy,
}

impl Default for PluginState {
    fn default() -> Self {
        Self::with_model_library_root(None, BuildModelPolicy::default())
    }
}

impl PluginState {
    fn with_model_library_root(
        model_library_root: Option<PathBuf>,
        build_model_policy: BuildModelPolicy,
    ) -> Self {
        let native_runtime = Arc::new(NativeHostRuntime::default());
        let backend = Arc::new(LlamaBackend::with_default_native_runtime(Arc::clone(
            &native_runtime,
        )));
        Self {
            close_requested: AtomicBool::new(false),
            exit_authorized: AtomicBool::new(false),
            application: Mutex::new(ApplicationPhase::default()),
            session: Mutex::new(Session::default()),
            native_runtime,
            backend,
            model: Mutex::new(ModelRegistry::default()),
            model_lifecycle: Mutex::new(()),
            user_model_paths: Mutex::new(BTreeSet::new()),
            automatic_budget: AutomaticBudgetAuthority::default(),
            foreground_commands: ForegroundCommandRegistry::default(),
            research_promotions: Mutex::new(PendingResearchPromotions::default()),
            generations: GenerationRegistry::default(),
            generation_workers: GenerationWorkerRegistry::default(),
            model_loads: Arc::new(ModelLoadRegistry::default()),
            downloads: Arc::new(ModelDownloadRegistry::default()),
            download_workers: DownloadWorkerRegistry::default(),
            model_library_root,
            build_model_policy,
        }
    }

    fn stage_research_promotion(
        &self,
        project_id: ProjectId,
        session_id: CommandId,
        window_label: &str,
        subject: PendingPromotionSubject,
        request: PromotionCommandRequest,
    ) -> Result<ResearchPromotionPrompt, IpcFailure> {
        let mut session = lock_session(self)?;
        let store = require_bound_store(
            &mut session,
            &project_id.to_string(),
            &session_id.to_string(),
        )?;
        let mut pending = self.research_promotions.lock().map_err(|_| {
            IpcFailure::new(
                "research_promotion_state_unavailable",
                "the pending research-promotion registry is unavailable",
                false,
            )
        })?;
        let now = now_unix_ms();
        pending.by_command.retain(|_, value| {
            value.challenge.expires_at_unix_ms >= now
                && value.project_id == project_id
                && value.session_id == session_id
        });
        if pending.by_command.len() >= MAX_PENDING_FOREGROUND_COMMANDS {
            return Err(IpcFailure::new(
                "research_promotion_capacity",
                "too many research promotions are awaiting a foreground decision",
                true,
            ));
        }
        if pending.by_command.contains_key(&request.command_id()) {
            return Err(IpcFailure::new(
                "research_promotion_duplicate",
                "this research promotion is already awaiting a decision",
                false,
            ));
        }
        let binding_parts = store
            .foreground_promotion_binding_parts(&request)
            .map_err(IpcFailure::store)?;
        let result_bytes = store
            .read_blob(request.intended_result_blob_id())
            .map_err(IpcFailure::store)?;
        if result_bytes.len() > MAX_RESEARCH_PROMOTION_PREVIEW_BYTES {
            return Err(IpcFailure::new(
                "research_promotion_preview_too_large",
                "the research result exceeds the bounded foreground review surface",
                false,
            ));
        }
        let result_text = String::from_utf8(result_bytes).map_err(|_| {
            IpcFailure::new(
                "research_promotion_preview_not_utf8",
                "the research result is not valid manuscript UTF-8",
                false,
            )
        })?;
        let recorded_request = store
            .record_promotion_command_request(subject.lease(), &request)
            .map_err(IpcFailure::store)?;
        let binding = ForegroundCommandBinding {
            application_session_id: session_id,
            window_id: ForegroundWindowId::new(window_label).map_err(|error| {
                IpcFailure::new("invalid_foreground_window", error.to_string(), false)
            })?,
            document_id: binding_parts.document_id,
            candidate_fingerprint: binding_parts.candidate_fingerprint,
            command_id: request.command_id(),
            promotion_fingerprint: request.command_request_fingerprint(),
        };
        let challenge = self
            .foreground_commands
            .issue(binding, FOREGROUND_PROMOTION_TTL)
            .map_err(|error| {
                IpcFailure::new("foreground_command_rejected", error.to_string(), false)
            })?;
        let prompt = ResearchPromotionPrompt::from_parts(&request, &challenge, &result_text);
        pending.by_command.insert(
            request.command_id(),
            PendingResearchPromotion {
                project_id,
                session_id,
                request,
                recorded_request,
                subject,
                challenge,
                result_text,
            },
        );
        Ok(prompt)
    }
}

impl Drop for PluginState {
    fn drop(&mut self) {
        self.close_requested.store(true, Ordering::Release);
        self.exit_authorized.store(false, Ordering::Release);
        let _ = self.foreground_commands.revoke_all();
        if let Ok(pending) = self.research_promotions.get_mut() {
            pending.by_command.clear();
        }
        if let Ok(phase) = self.application.get_mut() {
            *phase = ApplicationPhase::Closing;
        }
        let _desktop_workers = self.join_desktop_workers_for_exit();
        let _native_runtime = self.native_runtime.shutdown_for_process_exit();
    }
}

#[derive(Clone, Debug)]
struct LoadedModel {
    profile: LocalModelProfile,
    descriptor: VerifiedModelDescriptor,
}

mod automatic_writer_authority {
    use super::*;

    /// A resident model whose exact bytes and native capabilities were bound
    /// to one writer entry in the closed build policy.
    ///
    /// The fields and constructor stay private to this module. Production code
    /// can obtain the witness only through `AuthorizedWeaveModel::bind`.
    #[derive(Debug)]
    pub(super) struct PolicyBoundAutomaticWriter {
        loaded: LoadedModel,
        profile_id: BuildWriterProfileId,
        rank: u32,
        policy_identity: BuildModelPolicyIdentity,
    }

    #[derive(Debug)]
    enum AuthorizedWeaveModelKind {
        Automatic(PolicyBoundAutomaticWriter),
        Manual(LoadedModel),
    }

    #[derive(Debug)]
    enum SubmittedWeaveAuthority {
        Automatic {
            profile_id: BuildWriterProfileId,
            rank: u32,
            policy_identity: BuildModelPolicyIdentity,
        },
        Manual,
    }

    /// An exact request whose model authority was preserved through request
    /// construction. Its payload is intentionally not exposed to the plugin;
    /// submission consumes this wrapper and is the only escape hatch.
    #[derive(Debug)]
    pub(super) struct AuthorizedWeaveRequest {
        request: ExactContinuationRequest,
        authority: SubmittedWeaveAuthority,
    }

    /// The only model authority accepted by the shared Weave submission path.
    /// Manual requests retain their explicit escape hatch; automatic requests
    /// necessarily carry a non-forgeable policy witness until request creation.
    #[derive(Debug)]
    pub(super) struct AuthorizedWeaveModel {
        policy: ValidatedWeavePolicy,
        kind: AuthorizedWeaveModelKind,
    }

    impl PolicyBoundAutomaticWriter {
        fn bind(loaded: LoadedModel, policy: &BuildModelPolicy) -> Result<Self, IpcFailure> {
            let matched = policy
                .matching_writer(
                    &loaded.descriptor.model_sha256,
                    loaded.descriptor.model_file_bytes,
                )
                .ok_or_else(|| {
                    IpcFailure::new(
                        "automatic_writer_not_in_build_policy",
                        "automatic suggestions require the exact local writer selected by this Loom build",
                        false,
                    )
                })?;
            let writer = matched.writer();
            let expectation = PolicyWriterExpectation {
                profile_id: writer.profile_id().to_owned(),
                rank: matched.rank(),
                role: writer.role(),
                prompt_mode: writer.prompt_mode(),
                model_sha256: writer.model_sha256(),
                model_file_bytes: writer.model_file_bytes(),
            };
            validate_policy_model_descriptor(
                &loaded.descriptor,
                &loaded.profile.model_path,
                &expectation,
            )?;
            Ok(Self {
                loaded,
                profile_id: writer.typed_profile_id(),
                rank: matched.rank(),
                policy_identity: policy.identity(),
            })
        }

        fn into_request_parts(self) -> (LocalModelProfile, SubmittedWeaveAuthority) {
            let Self {
                loaded,
                profile_id,
                rank,
                policy_identity,
            } = self;
            (
                loaded.profile,
                SubmittedWeaveAuthority::Automatic {
                    profile_id,
                    rank,
                    policy_identity,
                },
            )
        }
    }

    impl AuthorizedWeaveRequest {
        pub(super) fn submit(
            self,
            backend: &LlamaBackend,
        ) -> Result<LlamaGenerationHandle, LlamaBackendError> {
            let Self { request, authority } = self;
            match authority {
                SubmittedWeaveAuthority::Automatic {
                    profile_id: _profile_id,
                    rank: _rank,
                    policy_identity: _policy_identity,
                } => {}
                SubmittedWeaveAuthority::Manual => {}
            }
            backend.start_exact_continuation(request)
        }
    }

    impl AuthorizedWeaveModel {
        pub(super) fn bind(
            policy: ValidatedWeavePolicy,
            loaded: LoadedModel,
            build_policy: &BuildModelPolicy,
        ) -> Result<Self, IpcFailure> {
            let kind = match &policy {
                ValidatedWeavePolicy::AutomaticV2 => AuthorizedWeaveModelKind::Automatic(
                    PolicyBoundAutomaticWriter::bind(loaded, build_policy)?,
                ),
                ValidatedWeavePolicy::ManualV2 { .. } => AuthorizedWeaveModelKind::Manual(loaded),
            };
            Ok(Self { policy, kind })
        }

        pub(super) fn branch_count(&self) -> u32 {
            self.policy.branch_count()
        }

        pub(super) fn bind_document_kind(
            &self,
            kind: DocumentKind,
        ) -> Result<ResolvedWeavePolicy, IpcFailure> {
            self.policy.bind_document_kind(kind)
        }

        pub(super) fn admit(&self, gate: &AgencyGate) -> Result<(), IpcFailure> {
            let admission = match &self.kind {
                AuthorizedWeaveModelKind::Automatic(_) => gate.admit_automation(),
                AuthorizedWeaveModelKind::Manual(_) => gate.admit_manual_generation(),
            };
            admission
                .map_err(|error| IpcFailure::new("generation_blocked", error.to_string(), false))
        }

        pub(super) fn loaded(&self) -> &LoadedModel {
            match &self.kind {
                AuthorizedWeaveModelKind::Automatic(writer) => &writer.loaded,
                AuthorizedWeaveModelKind::Manual(loaded) => loaded,
            }
        }

        pub(super) fn automatic_writer(&self) -> Option<&PolicyBoundAutomaticWriter> {
            match &self.kind {
                AuthorizedWeaveModelKind::Automatic(writer) => Some(writer),
                AuthorizedWeaveModelKind::Manual(_) => None,
            }
        }

        pub(super) fn into_exact_continuation_request(
            self,
            request_id: String,
            exact_manuscript_prefix: String,
            prompt_recipe: PromptRecipe,
            cases: Vec<ContinuationCase>,
        ) -> AuthorizedWeaveRequest {
            let Self { policy: _, kind } = self;
            let (model, authority) = match kind {
                AuthorizedWeaveModelKind::Automatic(writer) => writer.into_request_parts(),
                AuthorizedWeaveModelKind::Manual(loaded) => {
                    (loaded.profile, SubmittedWeaveAuthority::Manual)
                }
            };
            AuthorizedWeaveRequest {
                request: ExactContinuationRequest {
                    request_id,
                    model,
                    exact_manuscript_prefix,
                    prompt_recipe,
                    cases,
                },
                authority,
            }
        }

        #[cfg(test)]
        pub(super) fn automatic_binding(
            &self,
        ) -> Option<(BuildWriterProfileId, u32, BuildModelPolicyIdentity)> {
            match &self.kind {
                AuthorizedWeaveModelKind::Automatic(writer) => {
                    Some((writer.profile_id, writer.rank, writer.policy_identity))
                }
                AuthorizedWeaveModelKind::Manual(_) => None,
            }
        }
    }
}

use automatic_writer_authority::{AuthorizedWeaveModel, PolicyBoundAutomaticWriter};

#[derive(Clone, Debug)]
struct GenerationResultBinding {
    exact_prompt_blob_id: BlobId,
    model_environment: ModelEnvironment,
    model: VerifiedModelDescriptor,
    generations: BTreeMap<GenerationRunId, GenerationStart>,
}

#[derive(Clone, Debug, Default)]
enum ModelRegistry {
    #[default]
    Empty,
    Loading {
        path: PathBuf,
        previous: Option<Box<LoadedModel>>,
    },
    Unloading(Box<LoadedModel>),
    ResidencyUnknown {
        reason: String,
    },
    Loaded(Box<LoadedModel>),
}

#[derive(Debug)]
enum GenerationWorkerSlot {
    Reserved,
    Running {
        worker: JoinHandle<()>,
        owner: GenerationWorkerOwner,
    },
}

/// Product builds have exactly one owner kind. The test-only fixture variant
/// exercises hostile cancellation and nested-worker schedules without adding
/// an erasable production authority surface.
#[derive(Debug)]
enum GenerationWorkerOwner {
    Llama(LlamaGenerationHandle),
    #[cfg(test)]
    Fixture(FixtureGenerationWorkerOwner),
}

#[cfg(test)]
trait FixtureGenerationWorkerCancellation: std::fmt::Debug + Send + Sync {
    fn cancel_all(&self);
}

#[cfg(test)]
#[derive(Debug)]
struct FixtureGenerationWorkerOwner {
    cancellation: Arc<dyn FixtureGenerationWorkerCancellation>,
    backend_worker: Option<JoinHandle<()>>,
}

#[derive(Debug)]
enum GenerationBackendWorkerJoined {
    Llama(JoinedLlamaGeneration),
    #[cfg(test)]
    Fixture {
        worker_was_present: bool,
        worker_panicked: bool,
    },
}

#[derive(Debug, Default)]
struct GenerationWorkerState {
    workers: BTreeMap<String, GenerationWorkerSlot>,
    join_failure: Option<String>,
}

#[derive(Debug)]
struct GenerationWorkerRegistry {
    identity: Arc<WorkerRegistryIdentity>,
    state: Mutex<GenerationWorkerState>,
}

#[derive(Debug)]
struct ModelLoadRegistry {
    identity: Arc<WorkerRegistryIdentity>,
    state: Mutex<ModelLoadState>,
    drained: Condvar,
}

#[derive(Debug)]
struct ModelLoadState {
    accepting: bool,
    active: usize,
}

#[derive(Debug)]
struct ModelLoadPermit {
    lifetime: Arc<ModelLoadLifetime>,
}

#[derive(Debug)]
struct ModelLoadWorkerGuard {
    _lifetime: Arc<ModelLoadLifetime>,
}

#[derive(Debug)]
struct ModelLoadLifetime {
    registry: Arc<ModelLoadRegistry>,
}

#[derive(Debug)]
struct ModelLoadsDrained {
    registry_identity: Arc<WorkerRegistryIdentity>,
    count: usize,
}

#[derive(Debug)]
struct GenerationWorkersJoined {
    registry_identity: Arc<WorkerRegistryIdentity>,
    family_count: usize,
    backend_workers: Vec<GenerationBackendWorkerJoined>,
}

#[derive(Debug)]
enum DownloadWorkerSlot {
    Reserved,
    Running {
        worker: JoinHandle<()>,
        cancellation: DownloadCancellation,
    },
}

#[derive(Debug, Default)]
struct DownloadWorkerState {
    workers: BTreeMap<CommandId, DownloadWorkerSlot>,
    join_failure: Option<String>,
}

#[derive(Debug)]
struct DownloadWorkerRegistry {
    identity: Arc<WorkerRegistryIdentity>,
    state: Mutex<DownloadWorkerState>,
}

#[derive(Debug)]
struct DownloadWorkerReservation<'registry, 'admission> {
    registry: &'registry DownloadWorkerRegistry,
    command_id: CommandId,
    attached: bool,
    _admission: PhantomData<&'admission ApplicationPhase>,
}

#[derive(Debug)]
struct DownloadWorkerAttachError {
    failure: IpcFailure,
    worker: JoinHandle<()>,
}

#[derive(Debug)]
struct DownloadWorkersJoined {
    registry_identity: Arc<WorkerRegistryIdentity>,
    count: usize,
}

#[derive(Debug)]
struct GenerationWorkerReservation<'registry, 'admission> {
    registry: &'registry GenerationWorkerRegistry,
    request_id: String,
    attached: bool,
    _admission: PhantomData<&'admission ApplicationPhase>,
}

#[derive(Debug)]
struct GenerationWorkerAttachError {
    failure: IpcFailure,
    worker: JoinHandle<()>,
    owner: GenerationWorkerOwner,
}

impl Default for ModelLoadRegistry {
    fn default() -> Self {
        Self {
            identity: Arc::new(WorkerRegistryIdentity),
            state: Mutex::new(ModelLoadState {
                accepting: true,
                active: 0,
            }),
            drained: Condvar::new(),
        }
    }
}

impl ModelLoadRegistry {
    fn reserve(
        self: &Arc<Self>,
        _admission: &MutexGuard<'_, ApplicationPhase>,
    ) -> Result<ModelLoadPermit, IpcFailure> {
        let mut state = self.state.lock().map_err(|_| {
            IpcFailure::new(
                "model_load_worker_state_poisoned",
                "the local model loader entered an invalid state; restart Loom",
                false,
            )
        })?;
        if !state.accepting {
            return Err(IpcFailure::new(
                "application_quiescing",
                "Loom will not start local model verification while the application is closing",
                true,
            ));
        }
        state.active = state.active.checked_add(1).ok_or_else(|| {
            IpcFailure::new(
                "model_load_worker_capacity",
                "the local model loader count overflowed",
                false,
            )
        })?;
        Ok(ModelLoadPermit {
            lifetime: Arc::new(ModelLoadLifetime {
                registry: Arc::clone(self),
            }),
        })
    }

    /// Permanently closes model-load admission and waits until both every
    /// admitted async command and its blocking worker have released their
    /// shared lifetime, including registry commit or rollback.
    fn close_and_drain(&self) -> ModelLoadsDrained {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        let count = state.active;
        while state.active != 0 {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        ModelLoadsDrained {
            registry_identity: Arc::clone(&self.identity),
            count,
        }
    }

    fn reopen_after_aborted_close(&self) -> Result<(), IpcFailure> {
        let mut state = self.state.lock().map_err(|_| {
            IpcFailure::new(
                "model_load_worker_state_poisoned",
                "the local model loader entered an invalid state; restart Loom",
                false,
            )
        })?;
        if state.active != 0 {
            return Err(IpcFailure::new(
                "model_load_worker_active",
                "the local model loader cannot reopen while verification is active",
                true,
            ));
        }
        state.accepting = true;
        Ok(())
    }
}

impl ModelLoadPermit {
    fn worker_guard(&self) -> ModelLoadWorkerGuard {
        ModelLoadWorkerGuard {
            _lifetime: Arc::clone(&self.lifetime),
        }
    }
}

impl Drop for ModelLoadLifetime {
    fn drop(&mut self) {
        let mut state = self
            .registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state
            .active
            .checked_sub(1)
            .expect("a model-load permit can be released only once");
        if state.active == 0 {
            self.registry.drained.notify_all();
        }
    }
}

impl ModelLoadsDrained {
    fn belongs_to(&self, registry: &ModelLoadRegistry) -> bool {
        Arc::ptr_eq(&self.registry_identity, &registry.identity)
    }

    const fn count(&self) -> usize {
        self.count
    }
}

impl GenerationWorkerOwner {
    fn cancel_all(&self) {
        match self {
            Self::Llama(owner) => {
                let _ = owner.cancel_all();
            }
            #[cfg(test)]
            Self::Fixture(owner) => owner.cancellation.cancel_all(),
        }
    }

    /// Consumes the sole backend-worker owner and returns only after its exact
    /// nested worker has been joined. Cancellation panics in hostile fixtures
    /// are contained before the join; product owners use the infallible,
    /// per-case-catch native path.
    fn shutdown_joined(self) -> GenerationBackendWorkerJoined {
        match self {
            Self::Llama(owner) => GenerationBackendWorkerJoined::Llama(owner.shutdown_joined()),
            #[cfg(test)]
            Self::Fixture(mut owner) => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    owner.cancellation.cancel_all();
                }));
                let backend_worker = owner.backend_worker.take();
                GenerationBackendWorkerJoined::Fixture {
                    worker_was_present: backend_worker.is_some(),
                    worker_panicked: backend_worker
                        .is_some_and(|backend_worker| backend_worker.join().is_err()),
                }
            }
        }
    }

    #[cfg(test)]
    fn fixture(
        cancellation: Arc<dyn FixtureGenerationWorkerCancellation>,
        backend_worker: JoinHandle<()>,
    ) -> Self {
        Self::Fixture(FixtureGenerationWorkerOwner {
            cancellation,
            backend_worker: Some(backend_worker),
        })
    }
}

impl GenerationBackendWorkerJoined {
    const fn worker_panicked(&self) -> bool {
        match self {
            Self::Llama(joined) => joined.worker_panicked(),
            #[cfg(test)]
            Self::Fixture {
                worker_panicked, ..
            } => *worker_panicked,
        }
    }

    const fn joined_worker_count(&self) -> usize {
        match self {
            Self::Llama(joined) => joined.joined_worker_count(),
            #[cfg(test)]
            Self::Fixture {
                worker_was_present, ..
            } => *worker_was_present as usize,
        }
    }
}

impl GenerationWorkerRegistry {
    fn reserve<'registry, 'admission>(
        &'registry self,
        request_id: &str,
        _admission: &'admission MutexGuard<'_, ApplicationPhase>,
    ) -> Result<GenerationWorkerReservation<'registry, 'admission>, IpcFailure> {
        self.reap_finished()?;
        let mut state = self.lock()?;
        if let Some(failure) = &state.join_failure {
            return Err(IpcFailure::new(
                "generation_worker_join_failed",
                failure.clone(),
                false,
            ));
        }
        if state.workers.len() >= MAX_TRACKED_GENERATION_WORKERS {
            return Err(IpcFailure::new(
                "generation_worker_capacity",
                format!(
                    "Loom already owns the maximum of {MAX_TRACKED_GENERATION_WORKERS} generation workers"
                ),
                true,
            ));
        }
        if state.workers.contains_key(request_id) {
            return Err(IpcFailure::new(
                "generation_worker_duplicate",
                "the generation request already owns a desktop worker",
                false,
            ));
        }
        state
            .workers
            .insert(request_id.to_owned(), GenerationWorkerSlot::Reserved);
        Ok(GenerationWorkerReservation {
            registry: self,
            request_id: request_id.to_owned(),
            attached: false,
            _admission: PhantomData,
        })
    }

    fn reap_finished(&self) -> Result<usize, IpcFailure> {
        let finished = {
            let mut state = self.lock()?;
            if let Some(failure) = &state.join_failure {
                return Err(IpcFailure::new(
                    "generation_worker_join_failed",
                    failure.clone(),
                    false,
                ));
            }
            let finished_ids = state
                .workers
                .iter()
                .filter_map(|(request_id, slot)| match slot {
                    GenerationWorkerSlot::Running { worker, .. } if worker.is_finished() => {
                        Some(request_id.clone())
                    }
                    GenerationWorkerSlot::Reserved | GenerationWorkerSlot::Running { .. } => None,
                })
                .collect::<Vec<_>>();
            finished_ids
                .into_iter()
                .filter_map(|request_id| match state.workers.remove(&request_id) {
                    Some(GenerationWorkerSlot::Running { worker, owner }) => Some((worker, owner)),
                    Some(GenerationWorkerSlot::Reserved) | None => None,
                })
                .collect::<Vec<_>>()
        };
        self.join_workers(finished).map(|(count, _)| count)
    }

    fn join_all(&self) -> Result<GenerationWorkersJoined, IpcFailure> {
        let workers = {
            let mut state = self.lock()?;
            if let Some(failure) = &state.join_failure {
                return Err(IpcFailure::new(
                    "generation_worker_join_failed",
                    failure.clone(),
                    false,
                ));
            }
            if state
                .workers
                .values()
                .any(|slot| matches!(slot, GenerationWorkerSlot::Reserved))
            {
                return Err(IpcFailure::new(
                    "generation_worker_starting",
                    "a generation worker is still entering its owned lifecycle",
                    true,
                ));
            }
            std::mem::take(&mut state.workers)
                .into_values()
                .filter_map(|slot| match slot {
                    GenerationWorkerSlot::Running { worker, owner } => Some((worker, owner)),
                    GenerationWorkerSlot::Reserved => None,
                })
                .collect::<Vec<_>>()
        };
        let (family_count, backend_workers) = self.join_workers(workers)?;
        Ok(GenerationWorkersJoined {
            registry_identity: Arc::clone(&self.identity),
            family_count,
            backend_workers,
        })
    }

    /// Final event-loop fallback for an operating-system exit that can no
    /// longer be prevented. Every reservation is statically tied to an
    /// application-admission guard, so owning that phase mutex proves there
    /// can be no unattached worker here.
    fn join_all_for_exit(&self) -> GenerationWorkersJoined {
        let workers = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut state.workers)
                .into_values()
                .filter_map(|slot| match slot {
                    GenerationWorkerSlot::Running { worker, owner } => Some((worker, owner)),
                    GenerationWorkerSlot::Reserved => None,
                })
                .collect::<Vec<_>>()
        };
        let family_count = workers.len();
        let mut backend_workers = Vec::with_capacity(family_count);
        for (worker, owner) in workers {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                owner.cancel_all();
            }));
            let _ = worker.join();
            backend_workers.push(owner.shutdown_joined());
        }
        GenerationWorkersJoined {
            registry_identity: Arc::clone(&self.identity),
            family_count,
            backend_workers,
        }
    }

    fn join_workers(
        &self,
        workers: Vec<(JoinHandle<()>, GenerationWorkerOwner)>,
    ) -> Result<(usize, Vec<GenerationBackendWorkerJoined>), IpcFailure> {
        let family_count = workers.len();
        let mut panicked = false;
        let mut backend_workers = Vec::with_capacity(family_count);
        for (worker, owner) in workers {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                owner.cancel_all();
            }));
            panicked |= worker.join().is_err();
            let backend_worker = owner.shutdown_joined();
            panicked |= backend_worker.worker_panicked();
            backend_workers.push(backend_worker);
        }
        if panicked {
            return Err(self.record_join_failure());
        }
        Ok((family_count, backend_workers))
    }

    fn record_join_failure(&self) -> IpcFailure {
        let message =
            "a desktop generation worker panicked; Loom will not infer safe native teardown"
                .to_owned();
        let Ok(mut state) = self.lock() else {
            return IpcFailure::new(
                "generation_worker_state_poisoned",
                "the generation worker registry entered an invalid state; restart Loom",
                false,
            );
        };
        state.join_failure = Some(message.clone());
        IpcFailure::new("generation_worker_join_failed", message, false)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, GenerationWorkerState>, IpcFailure> {
        self.state.lock().map_err(|_| {
            IpcFailure::new(
                "generation_worker_state_poisoned",
                "the generation worker registry entered an invalid state; restart Loom",
                false,
            )
        })
    }
}

impl Default for GenerationWorkerRegistry {
    fn default() -> Self {
        Self {
            identity: Arc::new(WorkerRegistryIdentity),
            state: Mutex::new(GenerationWorkerState::default()),
        }
    }
}

impl GenerationWorkersJoined {
    fn belongs_to(&self, registry: &GenerationWorkerRegistry) -> bool {
        Arc::ptr_eq(&self.registry_identity, &registry.identity)
    }

    #[cfg(test)]
    const fn count(&self) -> usize {
        self.family_count
    }

    fn joined_worker_count(&self) -> usize {
        self.family_count.saturating_add(
            self.backend_workers
                .iter()
                .map(GenerationBackendWorkerJoined::joined_worker_count)
                .sum::<usize>(),
        )
    }
}

impl Default for DownloadWorkerRegistry {
    fn default() -> Self {
        Self {
            identity: Arc::new(WorkerRegistryIdentity),
            state: Mutex::new(DownloadWorkerState::default()),
        }
    }
}

impl DownloadWorkerRegistry {
    fn reserve<'registry, 'admission>(
        &'registry self,
        command_id: CommandId,
        _admission: &'admission MutexGuard<'_, ApplicationPhase>,
    ) -> Result<DownloadWorkerReservation<'registry, 'admission>, IpcFailure> {
        let mut state = self.lock()?;
        if let Some(failure) = &state.join_failure {
            return Err(IpcFailure::new(
                "download_worker_join_failed",
                failure.clone(),
                false,
            ));
        }
        if state.workers.contains_key(&command_id) {
            return Err(IpcFailure::new(
                "download_worker_duplicate",
                "the model download already owns a desktop worker",
                false,
            ));
        }
        if state.workers.len() >= crate::model_download::MAX_RETAINED_MODEL_DOWNLOADS {
            return Err(IpcFailure::new(
                "download_worker_capacity",
                "Loom must join completed model downloads before starting another",
                true,
            ));
        }
        state
            .workers
            .insert(command_id, DownloadWorkerSlot::Reserved);
        Ok(DownloadWorkerReservation {
            registry: self,
            command_id,
            attached: false,
            _admission: PhantomData,
        })
    }

    fn reap_finished(&self) -> Result<usize, IpcFailure> {
        let workers = {
            let mut state = self.lock()?;
            if let Some(failure) = &state.join_failure {
                return Err(IpcFailure::new(
                    "download_worker_join_failed",
                    failure.clone(),
                    false,
                ));
            }
            let finished = state
                .workers
                .iter()
                .filter_map(|(command_id, slot)| match slot {
                    DownloadWorkerSlot::Running { worker, .. } if worker.is_finished() => {
                        Some(*command_id)
                    }
                    DownloadWorkerSlot::Reserved | DownloadWorkerSlot::Running { .. } => None,
                })
                .collect::<Vec<_>>();
            finished
                .into_iter()
                .filter_map(|command_id| match state.workers.remove(&command_id) {
                    Some(DownloadWorkerSlot::Running { worker, .. }) => Some(worker),
                    Some(DownloadWorkerSlot::Reserved) | None => None,
                })
                .collect::<Vec<_>>()
        };
        self.join_workers(workers)
    }

    fn join_all(&self) -> Result<DownloadWorkersJoined, IpcFailure> {
        let workers = {
            let mut state = self.lock()?;
            if let Some(failure) = &state.join_failure {
                return Err(IpcFailure::new(
                    "download_worker_join_failed",
                    failure.clone(),
                    false,
                ));
            }
            if state
                .workers
                .values()
                .any(|slot| matches!(slot, DownloadWorkerSlot::Reserved))
            {
                return Err(IpcFailure::new(
                    "download_worker_starting",
                    "a model download is still entering its owned lifecycle",
                    true,
                ));
            }
            std::mem::take(&mut state.workers)
                .into_values()
                .filter_map(|slot| match slot {
                    DownloadWorkerSlot::Running { worker, .. } => Some(worker),
                    DownloadWorkerSlot::Reserved => None,
                })
                .collect::<Vec<_>>()
        };
        let count = self.join_workers(workers)?;
        Ok(DownloadWorkersJoined {
            registry_identity: Arc::clone(&self.identity),
            count,
        })
    }

    fn join_all_for_exit(&self) -> DownloadWorkersJoined {
        let workers = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut state.workers)
                .into_values()
                .filter_map(|slot| match slot {
                    DownloadWorkerSlot::Running {
                        worker,
                        cancellation,
                    } => Some((worker, cancellation)),
                    DownloadWorkerSlot::Reserved => None,
                })
                .collect::<Vec<_>>()
        };
        let count = workers.len();
        for (worker, cancellation) in workers {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cancellation.cancel();
            }));
            let _ = worker.join();
        }
        DownloadWorkersJoined {
            registry_identity: Arc::clone(&self.identity),
            count,
        }
    }

    fn join_workers(&self, workers: Vec<JoinHandle<()>>) -> Result<usize, IpcFailure> {
        let count = workers.len();
        let mut first_failure = None;
        for worker in workers {
            if worker.join().is_err() {
                first_failure.get_or_insert_with(|| "worker panicked".to_owned());
            }
        }
        if let Some(error) = first_failure {
            let message = format!(
                "a desktop model download worker failed before it could be joined: {error}"
            );
            if let Ok(mut state) = self.state.lock() {
                state.join_failure = Some(message.clone());
            }
            return Err(IpcFailure::new(
                "download_worker_join_failed",
                message,
                false,
            ));
        }
        Ok(count)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, DownloadWorkerState>, IpcFailure> {
        self.state.lock().map_err(|_| {
            IpcFailure::new(
                "download_worker_state_poisoned",
                "the download worker registry entered an invalid state; restart Loom",
                false,
            )
        })
    }
}

impl DownloadWorkerReservation<'_, '_> {
    fn attach(
        mut self,
        worker: JoinHandle<()>,
        cancellation: DownloadCancellation,
    ) -> Result<(), DownloadWorkerAttachError> {
        let mut state = match self.registry.lock() {
            Ok(state) => state,
            Err(failure) => return Err(DownloadWorkerAttachError { failure, worker }),
        };
        match state.workers.get_mut(&self.command_id) {
            Some(slot @ DownloadWorkerSlot::Reserved) => {
                *slot = DownloadWorkerSlot::Running {
                    worker,
                    cancellation,
                };
                self.attached = true;
                Ok(())
            }
            Some(DownloadWorkerSlot::Running { .. }) | None => Err(DownloadWorkerAttachError {
                failure: IpcFailure::new(
                    "download_worker_state_changed",
                    "the model download worker reservation changed before attachment",
                    false,
                ),
                worker,
            }),
        }
    }
}

impl Drop for DownloadWorkerReservation<'_, '_> {
    fn drop(&mut self) {
        if self.attached {
            return;
        }
        if let Ok(mut state) = self.registry.state.lock()
            && matches!(
                state.workers.get(&self.command_id),
                Some(DownloadWorkerSlot::Reserved)
            )
        {
            state.workers.remove(&self.command_id);
        }
    }
}

impl DownloadWorkersJoined {
    fn belongs_to(&self, registry: &DownloadWorkerRegistry) -> bool {
        Arc::ptr_eq(&self.registry_identity, &registry.identity)
    }

    const fn count(&self) -> usize {
        self.count
    }
}

impl GenerationWorkerReservation<'_, '_> {
    fn attach(
        mut self,
        worker: JoinHandle<()>,
        owner: GenerationWorkerOwner,
    ) -> Result<(), GenerationWorkerAttachError> {
        let mut state = match self.registry.lock() {
            Ok(state) => state,
            Err(failure) => {
                return Err(GenerationWorkerAttachError {
                    failure,
                    worker,
                    owner,
                });
            }
        };
        match state.workers.get_mut(&self.request_id) {
            Some(slot @ GenerationWorkerSlot::Reserved) => {
                *slot = GenerationWorkerSlot::Running { worker, owner };
                self.attached = true;
                Ok(())
            }
            Some(GenerationWorkerSlot::Running { .. }) | None => Err(GenerationWorkerAttachError {
                failure: IpcFailure::new(
                    "generation_worker_state_changed",
                    "the desktop generation worker reservation changed before attachment",
                    false,
                ),
                worker,
                owner,
            }),
        }
    }
}

impl Drop for GenerationWorkerReservation<'_, '_> {
    fn drop(&mut self) {
        if self.attached {
            return;
        }
        if let Ok(mut state) = self.registry.state.lock()
            && matches!(
                state.workers.get(&self.request_id),
                Some(GenerationWorkerSlot::Reserved)
            )
        {
            state.workers.remove(&self.request_id);
        }
    }
}

enum ModelLoadPlan {
    Ready(ModelCapabilitySummary),
    Inspect {
        canonical_path: PathBuf,
        profile: LocalModelProfile,
    },
}

enum PolicyModelLoadPlan {
    Ready(ModelCapabilitySummary),
    Inspect {
        canonical_path: PathBuf,
        profile: LocalModelProfile,
        expectation: PolicyWriterExpectation,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PolicyWriterExpectation {
    profile_id: String,
    rank: u32,
    role: ModelRole,
    prompt_mode: PromptMode,
    model_sha256: BlobId,
    model_file_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PolicyFileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct VerifiedPolicyFile {
    canonical_path: PathBuf,
    identity: FileIdentityHandle,
    stamp: PolicyFileStamp,
}

#[derive(Debug)]
struct PolicyInspectionFailure {
    failure: IpcFailure,
    native_inspection_started: bool,
}

struct PreparedModelDownload {
    command_id: CommandId,
    request: GgufDownloadRequest,
    spec: ModelDownloadSpec,
}

#[derive(Debug)]
struct LlamaCancellation {
    handle: Arc<LlamaGenerationControl>,
}

impl BranchCancellation for LlamaCancellation {
    fn cancel_branch(&self, branch_id: BranchId) -> bool {
        self.handle.cancel_branch(branch_id)
    }
}

#[derive(Debug, Default)]
pub struct Builder {
    build_model_policy: BuildModelPolicy,
}

impl Builder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_build_model_policy(mut self, build_model_policy: BuildModelPolicy) -> Self {
        self.build_model_policy = build_model_policy;
        self
    }

    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        let build_model_policy = self.build_model_policy;
        PluginBuilder::new("loom")
            .invoke_handler(tauri::generate_handler![
                project_open_default,
                project_choose_create,
                project_choose_open,
                project_close,
                project_current,
                project_recover,
                document_open,
                document_checkpoint,
                document_draft_upsert,
                document_draft_clear,
                document_reconciliation_preview,
                document_reconcile_apply,
                build_model_policy_get,
                model_list,
                model_choose,
                model_load,
                model_load_policy_candidate,
                model_unload,
                model_download_start,
                model_download_cancel,
                model_download_status,
                model_download_list,
                branch_page,
                branch_get,
                branch_body,
                weave_status,
                weave_start,
                generation_cancel,
                candidate_keep,
                candidate_promote,
                research_promotion_import,
                research_promotion_pending,
                research_promotion_confirm,
                suggestions_set,
                focus_mode_set,
                application_close,
                application_close_abort,
                application_close_pending,
            ])
            .setup(move |app, _api| {
                let model_library_root = app.path().app_local_data_dir().ok();
                app.manage(PluginState::with_model_library_root(
                    model_library_root,
                    build_model_policy,
                ));
                Ok(())
            })
            .on_window_ready(|window| {
                if window.label() != "main" {
                    return;
                }
                let app = window.app_handle().clone();
                let event_app = app.clone();
                let window_id = ForegroundWindowId::new(window.label())
                    .expect("Tauri window labels are bounded application constants");
                let event_window_id = window_id.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::Focused(focused) = event {
                        let _ = event_app
                            .state::<PluginState>()
                            .foreground_commands
                            .observe_window_focus(event_window_id.clone(), *focused);
                    }
                    if let WindowEvent::CloseRequested { api, .. } = event
                        && !prepare_application_exit_request(&event_app)
                    {
                        api.prevent_close();
                        emit_application_close_request(&event_app);
                    }
                });
                // Install the event listener first, then sample native state.
                // This closes the registration/sampling window: a transition
                // is either observed by the callback or by this later sample.
                if let Ok(focused) = window.is_focused() {
                    let _ = app
                        .state::<PluginState>()
                        .foreground_commands
                        .observe_window_focus(window_id, focused);
                }
            })
            .on_event(|app, event| {
                if let RunEvent::Exit = event {
                    quiesce_unpreventable_runtime_exit(app);
                }
                if let RunEvent::MenuEvent(menu_event) = event
                    && menu_event.id() == APPLICATION_QUIT_MENU_ID
                {
                    let _ = prepare_application_exit_request(app);
                    emit_application_close_request(app);
                }
                if let RunEvent::ExitRequested { api, .. } = event
                    && !prepare_application_exit_request(app)
                {
                    api.prevent_exit();
                    emit_application_close_request(app);
                }
            })
            .build()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct IpcFailure {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl IpcFailure {
    fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the exhaustive store-to-IPC mapping is safer as one auditable match"
    )]
    #[allow(clippy::needless_pass_by_value)]
    fn store(error: loom_store::StoreError) -> Self {
        use loom_store::StoreError;

        let code = match &error {
            StoreError::Io(_) => "filesystem_error",
            StoreError::Sqlite(_) => "database_error",
            StoreError::Json(_) => "manifest_json_error",
            StoreError::Document(_) => "document_projection_error",
            StoreError::ResearchCall(_) => "research_call_invalid",
            StoreError::ResearchAssembly(_) => "research_assembly_invalid",
            StoreError::ResearchAdmission(_) => "research_admission_rejected",
            StoreError::EmptyResearchExecutionRecord => "research_execution_record_empty",
            StoreError::ResearchExecutionRecordTooLarge { .. } => {
                "research_execution_record_too_large"
            }
            StoreError::ResearchExecutionRecordConflict { .. } => {
                "research_execution_record_conflict"
            }
            StoreError::ResearchSubjectProjectMismatch => "research_subject_project_mismatch",
            StoreError::ResearchExecutionSubjectConflict { .. } => {
                "research_execution_subject_conflict"
            }
            StoreError::ResearchCampaignNotPersisted(_) => "research_campaign_not_persisted",
            StoreError::InvalidFrozenResearchSubject(_) => "invalid_frozen_research_subject",
            StoreError::InvalidResearchDiagnostic(_) => "invalid_research_diagnostic",
            StoreError::ResearchSessionSubjectNotPersisted { .. } => {
                "research_session_subject_not_persisted"
            }
            StoreError::ResearchSessionAlreadyActive { .. } => "research_session_already_active",
            StoreError::TrialRunNotDispatched(_) => "trial_run_not_dispatched",
            StoreError::ResearchJournalLeaseMismatch => "research_journal_lease_mismatch",
            StoreError::InvalidResearchJournalMutation => "invalid_research_journal_mutation",
            StoreError::ResearchJournalEventLimit { .. } => "research_journal_event_limit",
            StoreError::ResearchJournalRecordTooLarge { .. } => "research_journal_record_too_large",
            StoreError::ResearchJournalTotalTooLarge { .. } => "research_journal_total_too_large",
            StoreError::SessionEntropy(_) => "session_entropy_unavailable",
            StoreError::NonUtf8Path(_) => "non_utf8_path",
            StoreError::UnsafeRelativePath(_) => "unsafe_relative_path",
            StoreError::SymbolicLink(_) => "symbolic_link_refused",
            StoreError::NotDirectory(_) => "not_a_directory",
            StoreError::NotRegularFile(_) => "not_a_regular_file",
            StoreError::AlreadyInitialized(_) => "project_already_initialized",
            StoreError::ProjectAlreadyOpen(_) => "project_already_open",
            StoreError::NotAProject(_) => "not_a_loom_project",
            StoreError::UnsupportedFormat(_) => "unsupported_project_format",
            StoreError::UnsupportedSchema { .. } => "unsupported_project_schema",
            StoreError::InvalidProjectName { .. } => "invalid_project_name",
            StoreError::ReasonTooLong { .. } => "checkpoint_reason_too_long",
            StoreError::DocumentTooLarge { .. } => "document_too_large",
            StoreError::DocumentKindMismatch { .. } => "document_kind_mismatch",
            StoreError::VisibleFileConflict { .. } => "visible_file_conflict",
            StoreError::VisibleFileAlreadyExists(_) => "visible_file_already_exists",
            StoreError::DocumentAlreadyExists(_) => "document_already_exists",
            StoreError::UncheckpointedVisibleChange(_) => "external_file_change",
            StoreError::ExternalVisibleFileDeleted(_) => "external_file_deleted",
            StoreError::ExternalVisibleBlobMismatch { .. } => "external_file_conflict",
            StoreError::ExternalVisibleInvalidUtf8(_) => "external_file_invalid_utf8",
            StoreError::MissingBlob { .. } => "missing_content_blob",
            StoreError::UnregisteredBlob(_) => "unregistered_content_blob",
            StoreError::CorruptBlob { .. } => "corrupt_content_blob",
            StoreError::CorruptDatabase(_) => "corrupt_project_database",
            StoreError::NoActiveRevision(_) => "no_active_revision",
            StoreError::SourceRevisionMismatch { .. } => "source_revision_conflict",
            StoreError::SourceBlobMismatch { .. } => "source_blob_conflict",
            StoreError::ArtifactKindMismatch { .. } => "artifact_kind_mismatch",
            StoreError::ModelEnvironmentContentConflict { .. } => {
                "model_environment_content_conflict"
            }
            StoreError::GenerationRunNotFound(_) => "generation_run_not_found",
            StoreError::InvalidBranchPageLimit { .. } => "invalid_branch_page_limit",
            StoreError::InvalidBranchPageCursor => "invalid_branch_page_cursor",
            StoreError::InvalidBranchBodyLimit { .. } => "invalid_branch_body_limit",
            StoreError::BranchBodyTooLarge { .. } => "branch_body_too_large",
            StoreError::EmptyGenerationFamily => "empty_generation_family",
            StoreError::DuplicateGenerationRun(_) => "duplicate_generation_run",
            StoreError::DuplicateGenerationBranch(_) => "duplicate_generation_branch",
            StoreError::GenerationFamilySourceMismatch => "generation_family_source_mismatch",
            StoreError::CandidateNotFound(_) => "candidate_not_found",
            StoreError::LegacyCandidateNotAdmitted => "legacy_candidate_not_admitted",
            StoreError::GenerationAlreadyTerminal(_) => "generation_already_terminal",
            StoreError::CompletedGenerationRequiresCandidate => {
                "generation_terminal_candidate_required"
            }
            StoreError::CandidateReadyRequiresTerminalCandidate => {
                "candidate_ready_requires_terminal"
            }
            StoreError::FailedGenerationRequiresError => "failed_generation_requires_error",
            StoreError::CriticCannotPromote => "critic_cannot_promote",
            StoreError::ModelRoleNotAssigned => "model_role_not_assigned",
            StoreError::InvalidAuthorityPolicy => "invalid_authority_policy",
            StoreError::InvalidGenerationRange => "invalid_generation_range",
            StoreError::NonCanonicalGeneratedText => "noncanonical_generated_text",
            StoreError::IdempotencyConflict { .. } => "idempotency_conflict",
            StoreError::ProvenancePayloadTooLarge { .. } => "provenance_payload_too_large",
            StoreError::EditDiffBudgetExceeded { .. } => "edit_diff_budget_exceeded",
            StoreError::RevisionSegmentLimitExceeded { .. } => "revision_segment_limit_exceeded",
            StoreError::TransientDraftVersionConflict { .. } => "transient_draft_version_conflict",
            StoreError::TransientDraftIdentityMismatch { .. } => {
                "transient_draft_identity_mismatch"
            }
        };
        let retryable = matches!(
            error,
            StoreError::ProjectAlreadyOpen(_)
                | StoreError::ResearchSessionAlreadyActive { .. }
                | StoreError::SessionEntropy(_)
        );
        Self::new(code, error.to_string(), retryable)
    }

    fn merge(error: &MergeError) -> Self {
        let code = match error {
            MergeError::HybridMetadataRequired => "hybrid_reconciliation_unsupported",
            MergeError::BudgetExceeded { .. } => "merge_budget_exceeded",
            MergeError::RangeTooLarge => "merge_range_too_large",
            MergeError::InvalidEditScript => "merge_invalid_edit_script",
        };
        Self::new(code, error.to_string(), false)
    }

    fn backend(error: &LlamaBackendError) -> Self {
        let retryable = matches!(error, LlamaBackendError::ResultTimeout);
        Self::new("local_model_error", error.to_string(), retryable)
    }

    fn generation_registry(error: &GenerationRegistryError) -> Self {
        let retryable = matches!(error, GenerationRegistryError::CapacityExceeded { .. });
        Self::new("generation_lifecycle_error", error.to_string(), retryable)
    }

    fn model_download_registry(error: &ModelDownloadRegistryError) -> Self {
        let (code, retryable) = match error {
            ModelDownloadRegistryError::IdempotencyConflict { .. } => {
                ("model_download_idempotency_conflict", false)
            }
            ModelDownloadRegistryError::ActiveCapacity { .. }
            | ModelDownloadRegistryError::RetainedCapacity { .. } => {
                ("model_download_capacity", true)
            }
            ModelDownloadRegistryError::NotFound(_) => ("model_download_not_found", false),
            ModelDownloadRegistryError::AlreadyTerminal(_) => {
                ("model_download_already_terminal", false)
            }
            ModelDownloadRegistryError::Poisoned => ("model_download_state_error", false),
        };
        Self::new(code, error.to_string(), retryable)
    }

    fn model_library(error: &ModelLibraryError) -> Self {
        let (code, retryable) = match error {
            ModelLibraryError::InvalidFileName => ("invalid_model_file_name", false),
            ModelLibraryError::Symlink(_) => ("model_library_symlink_refused", false),
            ModelLibraryError::NotDirectory(_) => ("model_library_not_directory", false),
            ModelLibraryError::Io { .. } => ("model_library_io_error", true),
        };
        Self::new(code, error.to_string(), retryable)
    }

    fn model_download_request(error: &DownloadError) -> Self {
        Self::new("invalid_model_download", error.to_string(), false)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectSnapshot {
    project_id: String,
    session_id: String,
    title: String,
    root: String,
    schema_version: u32,
    documents: Vec<DocumentSummary>,
    pending_recovery: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectCloseReceipt {
    command_id: String,
    project_id: String,
    session_id: String,
    closed_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DocumentSummary {
    document_id: String,
    relative_path: String,
    title: String,
    kind: DocumentKind,
    revision_id: Option<String>,
    active_blob_id: Option<String>,
    word_count: usize,
    externally_modified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenDocument {
    summary: DocumentSummary,
    visible_blob_id: String,
    text: String,
    transient_draft: Option<TransientDraftSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransientDraftSnapshot {
    document_id: String,
    source_revision_id: String,
    blob_id: String,
    version: String,
    kind: DocumentKind,
    text: String,
    updated_at_unix_ms: i64,
    replayed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransientDraftWriteReceipt {
    document_id: String,
    source_revision_id: String,
    blob_id: String,
    version: String,
    kind: DocumentKind,
    updated_at_unix_ms: i64,
    replayed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationAppSource {
    Caller,
    TransientDraft,
    Base,
}

/// Exact, untruncated inputs and immutable identities for one bounded merge
/// preview. `external_text` is the canonical merge input;
/// `external_visible_text` is the exact UTF-8 text currently on disk and is
/// the value bound by `external_visible_blob_id`.
#[derive(Clone, Debug, Serialize)]
pub struct ReconciliationPreview {
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    kind: DocumentKind,
    active_revision_id: String,
    active_artifact_id: String,
    base_blob_id: String,
    app_blob_id: String,
    external_blob_id: String,
    external_visible_blob_id: String,
    base_text: String,
    app_text: String,
    external_text: String,
    external_visible_text: String,
    app_source: ReconciliationAppSource,
    draft_version: Option<String>,
    outcome: MergeOutcome,
}

#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    command_id: String,
    command_kind: String,
    project_id: String,
    schema_version: u32,
    source_revision_id: Option<String>,
    result_revision_id: Option<String>,
    result_blob_id: Option<String>,
    request_fingerprint: Option<String>,
    replayed: bool,
    visible_projection: Option<VisibleProjectionState>,
    artifact_ids: Vec<String>,
    completed_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResearchPromotionPrompt {
    command_id: String,
    nonce: String,
    document_id: String,
    candidate_fingerprint: String,
    promotion_fingerprint: String,
    subject_kind: &'static str,
    expires_at_unix_ms: i64,
    result_text: String,
}

/// A controller-produced, non-authorizing research packet selected through the
/// native file picker. Deserialization validates the mixed-authorship graph;
/// the host still has to admit the exact output into the live project store
/// before it can issue a foreground challenge.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchPromotionPacket {
    schema: String,
    document_id: String,
    record: MixedAuthorshipAssemblyRecord,
    result_text: String,
}

impl ResearchPromotionPrompt {
    fn from_parts(
        request: &PromotionCommandRequest,
        challenge: &ForegroundCommandChallenge,
        result_text: &str,
    ) -> Self {
        Self {
            command_id: request.command_id().to_string(),
            nonce: challenge.nonce.to_string(),
            document_id: challenge.binding.document_id.to_string(),
            candidate_fingerprint: challenge.binding.candidate_fingerprint.to_string(),
            promotion_fingerprint: request.command_request_fingerprint().to_string(),
            subject_kind: request.subject().kind_name(),
            expires_at_unix_ms: challenge.expires_at_unix_ms,
            result_text: result_text.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchPromotionConfirmInput {
    command_id: String,
    nonce: String,
    document_id: String,
    candidate_fingerprint: String,
    promotion_fingerprint: String,
}

#[derive(Debug, Serialize)]
pub struct ResearchPromotionResult {
    receipt: Receipt,
    foreground_receipt_blob_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecoveryReport {
    recovered: usize,
    conflicts: Vec<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Serialize)]
pub struct ModelCapabilitySummary {
    model_id: String,
    display_name: String,
    local: bool,
    loaded: bool,
    chat: bool,
    completion: bool,
    fill_in_middle: bool,
    output_tokens: bool,
    logprobs: bool,
    model_path: String,
    file_bytes: u64,
    header_verified: bool,
    architecture: Option<String>,
    context_tokens: Option<u32>,
    model_sha256: Option<String>,
    projector_present: Option<bool>,
    media_kinds: Vec<&'static str>,
    /// A size-only policy hint. It is emitted only for uninspected discoveries.
    policy_candidate: Option<PolicyProfileSummary>,
    /// Exact policy identity after native inspection and digest agreement.
    policy_verified: Option<PolicyProfileSummary>,
    /// Compatibility alias for the first UI slice. New code uses
    /// `policy_verified.profile_id`.
    tested_profile: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PolicyProfileSummary {
    profile_id: String,
    rank: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelUnloadOutcome {
    model_id: Option<String>,
    resident_slot_released: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchSnapshot {
    run_id: String,
    branch_id: String,
    document_id: String,
    candidate_id: Option<String>,
    source_revision_id: String,
    target_start_byte: u64,
    target_end_byte: u64,
    text: String,
    output_blob_id: Option<String>,
    output_byte_len: Option<u64>,
    status: &'static str,
    seed: String,
    model_id: String,
    selection: Option<&'static str>,
    error: Option<String>,
    error_truncated: bool,
    created_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BranchCursorSnapshot {
    /// Decimal u64, preserved as text across the JavaScript boundary.
    sequence: String,
    run_id: String,
}

impl TryFrom<BranchCursorSnapshot> for BranchPageCursor {
    type Error = IpcFailure;

    fn try_from(cursor: BranchCursorSnapshot) -> Result<Self, Self::Error> {
        let sequence = cursor.sequence.parse::<u64>().map_err(|_| {
            IpcFailure::new(
                "invalid_branch_page_cursor",
                "branch cursor sequence is not a decimal u64",
                false,
            )
        })?;
        let run_id = cursor.run_id.parse::<GenerationRunId>().map_err(|_| {
            IpcFailure::new(
                "invalid_branch_page_cursor",
                "branch cursor run ID is not a valid ULID",
                false,
            )
        })?;
        Ok(Self { sequence, run_id })
    }
}

impl From<BranchPageCursor> for BranchCursorSnapshot {
    fn from(cursor: BranchPageCursor) -> Self {
        Self {
            sequence: cursor.sequence.to_string(),
            run_id: cursor.run_id.to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchSummarySnapshot {
    run_id: String,
    branch_id: String,
    document_id: String,
    candidate_id: Option<String>,
    source_revision_id: String,
    target_start_byte: u64,
    target_end_byte: u64,
    output_blob_id: Option<String>,
    output_byte_len: Option<u64>,
    status: &'static str,
    seed: Option<String>,
    model_id: Option<String>,
    selection: Option<&'static str>,
    error: Option<String>,
    error_truncated: bool,
    created_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchPageSnapshot {
    branches: Vec<BranchSummarySnapshot>,
    next_cursor: Option<BranchCursorSnapshot>,
    has_more: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchBodySnapshot {
    run_id: String,
    branch_id: String,
    document_id: String,
    candidate_id: String,
    source_revision_id: String,
    target_start_byte: u64,
    target_end_byte: u64,
    seed: String,
    model_id: String,
    created_at_unix_ms: i64,
    output_blob_id: String,
    byte_len: u64,
    text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WeaveStarted {
    command_id: String,
    request_id: String,
    project_id: String,
    session_id: String,
    document_id: String,
    source_revision_id: String,
    exact_prompt_blob_id: String,
    branches: Vec<BranchSnapshot>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WeavePolicySnapshot {
    AutomaticV2 {},
    ManualV2 {
        branch_count: u32,
        max_tokens: u32,
        temperature: f32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WeavePreset {
    AutomaticProseV2,
    AutomaticVerseV2,
    ManualV2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedWeavePolicy {
    preset: WeavePreset,
    branch_count: u32,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Debug, PartialEq)]
enum ValidatedWeavePolicy {
    AutomaticV2,
    ManualV2 {
        branch_count: u32,
        max_tokens: u32,
        temperature: f32,
    },
}

const AUTOMATIC_WEAVE_BRANCH_COUNT_V2: u32 = 3;
const AUTOMATIC_WEAVE_MAX_TOKENS_V2: u32 = 48;
const AUTOMATIC_WEAVE_TEMPERATURE_V2: f32 = 0.8;

#[derive(Clone, Debug, Serialize)]
struct DesktopLoomEvent {
    project_id: String,
    session_id: String,
    document_id: String,
    request_id: String,
    event: LoomEvent,
}

/// Opens the app-owned writing folder, creating its first empty manuscript on
/// first launch. The folder is still an ordinary Loom project; this command
/// merely removes file-management ceremony from the default authoring path.
#[tauri::command]
async fn project_open_default<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ProjectSnapshot, IpcFailure> {
    ensure_application_running(&state, "a project session")?;
    reserve_project_choice(&state)?;
    let result = app
        .path()
        .app_local_data_dir()
        .map_err(|error| {
            IpcFailure::new(
                "default_project_directory_unavailable",
                format!("the local writing directory is unavailable: {error}"),
                true,
            )
        })
        .and_then(|root| open_or_initialize_default_project(&root.join(DEFAULT_PROJECT_DIRECTORY)));
    finish_project_choice(&state, result)
}

#[tauri::command]
async fn project_choose_create<R: Runtime>(
    title: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ProjectSnapshot, IpcFailure> {
    ensure_application_running(&state, "a project session")?;
    reserve_project_choice(&state)?;
    let result = choose_project_folder(&app).and_then(|path| initialize_project(&path, title));
    finish_project_choice(&state, result)
}

fn reserve_project_choice(state: &State<'_, PluginState>) -> Result<(), IpcFailure> {
    let _application_admission = lock_application_admission(state, "a project session")?;
    let mut session = lock_session(state)?;
    if session.phase != SessionPhase::Closed {
        return Err(IpcFailure::new(
            "project_session_active",
            "close the current project or folder chooser before opening another",
            false,
        ));
    }
    session.phase = SessionPhase::Choosing;
    Ok(())
}

fn finish_project_choice(
    state: &State<'_, PluginState>,
    result: Result<ProjectStore, IpcFailure>,
) -> Result<ProjectSnapshot, IpcFailure> {
    let store = match result {
        Ok(store) => store,
        Err(error) => {
            release_project_choice(state)?;
            return Err(error);
        }
    };
    let session_id = CommandId::new();
    let snapshot = match snapshot_for(&store, session_id) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            release_project_choice(state)?;
            return Err(error);
        }
    };
    let mut session = lock_session_internal(state)?;
    if session.phase != SessionPhase::Choosing {
        return Err(IpcFailure::new(
            "project_choice_state_changed",
            "the project chooser lost its reserved session",
            false,
        ));
    }
    session.store = Some(store);
    session.active_session_id = Some(session_id);
    session.agency = AgencyGate::default();
    session.phase = SessionPhase::Open;
    Ok(snapshot)
}

fn release_project_choice(state: &State<'_, PluginState>) -> Result<(), IpcFailure> {
    let mut session = lock_session_internal(state)?;
    if session.phase == SessionPhase::Choosing {
        session.phase = SessionPhase::Closed;
    }
    Ok(())
}

fn choose_project_folder<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, IpcFailure> {
    let selected = app.dialog().file().blocking_pick_folder().ok_or_else(|| {
        IpcFailure::new(
            "folder_selection_cancelled",
            "no project folder was selected",
            false,
        )
    })?;
    selected.into_path().map_err(|error| {
        IpcFailure::new(
            "selected_folder_unavailable",
            format!("the selected folder is not a local filesystem path: {error}"),
            false,
        )
    })
}

fn choose_model_file<R: Runtime>(app: &AppHandle<R>) -> Result<Option<PathBuf>, IpcFailure> {
    app.dialog()
        .file()
        .add_filter("GGUF model", &["gguf"])
        .blocking_pick_file()
        .map(|selected| {
            selected.into_path().map_err(|error| {
                IpcFailure::new(
                    "selected_model_unavailable",
                    format!("the selected model is not a local filesystem path: {error}"),
                    false,
                )
            })
        })
        .transpose()
}

fn initialize_project(path: &Path, title: String) -> Result<ProjectStore, IpcFailure> {
    let existing_initial = path.join(INITIAL_DOCUMENT);
    match std::fs::symlink_metadata(&existing_initial) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(IpcFailure::new(
                "initial_document_symlink",
                "the default manuscript path is a symbolic link; import it explicitly instead",
                false,
            ));
        }
        Ok(metadata) if metadata.is_file() => {
            return Err(IpcFailure::new(
                "existing_manuscript_requires_import",
                "the default manuscript file already exists; import the folder instead of creating an empty project",
                false,
            ));
        }
        Ok(_) => {
            return Err(IpcFailure::new(
                "initial_document_not_file",
                "the default manuscript path already exists and is not a regular file",
                false,
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(IpcFailure::new(
                "initial_document_inspection_failed",
                format!("could not inspect the default manuscript path: {error}"),
                false,
            ));
        }
    }
    let (mut store, _receipt) = ProjectStore::initialize(path, title).map_err(IpcFailure::store)?;
    store
        .create_document_if_absent(
            INITIAL_DOCUMENT,
            DocumentContent::Prose(String::new()),
            "initial manuscript",
        )
        .map_err(IpcFailure::store)?;
    Ok(store)
}

fn open_or_initialize_default_project(path: &Path) -> Result<ProjectStore, IpcFailure> {
    let manifest = path.join(".loom/project.json");
    let initialized = manifest.try_exists().map_err(|error| {
        IpcFailure::new(
            "default_project_inspection_failed",
            format!("the local writing folder could not be inspected: {error}"),
            true,
        )
    })?;
    let mut store = if initialized {
        ProjectStore::open(path).map_err(IpcFailure::store)?
    } else {
        validate_default_document_candidate(path)?;
        ProjectStore::initialize(path, "My Writing")
            .map(|(store, _)| store)
            .map_err(IpcFailure::store)?
    };

    // Settle an initialization/adoption transaction before deciding whether
    // the default document is absent. A registered document whose visible file
    // was later deleted is an external deletion, never permission to recreate
    // an empty file over the author's history.
    store.recover().map_err(IpcFailure::store)?;
    store
        .recover_interrupted_generations()
        .map_err(IpcFailure::store)?;
    ensure_default_document(&mut store)?;
    store.record_open().map_err(IpcFailure::store)?;
    Ok(store)
}

fn validate_default_document_candidate(path: &Path) -> Result<(), IpcFailure> {
    let visible = path.join(INITIAL_DOCUMENT);
    match std::fs::symlink_metadata(&visible) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(IpcFailure::new(
            "default_document_symlink",
            "Loom will not adopt an app-owned manuscript path that is a symbolic link",
            false,
        )),
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(IpcFailure::new(
            "default_document_not_file",
            "the app-owned manuscript path exists but is not a regular file",
            false,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IpcFailure::new(
            "default_document_inspection_failed",
            format!("the app-owned manuscript path could not be inspected: {error}"),
            true,
        )),
    }
}

fn ensure_default_document(store: &mut ProjectStore) -> Result<(), IpcFailure> {
    if store
        .list_documents()
        .map_err(IpcFailure::store)?
        .iter()
        .any(|document| document.relative_path == INITIAL_DOCUMENT)
    {
        return Ok(());
    }

    let visible = store.root().join(INITIAL_DOCUMENT);
    match std::fs::symlink_metadata(&visible) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(IpcFailure::new(
            "default_document_symlink",
            "Loom will not adopt an app-owned manuscript path that is a symbolic link",
            false,
        )),
        Ok(metadata) if metadata.is_file() => {
            store
                .adopt_visible_document_if_absent(
                    INITIAL_DOCUMENT,
                    DocumentKind::Prose,
                    "recover existing app-owned manuscript",
                )
                .map_err(IpcFailure::store)?;
            Ok(())
        }
        Ok(_) => Err(IpcFailure::new(
            "default_document_not_file",
            "the app-owned manuscript path exists but is not a regular file",
            false,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            store
                .create_document_if_absent(
                    INITIAL_DOCUMENT,
                    DocumentContent::Prose(String::new()),
                    "initial manuscript",
                )
                .map_err(IpcFailure::store)?;
            Ok(())
        }
        Err(error) => Err(IpcFailure::new(
            "default_document_inspection_failed",
            format!("the app-owned manuscript path could not be inspected: {error}"),
            true,
        )),
    }
}

#[tauri::command]
async fn project_choose_open<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ProjectSnapshot, IpcFailure> {
    ensure_application_running(&state, "a project session")?;
    reserve_project_choice(&state)?;
    let result = choose_project_folder(&app).and_then(|path| {
        let mut store = ProjectStore::open(path).map_err(IpcFailure::store)?;
        store
            .recover_interrupted_generations()
            .map_err(IpcFailure::store)?;
        store.record_open().map_err(IpcFailure::store)?;
        Ok(store)
    });
    finish_project_choice(&state, result)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn project_close(
    project_id: String,
    session_id: String,
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<ProjectCloseReceipt, IpcFailure> {
    let command_id = command_id.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "close command ID is not a valid ULID",
            false,
        )
    })?;
    close_project_with_wait(
        &state,
        project_id,
        session_id,
        command_id,
        PROJECT_CLOSE_GENERATION_WAIT,
    )
}

// Keep the close ordering visible in one routine: revoke authority, cancel,
// drain, repair, close, and only then release the session.
#[allow(clippy::too_many_lines)]
fn close_project_with_wait(
    state: &PluginState,
    project_id: String,
    session_id: String,
    command_id: CommandId,
    generation_wait: Duration,
) -> Result<ProjectCloseReceipt, IpcFailure> {
    let (typed_project_id, typed_session_id) = {
        let mut session = lock_session(state)?;
        if session.phase == SessionPhase::Closed {
            if let Some(receipt) = &session.last_close
                && receipt.command_id == command_id.to_string()
                && receipt.project_id == project_id
                && receipt.session_id == session_id
            {
                return Ok(receipt.clone());
            }
            return Err(IpcFailure::new(
                "project_not_open",
                "the requested project session is not open",
                false,
            ));
        }
        require_bound_store(&mut session, &project_id, &session_id)?;
        let typed_project_id = session
            .store
            .as_ref()
            .ok_or_else(|| IpcFailure::new("project_not_open", "open a Loom project first", false))?
            .manifest()
            .project_id;
        let typed_session_id = session.active_session_id.ok_or_else(|| {
            IpcFailure::new(
                "corrupt_project_session",
                "the live project session is missing its session ID",
                false,
            )
        })?;
        // Admission and route reservation use this same session mutex. Once
        // these flags change, an admitted family is either already visible to
        // the registry below or it cannot reserve routes at all.
        session.agency.set_automation_enabled(false);
        session.agency.set_focus_mode(true);
        (typed_project_id, typed_session_id)
    };

    // Project close removes command authority before waiting for inference.
    // A slow or failed drain must never leave a promotion nonce usable.
    state
        .foreground_commands
        .revoke_application_session(typed_session_id)
        .map_err(|error| {
            IpcFailure::new(
                "foreground_command_state_unavailable",
                error.to_string(),
                false,
            )
        })?;
    state
        .research_promotions
        .lock()
        .map_err(|_| {
            IpcFailure::new(
                "research_promotion_state_unavailable",
                "the pending research-promotion registry is unavailable",
                false,
            )
        })?
        .by_command
        .retain(|_, pending| pending.session_id != typed_session_id);

    cancel_and_drain_generation_session(
        state,
        typed_project_id,
        typed_session_id,
        generation_wait,
    )?;

    let mut session = lock_session(state)?;
    if session.phase == SessionPhase::Closed {
        if let Some(receipt) = &session.last_close
            && receipt.command_id == command_id.to_string()
            && receipt.project_id == project_id
            && receipt.session_id == session_id
        {
            return Ok(receipt.clone());
        }
        return Err(IpcFailure::new(
            "project_not_open",
            "the requested project session is not open",
            false,
        ));
    }
    require_bound_store(&mut session, &project_id, &session_id)?;
    if session.active_session_id != Some(typed_session_id)
        || session
            .store
            .as_ref()
            .is_none_or(|store| store.manifest().project_id != typed_project_id)
    {
        return Err(IpcFailure::new(
            "stale_project_session",
            "the project session changed while Loom cancelled its active strands",
            false,
        ));
    }
    let receipt = ProjectCloseReceipt {
        command_id: command_id.to_string(),
        project_id,
        session_id,
        closed_at_unix_ms: now_unix_ms(),
    };
    session.store = None;
    session.active_session_id = None;
    session.agency = AgencyGate::default();
    session.phase = SessionPhase::Closed;
    session.last_close = Some(receipt.clone());
    Ok(receipt)
}

fn cancel_and_drain_generation_session(
    state: &PluginState,
    project_id: ProjectId,
    session_id: CommandId,
    wait: Duration,
) -> Result<(), IpcFailure> {
    state
        .generations
        .cancel_session(project_id, session_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    let mut generation_idle = state
        .generations
        .wait_for_session_idle(project_id, session_id, wait)
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    if !generation_idle {
        let failures = state
            .generations
            .terminal_persistence_failures(project_id, session_id)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
        for failure in failures {
            terminalize_open_runs(state, &failure.identity, &failure.runs, &failure.error)
                .and_then(|_| {
                    release_family_after_terminal_persistence(
                        state,
                        &failure.identity,
                        &failure.runs,
                    )
                })
                .map_err(|error| {
                    IpcFailure::new(
                        "generation_terminal_persistence_failed",
                        format!(
                            "Loom could not preserve a terminal record before closing: {}",
                            error.message
                        ),
                        true,
                    )
                })?;
        }
        generation_idle = state
            .generations
            .wait_for_session_idle(project_id, session_id, Duration::ZERO)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
    }
    if !generation_idle {
        return Err(IpcFailure::new(
            "generation_cancellation_in_progress",
            "Loom requested cancellation and is still preserving terminal generation evidence; retry the same close command shortly",
            true,
        ));
    }
    Ok(())
}

#[tauri::command]
async fn project_current(state: State<'_, PluginState>) -> Result<ProjectSnapshot, IpcFailure> {
    let session = lock_session(&state)?;
    if session.phase != SessionPhase::Open {
        return Err(IpcFailure::new(
            "project_not_open",
            "there is no live native project session to reattach",
            false,
        ));
    }
    let session_id = session.active_session_id.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its session ID",
            false,
        )
    })?;
    let store = session.store.as_ref().ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its store",
            false,
        )
    })?;
    snapshot_for(store, session_id)
}

#[tauri::command]
async fn project_recover(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<RecoveryReport, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let report = store.recover().map_err(IpcFailure::store)?;
    Ok(RecoveryReport {
        recovered: report.applied + report.already_applied,
        conflicts: report
            .conflicts
            .into_iter()
            .map(|conflict| conflict.relative_path)
            .collect(),
    })
}

#[tauri::command]
async fn document_open(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    state: State<'_, PluginState>,
) -> Result<OpenDocument, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let document = store
        .read_document(&relative_path)
        .map_err(IpcFailure::store)?;
    ensure_document_id(&document, &document_id)?;
    let mut draft = store
        .load_transient_draft(&relative_path)
        .map_err(IpcFailure::store)?;
    if let Some(existing) = &draft
        && existing.document_id == document.document_id
        && existing.kind == document.kind
        && existing.blob_id == document.blob_id
    {
        store
            .clear_transient_draft(&relative_path, existing.version)
            .map_err(IpcFailure::store)?;
        draft = None;
    }
    Ok(open_document_from(document, draft))
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
async fn document_checkpoint(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    text: String,
    kind: DocumentKind,
    expected_revision_id: Option<String>,
    expected_visible_blob_id: String,
    command_id: String,
    draft_version: Option<String>,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let stored_document = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .find(|candidate| candidate.relative_path == relative_path)
        .ok_or_else(|| {
            IpcFailure::new(
                "document_not_found",
                "the requested document is not registered in this project",
                false,
            )
        })?;
    ensure_document_identity(&stored_document.document_id.to_string(), &document_id)?;
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_edit_unsupported",
            "hybrid editing is locked until block metadata can be preserved losslessly",
            false,
        ));
    }
    let expected_revision_id = expected_revision_id
        .ok_or_else(|| {
            IpcFailure::new(
                "revision_required",
                "checkpoint refused because the editor did not name its source revision",
                false,
            )
        })?
        .parse()
        .map_err(|_| {
            IpcFailure::new(
                "invalid_revision_id",
                "source revision ID is invalid",
                false,
            )
        })?;
    let expected_visible_blob_id = expected_visible_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "source visible-blob ID is invalid",
            false,
        )
    })?;
    let command_id = command_id.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "checkpoint command ID is not a valid ULID",
            false,
        )
    })?;
    let content = DocumentContent::from_visible(kind, text.into_bytes())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let draft_version = parse_checkpoint_draft_version(draft_version)?;
    let outcome = if let Some(draft_version) = draft_version {
        store.save_document_if_source_idempotent_consuming_draft(
            command_id,
            relative_path,
            content,
            "editor idle checkpoint",
            expected_revision_id,
            expected_visible_blob_id,
            draft_version,
        )
    } else {
        store.save_document_if_source_idempotent(
            command_id,
            relative_path,
            content,
            "editor idle checkpoint",
            expected_revision_id,
            expected_visible_blob_id,
        )
    }
    .map_err(IpcFailure::store)?;
    Ok(Receipt::from(outcome))
}

fn parse_checkpoint_draft_version(
    draft_version: Option<String>,
) -> Result<Option<u64>, IpcFailure> {
    draft_version
        .map(|version| {
            version.parse::<u64>().map_err(|_| {
                IpcFailure::new(
                    "invalid_draft_version",
                    "draft version is not an unsigned decimal integer",
                    false,
                )
            })
        })
        .transpose()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
async fn document_draft_upsert(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    text: String,
    kind: DocumentKind,
    source_revision_id: String,
    expected_version: String,
    state: State<'_, PluginState>,
) -> Result<TransientDraftWriteReceipt, IpcFailure> {
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_edit_unsupported",
            "hybrid drafts are locked until block metadata can be preserved losslessly",
            false,
        ));
    }
    let source_revision_id = source_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "draft source revision ID is invalid",
            false,
        )
    })?;
    let expected_version = expected_version.parse::<u64>().map_err(|_| {
        IpcFailure::new(
            "invalid_draft_version",
            "draft version is not an unsigned decimal integer",
            false,
        )
    })?;
    let content = DocumentContent::from_visible(kind, text.into_bytes())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let canonical_text = String::from_utf8(
        content
            .project_visible()
            .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?
            .bytes,
    )
    .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    ensure_registered_document(store, &relative_path, &document_id)?;
    match store.upsert_transient_draft(
        &relative_path,
        source_revision_id,
        expected_version,
        content,
    ) {
        Ok(outcome) => Ok(transient_draft_write_receipt(
            &outcome.draft,
            outcome.replayed,
        )),
        Err(loom_store::StoreError::TransientDraftVersionConflict { .. }) => {
            let existing = store
                .load_transient_draft(&relative_path)
                .map_err(IpcFailure::store)?;
            match existing {
                Some(draft)
                    if draft.document_id.to_string() == document_id
                        && draft.source_revision_id == source_revision_id
                        && draft.kind == kind
                        && draft.text == canonical_text =>
                {
                    Ok(transient_draft_write_receipt(&draft, true))
                }
                _ => Err(IpcFailure::new(
                    "transient_draft_version_conflict",
                    "a newer transient draft exists; reload it before writing",
                    false,
                )),
            }
        }
        Err(error) => Err(IpcFailure::store(error)),
    }
}

#[tauri::command]
async fn document_draft_clear(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_version: String,
    state: State<'_, PluginState>,
) -> Result<bool, IpcFailure> {
    let expected_version = expected_version.parse::<u64>().map_err(|_| {
        IpcFailure::new(
            "invalid_draft_version",
            "draft version is not an unsigned decimal integer",
            false,
        )
    })?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    ensure_registered_document(store, &relative_path, &document_id)?;
    match store.clear_transient_draft(&relative_path, expected_version) {
        Ok(cleared) => Ok(cleared),
        Err(loom_store::StoreError::TransientDraftVersionConflict { .. }) => {
            if store
                .load_transient_draft(&relative_path)
                .map_err(IpcFailure::store)?
                .is_none()
            {
                Ok(true)
            } else {
                Err(IpcFailure::new(
                    "transient_draft_version_conflict",
                    "a newer transient draft exists and was not cleared",
                    false,
                ))
            }
        }
        Err(error) => Err(IpcFailure::store(error)),
    }
}

#[derive(Debug)]
struct PreviewRequest {
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_revision_id: RevisionId,
    expected_base_blob_id: BlobId,
    app_text: Option<String>,
}

#[derive(Debug)]
struct ApplyRequest {
    document_id: String,
    relative_path: String,
    expected_revision_id: RevisionId,
    expected_base_blob_id: BlobId,
    expected_visible_blob_id: BlobId,
    resolved_content: DocumentContent,
    reason: String,
    command_id: CommandId,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn document_reconciliation_preview(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_revision_id: String,
    expected_base_blob_id: String,
    app_text: Option<String>,
    state: State<'_, PluginState>,
) -> Result<ReconciliationPreview, IpcFailure> {
    let expected_revision_id = expected_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "reconciliation source revision ID is invalid",
            false,
        )
    })?;
    let expected_base_blob_id = expected_base_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "reconciliation base-blob ID is invalid",
            false,
        )
    })?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    reconciliation_preview_for_store(
        store,
        PreviewRequest {
            project_id,
            session_id,
            document_id,
            relative_path,
            expected_revision_id,
            expected_base_blob_id,
            app_text,
        },
    )
}

fn reconciliation_preview_for_store(
    store: &ProjectStore,
    request: PreviewRequest,
) -> Result<ReconciliationPreview, IpcFailure> {
    if store.manifest().project_id.to_string() != request.project_id {
        return Err(IpcFailure::new(
            "project_identity_mismatch",
            "this reconciliation preview does not belong to the open project",
            false,
        ));
    }
    let snapshot = store
        .reconciliation_snapshot(&request.relative_path)
        .map_err(IpcFailure::store)?;
    validate_preview_identity(&snapshot, &request)?;
    if snapshot.kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let visible = snapshot.visible.as_ref().ok_or_else(|| {
        IpcFailure::new(
            "external_file_deleted",
            "the externally edited document was deleted; restore or import it before reconciling",
            false,
        )
    })?;
    if snapshot.visible_matches_active {
        return Err(IpcFailure::new(
            "external_file_unchanged",
            "the visible document still matches the active revision",
            false,
        ));
    }
    ensure_text_hash(
        &snapshot.base_text,
        snapshot.active_blob_id,
        "base_blob_identity_mismatch",
        "the immutable reconciliation base does not match its content identity",
    )?;
    ensure_text_hash(
        &visible.text,
        visible.blob_id,
        "external_file_conflict",
        "the external reconciliation snapshot changed while it was being read",
    )?;

    let draft = store
        .load_transient_draft(&request.relative_path)
        .map_err(IpcFailure::store)?;
    if let Some(draft) = &draft {
        validate_current_draft(draft, &snapshot)?;
    }
    let draft_version = draft.as_ref().map(|draft| draft.version.to_string());
    let (app_candidate, app_source) = match request.app_text {
        Some(text) => (text, ReconciliationAppSource::Caller),
        None => match &draft {
            Some(draft) => (draft.text.clone(), ReconciliationAppSource::TransientDraft),
            None => (snapshot.base_text.clone(), ReconciliationAppSource::Base),
        },
    };

    let base_text = canonical_visible_text(snapshot.kind, &snapshot.base_text)?;
    if base_text != snapshot.base_text {
        return Err(IpcFailure::new(
            "noncanonical_base_document",
            "the immutable base is not canonical for its registered document kind",
            false,
        ));
    }
    let app_text = canonical_visible_text(snapshot.kind, &app_candidate)?;
    let external_visible_text = visible.text.clone();
    let external_text = canonical_visible_text(snapshot.kind, &external_visible_text)?;
    let outcome = three_way_merge(snapshot.kind, &base_text, &app_text, &external_text)
        .map_err(|error| IpcFailure::merge(&error))?;

    Ok(ReconciliationPreview {
        project_id: request.project_id,
        session_id: request.session_id,
        document_id: snapshot.document_id.to_string(),
        relative_path: snapshot.relative_path,
        kind: snapshot.kind,
        active_revision_id: snapshot.active_revision_id.to_string(),
        active_artifact_id: snapshot.active_artifact_id.to_string(),
        base_blob_id: snapshot.active_blob_id.to_string(),
        app_blob_id: BlobId::digest(app_text.as_bytes()).to_string(),
        external_blob_id: BlobId::digest(external_text.as_bytes()).to_string(),
        external_visible_blob_id: visible.blob_id.to_string(),
        base_text,
        app_text,
        external_text,
        external_visible_text,
        app_source,
        draft_version,
        outcome,
    })
}

fn validate_preview_identity(
    snapshot: &DocumentReconciliationSnapshot,
    request: &PreviewRequest,
) -> Result<(), IpcFailure> {
    ensure_document_identity(&snapshot.document_id.to_string(), &request.document_id)?;
    if snapshot.relative_path != request.relative_path {
        return Err(IpcFailure::new(
            "document_path_identity_mismatch",
            "the requested path is not the registered document path",
            false,
        ));
    }
    if snapshot.active_revision_id != request.expected_revision_id {
        return Err(IpcFailure::new(
            "source_revision_conflict",
            "the active revision changed before reconciliation preview",
            false,
        ));
    }
    if snapshot.active_blob_id != request.expected_base_blob_id {
        return Err(IpcFailure::new(
            "source_blob_conflict",
            "the active base blob changed before reconciliation preview",
            false,
        ));
    }
    Ok(())
}

fn validate_current_draft(
    draft: &TransientDraft,
    snapshot: &DocumentReconciliationSnapshot,
) -> Result<(), IpcFailure> {
    if draft.document_id != snapshot.document_id
        || draft.source_revision_id != snapshot.active_revision_id
        || draft.kind != snapshot.kind
        || draft.blob_id != BlobId::digest(draft.text.as_bytes())
    {
        return Err(IpcFailure::new(
            "stale_transient_draft",
            "the recoverable draft is not based on the active reconciliation source",
            false,
        ));
    }
    Ok(())
}

fn ensure_text_hash(
    text: &str,
    expected: BlobId,
    code: &'static str,
    message: &'static str,
) -> Result<(), IpcFailure> {
    if BlobId::digest(text.as_bytes()) != expected {
        return Err(IpcFailure::new(code, message, false));
    }
    Ok(())
}

fn canonical_visible_text(kind: DocumentKind, text: &str) -> Result<String, IpcFailure> {
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let content = DocumentContent::from_visible(kind, text.as_bytes().to_vec())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let bytes = content
        .project_visible()
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?
        .bytes;
    String::from_utf8(bytes)
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn document_reconcile_apply(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_revision_id: String,
    expected_base_blob_id: String,
    expected_external_visible_blob_id: String,
    resolved_text: String,
    kind: DocumentKind,
    reason: String,
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let expected_revision_id = expected_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "reconciliation source revision ID is invalid",
            false,
        )
    })?;
    let expected_base_blob_id = expected_base_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "reconciliation base-blob ID is invalid",
            false,
        )
    })?;
    let expected_visible_blob_id = expected_external_visible_blob_id
        .parse::<BlobId>()
        .map_err(|_| {
            IpcFailure::new(
                "invalid_blob_id",
                "external visible-blob ID is invalid",
                false,
            )
        })?;
    let command_id = command_id.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "reconciliation command ID is not a valid ULID",
            false,
        )
    })?;
    let content = DocumentContent::from_visible(kind, resolved_text.into_bytes())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;

    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    reconcile_apply_for_store(
        store,
        ApplyRequest {
            document_id,
            relative_path,
            expected_revision_id,
            expected_base_blob_id,
            expected_visible_blob_id,
            resolved_content: content,
            reason,
            command_id,
        },
    )
}

fn reconcile_apply_for_store(
    store: &mut ProjectStore,
    request: ApplyRequest,
) -> Result<Receipt, IpcFailure> {
    if request.resolved_content.kind() == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let registered_kind =
        registered_document_kind(store, &request.relative_path, &request.document_id)?;
    if registered_kind != request.resolved_content.kind() {
        return Err(IpcFailure::new(
            "document_kind_mismatch",
            "the reconciliation kind does not match the registered document",
            false,
        ));
    }
    let outcome = store
        .reconcile_external_idempotent(
            request.command_id,
            ExternalReconciliationRequest {
                relative_path: request.relative_path,
                expected_active_revision_id: request.expected_revision_id,
                expected_base_blob_id: request.expected_base_blob_id,
                expected_visible_blob_id: request.expected_visible_blob_id,
                resolved_content: request.resolved_content,
                reason: request.reason,
            },
        )
        .map_err(IpcFailure::store)?;
    Ok(Receipt::from(outcome))
}

// Tauri's command ABI extracts `State` by value; the identity itself is still
// returned directly and cannot fail at runtime.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn build_model_policy_get(state: State<'_, PluginState>) -> BuildModelPolicyIdentity {
    state.build_model_policy.identity()
}

#[tauri::command]
async fn model_list(
    state: State<'_, PluginState>,
) -> Result<Vec<ModelCapabilitySummary>, IpcFailure> {
    let loaded = {
        let registry = lock_model_registry(&state)?;
        match &*registry {
            ModelRegistry::Loaded(model) | ModelRegistry::Unloading(model) => Some(model.clone()),
            ModelRegistry::Loading { previous, .. } => previous.clone(),
            ModelRegistry::ResidencyUnknown { .. } | ModelRegistry::Empty => None,
        }
    };
    let options = desktop_model_discovery_options(&state)?;
    let report = discover_gguf_models(&options)
        .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    let mut models = report
        .models
        .into_iter()
        .map(|model| {
            let model_path = model.resolved_path.to_string_lossy().into_owned();
            let display_name = model
                .selected_path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("Local GGUF")
                .to_owned();
            if let Some(loaded) = loaded
                .as_ref()
                .filter(|loaded| loaded.profile.model_path == model.resolved_path)
            {
                return model_summary(loaded, true, &state.build_model_policy);
            }
            ModelCapabilitySummary {
                model_id: format!("discovered:{}", BlobId::digest(model_path.as_bytes())),
                display_name,
                local: true,
                loaded: false,
                chat: false,
                // A GGUF header proves only the container. Completion and
                // token capabilities stay unavailable until native inspection.
                completion: false,
                fill_in_middle: false,
                output_tokens: false,
                logprobs: false,
                model_path,
                file_bytes: model.file_bytes,
                header_verified: matches!(model.header, GgufHeaderStatus::Verified),
                architecture: None,
                context_tokens: None,
                model_sha256: None,
                projector_present: None,
                media_kinds: Vec::new(),
                policy_candidate: policy_candidate_summary(
                    &state.build_model_policy,
                    model.file_bytes,
                ),
                policy_verified: None,
                tested_profile: None,
            }
        })
        .collect::<Vec<_>>();
    if let Some(loaded) = &loaded
        && !models.iter().any(|model| model.loaded)
    {
        models.push(model_summary(loaded, true, &state.build_model_policy));
    }
    models.sort_by(|left, right| {
        right
            .loaded
            .cmp(&left.loaded)
            .then_with(|| {
                left.policy_candidate
                    .as_ref()
                    .map_or(u32::MAX, |candidate| candidate.rank)
                    .cmp(
                        &right
                            .policy_candidate
                            .as_ref()
                            .map_or(u32::MAX, |candidate| candidate.rank),
                    )
            })
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.model_path.cmp(&right.model_path))
    });
    Ok(models)
}

#[tauri::command]
async fn model_choose<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<Option<ModelCapabilitySummary>, IpcFailure> {
    let Some(selected_path) = choose_model_file(&app)? else {
        return Ok(None);
    };
    let report = discover_gguf_models(&ModelDiscoveryOptions {
        hugging_face_cache_roots: Vec::new(),
        user_paths: vec![selected_path],
        max_entries: 1,
        max_depth: 1,
    })
    .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    let selected = report.models.into_iter().next().ok_or_else(|| {
        IpcFailure::new(
            "selected_model_not_gguf",
            "the selected file is not a readable GGUF model",
            false,
        )
    })?;
    if !matches!(selected.header, GgufHeaderStatus::Verified) {
        return Err(IpcFailure::new(
            "model_header_unverified",
            "the selected file does not have a verified GGUF container header",
            false,
        ));
    }
    remember_user_model_path(&state, selected.resolved_path.clone())?;
    let canonical_path = selected.resolved_path.to_string_lossy().into_owned();
    model_list(state)
        .await?
        .into_iter()
        .find(|model| model.model_path == canonical_path)
        .map(Some)
        .ok_or_else(|| {
            IpcFailure::new(
                "selected_model_disappeared",
                "the selected model changed before Loom could add it to the local library",
                true,
            )
        })
}

#[tauri::command]
async fn model_load<R: Runtime>(
    model_path: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ModelCapabilitySummary, IpcFailure> {
    let (model_load, plan) = {
        let application_admission = lock_application_admission(&state, "local model verification")?;
        let model_load = state.model_loads.reserve(&application_admission)?;
        let plan = prepare_model_load(&model_path, &state)?;
        (model_load, plan)
    };
    let (canonical_path, profile) = match plan {
        ModelLoadPlan::Ready(summary) => return Ok(summary),
        ModelLoadPlan::Inspect {
            canonical_path,
            profile,
        } => (canonical_path, profile),
    };
    let worker_app = app.clone();
    let worker_path = canonical_path.clone();
    let worker_profile = profile.clone();
    let cleanup_profile = profile;
    let backend = Arc::clone(&state.backend);
    let worker_guard = model_load.worker_guard();
    tauri::async_runtime::spawn_blocking(move || {
        let _worker_guard = worker_guard;
        let worker_state = worker_app.state::<PluginState>();
        let operation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let inspected = Ok(backend.inspect_model(&worker_profile));
            let descriptor =
                resolve_model_inspection(&worker_state, &worker_path, &worker_profile, inspected)?;
            commit_model_load(
                &worker_state,
                &worker_path,
                LoadedModel {
                    profile: worker_profile,
                    descriptor,
                },
            )
        }));
        match operation {
            Ok(result) => result,
            Err(panic) => {
                let _ = release_staged_model(&worker_state, &worker_path, &cleanup_profile);
                std::panic::resume_unwind(panic);
            }
        }
    })
    .await
    .map_err(|error| {
        IpcFailure::new(
            "model_worker_failed",
            format!("the local model verification worker stopped: {error}"),
            true,
        )
    })?
}

/// Automatically loads only a writer named by the embedded build policy.
///
/// This command hashes the canonical local file through an open identity
/// handle before llama.cpp sees the path. It rechecks that the path still
/// names the same file before and after native inspection, and it requires the
/// native descriptor to report the same digest and size. See
/// `docs/model-policy-loading.md` for the remaining cross-process mutation
/// limit of a path-based native loader.
#[tauri::command]
async fn model_load_policy_candidate<R: Runtime>(
    profile_id: String,
    model_path: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ModelCapabilitySummary, IpcFailure> {
    let (model_load, plan) = {
        let application_admission = lock_application_admission(&state, "local model verification")?;
        let model_load = state.model_loads.reserve(&application_admission)?;
        let plan = prepare_policy_model_load(&profile_id, &model_path, &state)?;
        (model_load, plan)
    };
    let (canonical_path, profile, expectation) = match plan {
        PolicyModelLoadPlan::Ready(summary) => return Ok(summary),
        PolicyModelLoadPlan::Inspect {
            canonical_path,
            profile,
            expectation,
        } => (canonical_path, profile, expectation),
    };
    let worker_app = app.clone();
    let worker_path = canonical_path.clone();
    let worker_profile = profile.clone();
    let cleanup_profile = profile;
    let worker_expectation = expectation.clone();
    let backend = Arc::clone(&state.backend);
    let worker_guard = model_load.worker_guard();
    tauri::async_runtime::spawn_blocking(move || {
        let _worker_guard = worker_guard;
        let worker_state = worker_app.state::<PluginState>();
        let operation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let inspected =
                inspect_preverified_policy_file(&worker_path, &worker_expectation, || {
                    let descriptor = backend
                        .inspect_model(&worker_profile)
                        .map_err(|error| IpcFailure::backend(&error))?;
                    validate_policy_model_descriptor(
                        &descriptor,
                        &worker_path,
                        &worker_expectation,
                    )?;
                    Ok(descriptor)
                });
            let descriptor = resolve_policy_model_inspection(
                &worker_state,
                &worker_path,
                &worker_profile,
                inspected,
            )?;
            commit_model_load(
                &worker_state,
                &worker_path,
                LoadedModel {
                    profile: worker_profile,
                    descriptor,
                },
            )
        }));
        match operation {
            Ok(result) => result,
            Err(panic) => {
                let _ = release_staged_model(&worker_state, &worker_path, &cleanup_profile);
                std::panic::resume_unwind(panic);
            }
        }
    })
    .await
    .map_err(|error| {
        IpcFailure::new(
            "model_worker_failed",
            format!("the policy model verification worker stopped: {error}"),
            true,
        )
    })?
}

fn prepare_policy_model_load(
    profile_id: &str,
    model_path: &str,
    state: &State<'_, PluginState>,
) -> Result<PolicyModelLoadPlan, IpcFailure> {
    // Resolve the profile before touching the path so an unknown policy name
    // cannot be used as a filesystem-probing oracle.
    let expectation = policy_writer_expectation(&state.build_model_policy, profile_id)?;
    let requested = PathBuf::from(model_path);
    let canonical_path = requested.canonicalize().map_err(|error| {
        IpcFailure::new(
            "policy_model_path_error",
            format!("the policy model path cannot be opened: {error}"),
            false,
        )
    })?;
    let discovered = discover_strict_policy_candidate(&canonical_path)?;
    if discovered.file_bytes != expectation.model_file_bytes {
        return Err(IpcFailure::new(
            "policy_model_size_mismatch",
            "the local file size does not match the selected writer policy",
            false,
        ));
    }

    let _lifecycle = lock_model_lifecycle(state)?;
    let mut registry = lock_model_registry(state)?;
    match &*registry {
        ModelRegistry::Loaded(loaded) if loaded.profile.model_path == canonical_path => {
            validate_policy_model_descriptor(&loaded.descriptor, &canonical_path, &expectation)?;
            let summary = model_summary(loaded, true, &state.build_model_policy);
            return Ok(PolicyModelLoadPlan::Ready(summary));
        }
        ModelRegistry::Loading { path, .. } => {
            return Err(IpcFailure::new(
                "model_load_in_progress",
                format!("Loom is already verifying {}", path.display()),
                true,
            ));
        }
        ModelRegistry::Unloading(_) => {
            return Err(IpcFailure::new(
                "model_unload_in_progress",
                "wait for the selected local model to finish unloading",
                true,
            ));
        }
        ModelRegistry::ResidencyUnknown { reason } => {
            return Err(model_residency_unknown(reason));
        }
        ModelRegistry::Loaded(_) | ModelRegistry::Empty => {}
    }
    ensure_no_active_generations(state, "switching local models")?;
    let previous = match std::mem::take(&mut *registry) {
        ModelRegistry::Loaded(previous) => Some(previous),
        ModelRegistry::Empty => None,
        ModelRegistry::Loading { .. }
        | ModelRegistry::Unloading(_)
        | ModelRegistry::ResidencyUnknown { .. } => {
            unreachable!("the loading state was rejected while holding the registry lock")
        }
    };
    *registry = ModelRegistry::Loading {
        path: canonical_path.clone(),
        previous,
    };
    Ok(PolicyModelLoadPlan::Inspect {
        canonical_path: canonical_path.clone(),
        profile: LocalModelProfile::for_gguf(canonical_path),
        expectation,
    })
}

fn policy_writer_expectation(
    policy: &BuildModelPolicy,
    profile_id: &str,
) -> Result<PolicyWriterExpectation, IpcFailure> {
    let ranked = policy.writer_by_profile_id(profile_id).ok_or_else(|| {
        IpcFailure::new(
            "unknown_policy_model_profile",
            "the requested writer profile is not in this build policy",
            false,
        )
    })?;
    let writer = ranked.writer();
    if writer.role() != ModelRole::Writer || writer.prompt_mode() != PromptMode::Completion {
        return Err(IpcFailure::new(
            "unsupported_policy_model_contract",
            "the selected profile is not an accepted raw-completion writer contract",
            false,
        ));
    }
    Ok(PolicyWriterExpectation {
        profile_id: writer.profile_id().to_owned(),
        rank: ranked.rank(),
        role: writer.role(),
        prompt_mode: writer.prompt_mode(),
        model_sha256: writer.model_sha256(),
        model_file_bytes: writer.model_file_bytes(),
    })
}

fn inspect_preverified_policy_file<T>(
    canonical_path: &Path,
    expectation: &PolicyWriterExpectation,
    inspect: impl FnOnce() -> Result<T, IpcFailure>,
) -> Result<T, PolicyInspectionFailure> {
    let verified = VerifiedPolicyFile::open(canonical_path, expectation).map_err(|failure| {
        PolicyInspectionFailure {
            failure,
            native_inspection_started: false,
        }
    })?;
    verified
        .ensure_path_binding()
        .map_err(|failure| PolicyInspectionFailure {
            failure,
            native_inspection_started: false,
        })?;
    let inspected = inspect().map_err(|failure| PolicyInspectionFailure {
        failure,
        native_inspection_started: true,
    })?;
    verified
        .ensure_path_binding()
        .map_err(|failure| PolicyInspectionFailure {
            failure,
            native_inspection_started: true,
        })?;
    Ok(inspected)
}

impl VerifiedPolicyFile {
    fn open(
        canonical_path: &Path,
        expectation: &PolicyWriterExpectation,
    ) -> Result<Self, IpcFailure> {
        ensure_regular_policy_path(canonical_path)?;
        let mut identity = FileIdentityHandle::from_path(canonical_path).map_err(|error| {
            policy_model_io_failure("open the policy model for identity verification", &error)
        })?;
        let stamp =
            policy_file_stamp(&identity.as_file().metadata().map_err(|error| {
                policy_model_io_failure("inspect the opened policy model", &error)
            })?);
        if stamp.len != expectation.model_file_bytes {
            return Err(IpcFailure::new(
                "policy_model_size_mismatch",
                "the opened local file size does not match the selected writer policy",
                false,
            ));
        }
        let digest = hash_policy_model_file(identity.as_file_mut(), expectation.model_file_bytes)?;
        if digest != expectation.model_sha256 {
            return Err(IpcFailure::new(
                "policy_model_digest_mismatch",
                "the local file digest does not match the selected writer policy",
                false,
            ));
        }
        let verified = Self {
            canonical_path: canonical_path.to_path_buf(),
            identity,
            stamp,
        };
        verified.ensure_path_binding()?;
        Ok(verified)
    }

    fn ensure_path_binding(&self) -> Result<(), IpcFailure> {
        ensure_regular_policy_path(&self.canonical_path)?;
        let resolved = self.canonical_path.canonicalize().map_err(|error| {
            policy_model_io_failure("canonicalize the verified policy model", &error)
        })?;
        if resolved != self.canonical_path {
            return Err(policy_model_file_changed());
        }
        let current = FileIdentityHandle::from_path(&self.canonical_path).map_err(|error| {
            policy_model_io_failure("reopen the verified policy model identity", &error)
        })?;
        if current != self.identity {
            return Err(policy_model_file_changed());
        }
        let open_stamp =
            policy_file_stamp(&self.identity.as_file().metadata().map_err(|error| {
                policy_model_io_failure("reinspect the verified open model", &error)
            })?);
        let path_stamp = policy_file_stamp(&current.as_file().metadata().map_err(|error| {
            policy_model_io_failure("reinspect the verified model path", &error)
        })?);
        if open_stamp != self.stamp || path_stamp != self.stamp {
            return Err(policy_model_file_changed());
        }
        Ok(())
    }
}

fn ensure_regular_policy_path(path: &Path) -> Result<(), IpcFailure> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| policy_model_io_failure("inspect the policy model path", &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IpcFailure::new(
            "policy_model_not_regular_file",
            "the policy model path must name one regular file without a final symlink",
            false,
        ));
    }
    Ok(())
}

fn policy_file_stamp(metadata: &Metadata) -> PolicyFileStamp {
    PolicyFileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

fn hash_policy_model_file(file: &mut File, expected_bytes: u64) -> Result<BlobId, IpcFailure> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; POLICY_MODEL_HASH_BUFFER_BYTES];
    let mut observed_bytes = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| policy_model_io_failure("hash the policy model", &error))?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes
            .checked_add(u64::try_from(read).map_err(|_| {
                IpcFailure::new(
                    "policy_model_size_overflow",
                    "the policy model read size does not fit the platform",
                    false,
                )
            })?)
            .ok_or_else(|| {
                IpcFailure::new(
                    "policy_model_size_overflow",
                    "the policy model byte count overflowed",
                    false,
                )
            })?;
        if observed_bytes > expected_bytes {
            return Err(policy_model_file_changed());
        }
        hasher.update(&buffer[..read]);
    }
    if observed_bytes != expected_bytes {
        return Err(policy_model_file_changed());
    }
    Ok(BlobId::from_bytes(hasher.finalize().into()))
}

fn validate_policy_model_descriptor(
    descriptor: &VerifiedModelDescriptor,
    canonical_path: &Path,
    expectation: &PolicyWriterExpectation,
) -> Result<(), IpcFailure> {
    let descriptor_digest = descriptor.model_sha256.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "policy_model_native_identity_mismatch",
            "native inspection returned an invalid model digest",
            false,
        )
    })?;
    if descriptor.model_path != canonical_path
        || descriptor.model_file_bytes != expectation.model_file_bytes
        || descriptor_digest != expectation.model_sha256
    {
        return Err(IpcFailure::new(
            "policy_model_native_identity_mismatch",
            "native inspection did not return the preverified policy model identity",
            false,
        ));
    }
    if expectation.role != ModelRole::Writer
        || expectation.prompt_mode != PromptMode::Completion
        || !descriptor.capabilities.completion_text.is_supported()
        || !descriptor.capabilities.generated_token_ids.is_supported()
    {
        return Err(IpcFailure::new(
            "policy_model_capability_mismatch",
            "native inspection did not prove raw completion and generated-token support required by the writer policy",
            false,
        ));
    }
    // Schema v1 does not encode a chat-support requirement. Do not infer one:
    // a future policy field must be checked here before that contract can be
    // accepted automatically.
    Ok(())
}

fn resolve_policy_model_inspection(
    state: &PluginState,
    canonical_path: &Path,
    profile: &LocalModelProfile,
    inspected: Result<VerifiedModelDescriptor, PolicyInspectionFailure>,
) -> Result<VerifiedModelDescriptor, IpcFailure> {
    match inspected {
        Ok(descriptor) => Ok(descriptor),
        Err(error) => {
            if error.native_inspection_started {
                release_staged_model(state, canonical_path, profile)?;
            } else {
                restore_staged_model(state, canonical_path)?;
            }
            Err(error.failure)
        }
    }
}

fn restore_staged_model(state: &PluginState, path: &Path) -> Result<(), IpcFailure> {
    let mut registry = lock_model_registry(state)?;
    let current = std::mem::take(&mut *registry);
    match current {
        ModelRegistry::Loading {
            path: loading,
            previous,
        } if loading == path => {
            *registry = previous.map_or(ModelRegistry::Empty, ModelRegistry::Loaded);
            Ok(())
        }
        current => {
            *registry = current;
            Err(IpcFailure::new(
                "model_load_state_changed",
                "the selected model changed during policy verification",
                true,
            ))
        }
    }
}

fn policy_model_io_failure(action: &str, error: &std::io::Error) -> IpcFailure {
    IpcFailure::new(
        "policy_model_io_error",
        format!("could not {action}: {error}"),
        true,
    )
}

fn policy_model_file_changed() -> IpcFailure {
    IpcFailure::new(
        "policy_model_file_changed",
        "the policy model changed during identity verification; retry from a stable local file",
        true,
    )
}

fn prepare_model_load(
    model_path: &str,
    state: &State<'_, PluginState>,
) -> Result<ModelLoadPlan, IpcFailure> {
    let requested = PathBuf::from(model_path);
    let canonical = requested.canonicalize().map_err(|error| {
        IpcFailure::new(
            "model_path_error",
            format!("the selected model path cannot be opened: {error}"),
            false,
        )
    })?;
    let discovered = discover_loadable_model(state, &canonical)?;
    let _lifecycle = lock_model_lifecycle(state)?;
    let mut registry = lock_model_registry(state)?;
    match &*registry {
        ModelRegistry::Loaded(loaded) if loaded.profile.model_path == canonical => {
            return Ok(ModelLoadPlan::Ready(model_summary(
                loaded,
                true,
                &state.build_model_policy,
            )));
        }
        ModelRegistry::Loading { path, .. } => {
            return Err(IpcFailure::new(
                "model_load_in_progress",
                format!("Loom is already verifying {}", path.display()),
                true,
            ));
        }
        ModelRegistry::Unloading(_) => {
            return Err(IpcFailure::new(
                "model_unload_in_progress",
                "wait for the selected local model to finish unloading",
                true,
            ));
        }
        ModelRegistry::ResidencyUnknown { reason } => {
            return Err(model_residency_unknown(reason));
        }
        ModelRegistry::Loaded(_) | ModelRegistry::Empty => {}
    }
    ensure_no_active_generations(state, "switching local models")?;
    let previous = match std::mem::take(&mut *registry) {
        ModelRegistry::Loaded(previous) => Some(previous),
        ModelRegistry::Empty => None,
        ModelRegistry::Loading { .. }
        | ModelRegistry::Unloading(_)
        | ModelRegistry::ResidencyUnknown { .. } => {
            unreachable!("the loading state was rejected while holding the registry lock")
        }
    };
    *registry = ModelRegistry::Loading {
        path: canonical.clone(),
        previous,
    };
    Ok(ModelLoadPlan::Inspect {
        canonical_path: canonical,
        profile: LocalModelProfile::for_gguf(discovered.resolved_path),
    })
}

fn resolve_model_inspection(
    state: &State<'_, PluginState>,
    canonical_path: &Path,
    profile: &LocalModelProfile,
    inspected: Result<Result<VerifiedModelDescriptor, LlamaBackendError>, IpcFailure>,
) -> Result<VerifiedModelDescriptor, IpcFailure> {
    Ok(match inspected {
        Ok(Ok(descriptor)) => descriptor,
        Ok(Err(error)) => {
            release_staged_model(state, canonical_path, profile)?;
            return Err(IpcFailure::backend(&error));
        }
        Err(error) => {
            release_staged_model(state, canonical_path, profile)?;
            return Err(error);
        }
    })
}

fn commit_model_load(
    state: &State<'_, PluginState>,
    canonical_path: &Path,
    loaded: LoadedModel,
) -> Result<ModelCapabilitySummary, IpcFailure> {
    let summary = model_summary(&loaded, true, &state.build_model_policy);
    let mut registry = lock_model_registry(state)?;
    match std::mem::take(&mut *registry) {
        ModelRegistry::Loading { path, previous } if path == canonical_path => {
            if let Some(previous) = previous {
                match state.backend.release_model(&previous.profile) {
                    Ok(ModelRelease::Released { .. }) => {}
                    Ok(ModelRelease::NeverAcquired) => {
                        match state.backend.release_model(&loaded.profile) {
                            Ok(ModelRelease::Released { .. }) => {}
                            Ok(ModelRelease::NeverAcquired) => {
                                let reason = "the previous slot was absent and cleanup could not prove access to the verified staged model's native resident slot".to_owned();
                                *registry = ModelRegistry::ResidencyUnknown {
                                    reason: reason.clone(),
                                };
                                return Err(model_residency_unknown(&reason));
                            }
                            Err(cleanup) => {
                                let reason = format!(
                                    "the previous slot was absent and the verified staged model cleanup failed: {cleanup}"
                                );
                                *registry = ModelRegistry::ResidencyUnknown {
                                    reason: reason.clone(),
                                };
                                return Err(model_residency_unknown(&reason));
                            }
                        }
                        let reason =
                            "the previous selected model had no provably accessible native resident slot"
                                .to_owned();
                        *registry = ModelRegistry::ResidencyUnknown {
                            reason: reason.clone(),
                        };
                        return Err(model_residency_unknown(&reason));
                    }
                    Err(error) => {
                        match state.backend.release_model(&loaded.profile) {
                            Ok(ModelRelease::Released { .. }) => {}
                            Ok(ModelRelease::NeverAcquired) => {
                                let reason = format!(
                                    "the previous model release failed ({error}) and cleanup could not prove access to the verified staged model's native resident slot"
                                );
                                *registry = ModelRegistry::ResidencyUnknown {
                                    reason: reason.clone(),
                                };
                                return Err(model_residency_unknown(&reason));
                            }
                            Err(cleanup) => {
                                let reason = format!(
                                    "the previous model release failed ({error}) and the verified staged model cleanup also failed ({cleanup})"
                                );
                                *registry = ModelRegistry::ResidencyUnknown {
                                    reason: reason.clone(),
                                };
                                return Err(model_residency_unknown(&reason));
                            }
                        }
                        *registry = ModelRegistry::Loaded(previous);
                        return Err(IpcFailure::new(
                            "model_release_failed",
                            format!(
                                "the previous local model could not be released safely: {error}"
                            ),
                            true,
                        ));
                    }
                }
            }
            *registry = ModelRegistry::Loaded(Box::new(loaded));
        }
        current => {
            match state.backend.release_model(&loaded.profile) {
                Ok(ModelRelease::Released { .. }) => {}
                Ok(ModelRelease::NeverAcquired) => {
                    let reason = "the model registry changed during verification and cleanup could not prove access to the verified staged model's native resident slot".to_owned();
                    *registry = ModelRegistry::ResidencyUnknown {
                        reason: reason.clone(),
                    };
                    return Err(model_residency_unknown(&reason));
                }
                Err(cleanup) => {
                    let reason = format!(
                        "the model registry changed during verification and the verified staged model cleanup failed: {cleanup}"
                    );
                    *registry = ModelRegistry::ResidencyUnknown {
                        reason: reason.clone(),
                    };
                    return Err(model_residency_unknown(&reason));
                }
            }
            *registry = current;
            return Err(IpcFailure::new(
                "model_load_state_changed",
                "the selected model changed while native verification was running",
                true,
            ));
        }
    }
    Ok(summary)
}

fn model_residency_unknown(reason: &str) -> IpcFailure {
    IpcFailure::new(
        "model_residency_unknown",
        format!(
            "Loom cannot prove that every native model resource was released: {reason}; restart after preserving the manuscript"
        ),
        false,
    )
}

#[tauri::command]
async fn model_unload(state: State<'_, PluginState>) -> Result<ModelUnloadOutcome, IpcFailure> {
    let _application_admission = lock_application_admission(&state, "local model teardown")?;
    let _lifecycle = lock_model_lifecycle(&state)?;
    ensure_no_active_generations(&state, "unloading the local model")?;
    unload_registered_model(&state)
}

fn unload_registered_model(state: &PluginState) -> Result<ModelUnloadOutcome, IpcFailure> {
    let loaded = {
        let mut registry = lock_model_registry(state)?;
        match std::mem::take(&mut *registry) {
            ModelRegistry::Empty => {
                return Ok(ModelUnloadOutcome {
                    model_id: None,
                    resident_slot_released: false,
                });
            }
            loading @ ModelRegistry::Loading { .. } => {
                *registry = loading;
                return Err(IpcFailure::new(
                    "model_load_in_progress",
                    "wait for local model verification to finish before unloading",
                    true,
                ));
            }
            unloading @ ModelRegistry::Unloading(_) => {
                *registry = unloading;
                return Err(IpcFailure::new(
                    "model_unload_in_progress",
                    "wait for the selected local model to finish unloading",
                    true,
                ));
            }
            ModelRegistry::ResidencyUnknown { reason } => {
                let failure = model_residency_unknown(&reason);
                *registry = ModelRegistry::ResidencyUnknown { reason };
                return Err(failure);
            }
            ModelRegistry::Loaded(loaded) => {
                *registry = ModelRegistry::Unloading(loaded.clone());
                loaded
            }
        }
    };
    let release = state.backend.release_model(&loaded.profile);
    let mut registry = lock_model_registry(state)?;
    let current = std::mem::take(&mut *registry);
    let unloading = match current {
        ModelRegistry::Unloading(unloading)
            if unloading.profile.model_path == loaded.profile.model_path =>
        {
            unloading
        }
        current => {
            *registry = current;
            return Err(IpcFailure::new(
                "model_load_state_changed",
                "the selected model changed while native unload was running",
                true,
            ));
        }
    };
    let model_id = unloading.descriptor.stable_model_id.clone();
    match release {
        Ok(ModelRelease::Released { .. }) => {
            *registry = ModelRegistry::Empty;
            Ok(ModelUnloadOutcome {
                model_id: Some(model_id),
                resident_slot_released: true,
            })
        }
        Ok(ModelRelease::NeverAcquired) => {
            let reason =
                "the selected model had no provably accessible native resident slot".to_owned();
            *registry = ModelRegistry::ResidencyUnknown {
                reason: reason.clone(),
            };
            Err(model_residency_unknown(&reason))
        }
        Err(error) => {
            *registry = ModelRegistry::Loaded(unloading);
            Err(IpcFailure::new(
                "model_release_failed",
                format!("the selected local model could not be released safely: {error}"),
                true,
            ))
        }
    }
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
#[tauri::command]
fn model_download_start<R: Runtime>(
    command_id: String,
    url: String,
    file_name: String,
    expected_sha256: String,
    expected_bytes: Option<u64>,
    max_bytes: u64,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ModelDownloadSnapshot, IpcFailure> {
    ensure_application_running(&state, "a model download")?;
    state.download_workers.reap_finished()?;
    let PreparedModelDownload {
        command_id,
        request,
        spec,
    } = prepare_model_download(
        &state,
        &command_id,
        url,
        file_name,
        &expected_sha256,
        expected_bytes,
        max_bytes,
    )?;
    let application_admission = lock_application_admission(&state, "a model download")?;
    let (reservation, snapshot) = state
        .downloads
        .reserve(spec, now_unix_ms())
        .map_err(|error| IpcFailure::model_download_registry(&error))?;
    match reservation {
        ReservationOutcome::Replayed => {
            drop(application_admission);
            return Ok(snapshot);
        }
        ReservationOutcome::Started => {}
    }
    let worker_reservation = state
        .download_workers
        .reserve(command_id, &application_admission)
        .inspect_err(|failure| {
            let _ = state.downloads.fail(
                command_id,
                failure.message.clone(),
                failure.retryable,
                now_unix_ms(),
            );
        })?;
    emit_model_download_snapshot(
        &app,
        &state.downloads,
        "loom://model-download-progress",
        command_id,
        &snapshot,
    );
    let cancellation = state
        .downloads
        .cancellation(command_id)
        .map_err(|error| IpcFailure::model_download_registry(&error))?;
    start_model_download_worker(
        app,
        &state.downloads,
        command_id,
        request,
        &cancellation,
        worker_reservation,
    )?;
    drop(application_admission);
    Ok(snapshot)
}

fn start_model_download_worker<R: Runtime>(
    app: AppHandle<R>,
    downloads: &Arc<ModelDownloadRegistry>,
    command_id: CommandId,
    request: GgufDownloadRequest,
    cancellation: &DownloadCancellation,
    reservation: DownloadWorkerReservation<'_, '_>,
) -> Result<(), IpcFailure> {
    let worker = spawn_model_download(
        app,
        Arc::clone(downloads),
        command_id,
        request,
        cancellation.clone(),
    )
    .map_err(|error| {
        cancellation.cancel();
        let failure = IpcFailure::new(
            "download_worker_spawn_failed",
            format!("the model download worker could not start: {error}"),
            true,
        );
        let _ = downloads.fail(
            command_id,
            failure.message.clone(),
            failure.retryable,
            now_unix_ms(),
        );
        failure
    })?;
    if let Err(DownloadWorkerAttachError { failure, worker }) =
        reservation.attach(worker, cancellation.clone())
    {
        cancellation.cancel();
        let _ = worker.join();
        let _ = downloads.fail(
            command_id,
            failure.message.clone(),
            failure.retryable,
            now_unix_ms(),
        );
        return Err(failure);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_model_download(
    state: &State<'_, PluginState>,
    command_id: &str,
    url: String,
    file_name: String,
    expected_sha256: &str,
    expected_bytes: Option<u64>,
    max_bytes: u64,
) -> Result<PreparedModelDownload, IpcFailure> {
    let command_id = parse_model_download_command_id(command_id)?;
    if url.is_empty() || url.len() > MAX_MODEL_DOWNLOAD_URL_BYTES {
        return Err(IpcFailure::new(
            "invalid_model_download_url",
            format!(
                "model download URL must contain 1 to {MAX_MODEL_DOWNLOAD_URL_BYTES} UTF-8 bytes"
            ),
            false,
        ));
    }
    if max_bytes == 0 || max_bytes > MAX_MODEL_DOWNLOAD_BYTES {
        return Err(IpcFailure::new(
            "invalid_model_download_limit",
            format!(
                "maximum model download size must be between 1 and {MAX_MODEL_DOWNLOAD_BYTES} bytes"
            ),
            false,
        ));
    }
    if expected_bytes.is_some_and(|bytes| bytes == 0 || bytes > max_bytes) {
        return Err(IpcFailure::new(
            "invalid_model_download_size",
            "expected model size must be positive and no larger than the download limit",
            false,
        ));
    }

    let root = state.model_library_root.as_deref().ok_or_else(|| {
        IpcFailure::new(
            "model_library_unavailable",
            "the operating system did not provide an application data directory",
            false,
        )
    })?;
    let library = prepare_model_library(root).map_err(|error| IpcFailure::model_library(&error))?;
    let target_path = model_target_path(&library, &file_name)
        .map_err(|error| IpcFailure::model_library(&error))?;
    let expected_sha256 = Sha256Digest::from_hex(expected_sha256)
        .map_err(|error| IpcFailure::model_download_request(&error))?;
    let mut request =
        GgufDownloadRequest::new(url, target_path.clone(), expected_sha256, max_bytes);
    request.expected_bytes = expected_bytes;
    validate_gguf_download_request(&request)
        .map_err(|error| IpcFailure::model_download_request(&error))?;
    let request_fingerprint = model_download_fingerprint(&request, &file_name);
    Ok(PreparedModelDownload {
        command_id,
        request,
        spec: ModelDownloadSpec {
            command_id,
            request_fingerprint,
            display_name: file_name,
            target_path,
            expected_sha256: expected_sha256.to_string(),
            expected_bytes,
        },
    })
}

fn spawn_model_download<R: Runtime>(
    app: AppHandle<R>,
    downloads: Arc<ModelDownloadRegistry>,
    command_id: CommandId,
    request: GgufDownloadRequest,
    cancellation: DownloadCancellation,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("loom-model-download".to_owned())
        .spawn(move || {
            tauri::async_runtime::block_on(async move {
                let progress_downloads = Arc::clone(&downloads);
                let progress_app = app.clone();
                let result =
                    download_gguf(
                        &request,
                        &cancellation,
                        move |progress| match progress_downloads.record_progress(
                            command_id,
                            progress,
                            now_unix_ms(),
                        ) {
                            Ok(snapshot) => {
                                emit_model_download_snapshot(
                                    &progress_app,
                                    &progress_downloads,
                                    "loom://model-download-progress",
                                    command_id,
                                    &snapshot,
                                );
                                DownloadControl::Continue
                            }
                            Err(_) => DownloadControl::Cancel,
                        },
                    )
                    .await;
                let terminal = match result {
                    Ok(result) => downloads.complete(command_id, &result, now_unix_ms()),
                    Err(error) if error.is_cancelled() => {
                        downloads.finish_cancelled(command_id, now_unix_ms())
                    }
                    Err(error) => downloads.fail(
                        command_id,
                        error.to_string(),
                        error.is_retryable(),
                        now_unix_ms(),
                    ),
                };
                match terminal {
                    Ok(snapshot) => emit_model_download_snapshot(
                        &app,
                        &downloads,
                        "loom://model-download-terminal",
                        command_id,
                        &snapshot,
                    ),
                    Err(error) => eprintln!("Loom model download terminal state failed: {error}"),
                }
            });
        })
}

#[tauri::command]
async fn model_download_cancel<R: Runtime>(
    command_id: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ModelDownloadSnapshot, IpcFailure> {
    let command_id = parse_model_download_command_id(&command_id)?;
    let snapshot = state
        .downloads
        .request_cancel(command_id, now_unix_ms())
        .map_err(|error| IpcFailure::model_download_registry(&error))?;
    if !snapshot.status.is_terminal() {
        emit_model_download_snapshot(
            &app,
            &state.downloads,
            "loom://model-download-progress",
            command_id,
            &snapshot,
        );
    }
    Ok(snapshot)
}

#[tauri::command]
async fn model_download_status(
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<ModelDownloadSnapshot, IpcFailure> {
    let command_id = parse_model_download_command_id(&command_id)?;
    state
        .downloads
        .status(command_id)
        .map_err(|error| IpcFailure::model_download_registry(&error))
}

#[tauri::command]
async fn model_download_list(
    state: State<'_, PluginState>,
) -> Result<Vec<ModelDownloadSnapshot>, IpcFailure> {
    state
        .downloads
        .list()
        .map_err(|error| IpcFailure::model_download_registry(&error))
}

fn parse_model_download_command_id(command_id: &str) -> Result<CommandId, IpcFailure> {
    command_id.parse().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "model download command ID is not a valid ULID",
            false,
        )
    })
}

fn model_download_fingerprint(request: &GgufDownloadRequest, file_name: &str) -> String {
    let mut bytes = Vec::with_capacity(
        request
            .url
            .len()
            .saturating_add(file_name.len())
            .saturating_add(160),
    );
    append_fingerprint_field(&mut bytes, b"loom-model-download-v1");
    append_fingerprint_field(&mut bytes, request.url.as_bytes());
    append_fingerprint_field(&mut bytes, file_name.as_bytes());
    append_fingerprint_field(&mut bytes, request.expected_sha256.to_string().as_bytes());
    append_fingerprint_field(&mut bytes, &request.max_bytes.to_be_bytes());
    match request.expected_bytes {
        Some(expected) => {
            bytes.push(1);
            bytes.extend_from_slice(&expected.to_be_bytes());
        }
        None => bytes.push(0),
    }
    BlobId::digest(&bytes).to_string()
}

fn append_fingerprint_field(target: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(value);
}

fn emit_model_download_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    registry: &ModelDownloadRegistry,
    event: &str,
    command_id: CommandId,
    snapshot: &ModelDownloadSnapshot,
) {
    if app.emit(event, snapshot.clone()).is_err()
        && let Err(error) = registry.record_delivery_failure(command_id)
    {
        eprintln!("Loom model download event reconciliation failed: {error}");
    }
}

fn discover_loadable_model(
    state: &State<'_, PluginState>,
    canonical: &Path,
) -> Result<loom_backend_llama::DiscoveredGguf, IpcFailure> {
    let options = desktop_model_discovery_options(state)?;
    let report = discover_gguf_models(&options)
        .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    let model = report
        .models
        .into_iter()
        .find(|model| model.resolved_path == canonical)
        .ok_or_else(|| {
            IpcFailure::new(
                "model_not_discovered",
                "the selected path is not one of Loom's bounded, local GGUF discoveries",
                false,
            )
        })?;
    if !matches!(model.header, GgufHeaderStatus::Verified) {
        return Err(IpcFailure::new(
            "model_header_unverified",
            "the selected file does not have a verified GGUF container header",
            false,
        ));
    }
    Ok(model)
}

/// Reopens one exact path for the closed build-policy loader.
///
/// Unlike manual loading, this does not depend on the process-local path
/// registry: a verified choice must survive an application restart. The path
/// still cannot become a writer merely by naming a GGUF. The caller binds it
/// to an embedded profile first, then enforces exact file size, a SHA-256 over
/// an open identity handle, unchanged path identity, native inspection, and
/// the required raw-completion capabilities before committing residency.
fn discover_strict_policy_candidate(
    canonical: &Path,
) -> Result<loom_backend_llama::DiscoveredGguf, IpcFailure> {
    let report = discover_gguf_models(&ModelDiscoveryOptions {
        hugging_face_cache_roots: Vec::new(),
        user_paths: vec![canonical.to_path_buf()],
        max_entries: 1,
        max_depth: 1,
    })
    .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    let model = report.models.into_iter().next().ok_or_else(|| {
        IpcFailure::new(
            "policy_model_not_found",
            "the remembered writing model is no longer a readable local GGUF file",
            false,
        )
    })?;
    if model.resolved_path != canonical || !matches!(model.header, GgufHeaderStatus::Verified) {
        return Err(IpcFailure::new(
            "policy_model_header_unverified",
            "the remembered writing model no longer has the expected local GGUF identity",
            false,
        ));
    }
    Ok(model)
}

fn desktop_model_discovery_options(
    state: &State<'_, PluginState>,
) -> Result<ModelDiscoveryOptions, IpcFailure> {
    let mut options = ModelDiscoveryOptions::default();
    options.max_entries = options.max_entries.min(20_000);
    options.max_depth = options.max_depth.min(12);
    if let Some(path) = std::env::var_os("LOOM_GGUF_MODEL_PATH") {
        options.user_paths.push(PathBuf::from(path));
    }
    if let Some(root) = &state.model_library_root {
        options.user_paths.push(root.join("models"));
    }
    options.user_paths.extend(
        state
            .user_model_paths
            .lock()
            .map_err(|_| {
                IpcFailure::new(
                    "model_path_registry_poisoned",
                    "the selected-model registry entered an invalid state; restart Loom",
                    false,
                )
            })?
            .iter()
            .cloned(),
    );
    Ok(options)
}

fn remember_user_model_path(
    state: &State<'_, PluginState>,
    path: PathBuf,
) -> Result<(), IpcFailure> {
    state
        .user_model_paths
        .lock()
        .map_err(|_| {
            IpcFailure::new(
                "model_path_registry_poisoned",
                "the selected-model registry entered an invalid state; restart Loom",
                false,
            )
        })?
        .insert(path);
    Ok(())
}

fn release_staged_model(
    state: &PluginState,
    path: &Path,
    profile: &LocalModelProfile,
) -> Result<(), IpcFailure> {
    let release = state.backend.release_model(profile);
    let mut registry = lock_model_registry(state)?;
    let current = std::mem::take(&mut *registry);
    match current {
        ModelRegistry::Loading {
            path: loading,
            previous,
        } if loading == path => match release {
            Ok(ModelRelease::Released { .. }) => {
                *registry = previous.map_or(ModelRegistry::Empty, ModelRegistry::Loaded);
            }
            Ok(ModelRelease::NeverAcquired) => {
                // Native inspection can fail before host acquisition. The
                // runtime's independent ledger proves that this exact staged
                // attempt never became resident, so restoring the previous
                // registry is safe and preserves the useful inspection error.
                *registry = previous.map_or(ModelRegistry::Empty, ModelRegistry::Loaded);
            }
            Err(error) => {
                let reason = format!("rejected staged model cleanup failed: {error}");
                *registry = ModelRegistry::ResidencyUnknown {
                    reason: reason.clone(),
                };
                return Err(model_residency_unknown(&reason));
            }
        },
        current => match release {
            Ok(ModelRelease::Released { .. }) => {
                *registry = current;
                return Err(IpcFailure::new(
                    "model_load_state_changed",
                    "the selected model changed while native verification was running",
                    true,
                ));
            }
            Ok(ModelRelease::NeverAcquired) => {
                let reason = "the model registry changed during rejected-model cleanup and cleanup could not prove access to the staged native resident slot".to_owned();
                *registry = ModelRegistry::ResidencyUnknown {
                    reason: reason.clone(),
                };
                return Err(model_residency_unknown(&reason));
            }
            Err(error) => {
                let reason = format!(
                    "the model registry changed during rejected-model cleanup, which then failed: {error}"
                );
                *registry = ModelRegistry::ResidencyUnknown {
                    reason: reason.clone(),
                };
                return Err(model_residency_unknown(&reason));
            }
        },
    }
    Ok(())
}

fn model_summary(
    model: &LoadedModel,
    header_verified: bool,
    build_model_policy: &BuildModelPolicy,
) -> ModelCapabilitySummary {
    let policy_verified = build_model_policy
        .matching_writer(
            &model.descriptor.model_sha256,
            model.descriptor.model_file_bytes,
        )
        .map(|matched| PolicyProfileSummary {
            profile_id: matched.writer().profile_id().to_owned(),
            rank: matched.rank(),
        });
    ModelCapabilitySummary {
        model_id: model.descriptor.stable_model_id.clone(),
        display_name: model.descriptor.display_name.clone(),
        local: true,
        loaded: true,
        chat: model.descriptor.capabilities.chat.is_supported(),
        completion: model.descriptor.capabilities.completion_text.is_supported(),
        fill_in_middle: model
            .descriptor
            .capabilities
            .fill_in_middle_contract_id
            .is_some(),
        output_tokens: model
            .descriptor
            .capabilities
            .generated_token_ids
            .is_supported(),
        logprobs: !model
            .descriptor
            .capabilities
            .log_probability_stages
            .is_empty(),
        model_path: model.profile.model_path.to_string_lossy().into_owned(),
        file_bytes: model.descriptor.model_file_bytes,
        header_verified,
        architecture: model.descriptor.architecture.clone(),
        context_tokens: Some(model.descriptor.context_tokens),
        model_sha256: Some(model.descriptor.model_sha256.clone()),
        projector_present: Some(model.descriptor.projector_sha256.is_some()),
        media_kinds: model
            .descriptor
            .capabilities
            .media
            .iter()
            .map(|media| match media.kind {
                loom_backend_llama::VerifiedMediaKind::Image => "image",
                loom_backend_llama::VerifiedMediaKind::Audio => "audio",
            })
            .collect(),
        // Once native inspection has produced an exact digest, a size-only
        // hint has served its purpose and must not survive a mismatch.
        policy_candidate: None,
        tested_profile: policy_verified
            .as_ref()
            .map(|profile| profile.profile_id.clone()),
        policy_verified,
    }
}

fn policy_candidate_summary(
    policy: &BuildModelPolicy,
    model_file_bytes: u64,
) -> Option<PolicyProfileSummary> {
    policy
        .unverified_size_candidate(model_file_bytes)
        .map(|candidate| PolicyProfileSummary {
            profile_id: candidate.writer().profile_id().to_owned(),
            rank: candidate.rank(),
        })
}

#[tauri::command]
async fn branch_page(
    project_id: String,
    session_id: String,
    document_id: String,
    after: Option<BranchCursorSnapshot>,
    limit: u32,
    state: State<'_, PluginState>,
) -> Result<BranchPageSnapshot, IpcFailure> {
    let document_id = document_id.parse::<DocumentId>().map_err(|_| {
        IpcFailure::new(
            "invalid_document_id",
            "document ID is not a valid ULID",
            false,
        )
    })?;
    let after = after.map(BranchPageCursor::try_from).transpose()?;
    let limit = usize::try_from(limit).map_err(|_| {
        IpcFailure::new(
            "invalid_branch_page_limit",
            "branch page limit does not fit this platform",
            false,
        )
    })?;
    let page = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        store
            .branch_page(document_id, after, limit)
            .map_err(IpcFailure::store)?
    };
    let branches = page
        .branches
        .into_iter()
        .map(|summary| {
            let active = state
                .generations
                .route_for_run(summary.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_summary_snapshot(summary, active))
        })
        .collect::<Result<Vec<_>, IpcFailure>>()?;
    Ok(BranchPageSnapshot {
        branches,
        next_cursor: page.next_cursor.map(BranchCursorSnapshot::from),
        has_more: page.has_more,
    })
}

#[tauri::command]
async fn branch_get(
    project_id: String,
    session_id: String,
    document_id: String,
    run_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<BranchSummarySnapshot>, IpcFailure> {
    let document_id = parse_document_id(&document_id)?;
    let run_id = parse_generation_run_id(&run_id)?;
    let summary = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        store
            .branch_summary(document_id, run_id)
            .map_err(IpcFailure::store)?
    };
    summary
        .map(|summary| {
            let active = state
                .generations
                .route_for_run(summary.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_summary_snapshot(summary, active))
        })
        .transpose()
}

#[tauri::command]
async fn branch_body(
    project_id: String,
    session_id: String,
    document_id: String,
    run_id: String,
    max_bytes: u32,
    state: State<'_, PluginState>,
) -> Result<Option<BranchBodySnapshot>, IpcFailure> {
    let document_id = parse_document_id(&document_id)?;
    let run_id = parse_generation_run_id(&run_id)?;
    let (summary, body) = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        let summary = store
            .branch_summary(document_id, run_id)
            .map_err(IpcFailure::store)?;
        let body = store
            .branch_body(document_id, run_id, u64::from(max_bytes))
            .map_err(IpcFailure::store)?;
        (summary, body)
    };
    match body {
        None => Ok(None),
        Some(body) => {
            let summary = summary.ok_or_else(|| {
                IpcFailure::new(
                    "corrupt_branch_body_identity",
                    "the stored branch body has no immutable branch occurrence",
                    false,
                )
            })?;
            branch_body_snapshot(body, summary).map(Some)
        }
    }
}

fn branch_snapshot(record: StoredBranchRecord, active: bool) -> BranchSnapshot {
    BranchSnapshot {
        run_id: record.run_id.to_string(),
        branch_id: record.branch_id.to_string(),
        document_id: record.document_id.to_string(),
        candidate_id: record.candidate_id.map(|id| id.to_string()),
        source_revision_id: record.source_revision_id.to_string(),
        target_start_byte: record.target_range.start,
        target_end_byte: record.target_range.end,
        text: record.output_text.unwrap_or_default(),
        output_blob_id: record.output_blob_id.map(|id| id.to_string()),
        output_byte_len: record.output_byte_len,
        status: branch_status(record.status, active),
        seed: record.seed.to_string(),
        model_id: record.model_identifier,
        selection: record.selection.map(selection_label),
        error: record.error,
        error_truncated: false,
        created_at_unix_ms: record.created_at_ms,
    }
}

fn branch_summary_snapshot(summary: StoredBranchSummary, active: bool) -> BranchSummarySnapshot {
    BranchSummarySnapshot {
        run_id: summary.run_id.to_string(),
        branch_id: summary.branch_id.to_string(),
        document_id: summary.document_id.to_string(),
        candidate_id: summary.candidate_id.map(|id| id.to_string()),
        source_revision_id: summary.source_revision_id.to_string(),
        target_start_byte: summary.target_range.start,
        target_end_byte: summary.target_range.end,
        output_blob_id: summary.output_blob_id.map(|id| id.to_string()),
        output_byte_len: summary.output_byte_len,
        status: branch_status(summary.status, active),
        seed: summary.seed.map(|seed| seed.to_string()),
        model_id: summary.model_identifier,
        selection: summary.selection.map(selection_label),
        error: summary.error,
        error_truncated: summary.error_truncated,
        created_at_unix_ms: summary.created_at_ms,
    }
}

fn branch_body_snapshot(
    body: StoredBranchBody,
    summary: StoredBranchSummary,
) -> Result<BranchBodySnapshot, IpcFailure> {
    if summary.run_id != body.run_id
        || summary.output_blob_id != Some(body.output_blob_id)
        || summary.output_byte_len != Some(body.byte_len)
    {
        return Err(IpcFailure::new(
            "corrupt_branch_body_identity",
            "the stored branch body does not match its immutable branch occurrence",
            false,
        ));
    }
    let candidate_id = summary.candidate_id.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_branch_body_identity",
            "the stored branch body has no candidate identity",
            false,
        )
    })?;
    let seed = summary.seed.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_branch_body_identity",
            "the stored branch body has no sampler identity",
            false,
        )
    })?;
    let model_id = summary.model_identifier.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_branch_body_identity",
            "the stored branch body has no model identity",
            false,
        )
    })?;
    Ok(BranchBodySnapshot {
        run_id: body.run_id.to_string(),
        branch_id: summary.branch_id.to_string(),
        document_id: summary.document_id.to_string(),
        candidate_id: candidate_id.to_string(),
        source_revision_id: summary.source_revision_id.to_string(),
        target_start_byte: summary.target_range.start,
        target_end_byte: summary.target_range.end,
        seed: seed.to_string(),
        model_id,
        created_at_unix_ms: summary.created_at_ms,
        output_blob_id: body.output_blob_id.to_string(),
        byte_len: body.byte_len,
        text: body.text,
    })
}

fn branch_status(status: StoredBranchStatus, active: bool) -> &'static str {
    if active {
        return "generating";
    }
    match status {
        StoredBranchStatus::Interrupted => "interrupted",
        StoredBranchStatus::Completed => "ready",
        StoredBranchStatus::Cancelled => "cancelled",
        StoredBranchStatus::Failed => "failed",
        StoredBranchStatus::Pruned => "pruned",
        StoredBranchStatus::Rejected => "rejected",
    }
}

const fn selection_label(selection: SelectionDecision) -> &'static str {
    match selection {
        SelectionDecision::KeepAlternative => "keep_alternative",
        SelectionDecision::Promote => "promote",
        SelectionDecision::Reject => "reject",
    }
}

fn branch_records_for_runs(
    store: &ProjectStore,
    document_id: DocumentId,
    run_ids: &[GenerationRunId],
) -> Result<Vec<StoredBranchRecord>, IpcFailure> {
    if run_ids.len() > 4 {
        return Err(IpcFailure::new(
            "generation_provenance_mismatch",
            "a recorded manual Weave family exceeds the four-branch recovery limit",
            false,
        ));
    }
    run_ids
        .iter()
        .map(|&run_id| {
            store
                .branch_record(document_id, run_id, MAX_BRANCH_BODY_BYTES)
                .map_err(IpcFailure::store)?
                .ok_or_else(|| {
                    IpcFailure::new(
                        "generation_provenance_missing",
                        "the recorded Weave family is missing a durable branch projection",
                        false,
                    )
                })
        })
        .collect()
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn replay_weave_if_recorded(
    state: &State<'_, PluginState>,
    project_id: &str,
    session_id: &str,
    command_id: CommandId,
    document_id: DocumentId,
    relative_path: &str,
    source_revision_id: RevisionId,
    expected_visible_blob_id: BlobId,
    cursor_byte: u64,
    policy: &ValidatedWeavePolicy,
) -> Result<Option<WeaveStarted>, IpcFailure> {
    let replay = {
        let mut session = lock_session(state)?;
        let store = require_bound_store(&mut session, project_id, session_id)?;
        let Some(family) = store
            .generation_family_for_command(command_id)
            .map_err(IpcFailure::store)?
        else {
            return Ok(None);
        };
        let document_kind = store
            .list_documents()
            .map_err(IpcFailure::store)?
            .into_iter()
            .find(|document| {
                document.document_id == document_id && document.relative_path == relative_path
            })
            .map(|document| document.kind);
        let resolved = document_kind
            .map(|kind| policy.bind_document_kind(kind))
            .transpose()?;
        let source_bytes = store
            .reconstruct_revision(source_revision_id)
            .map_err(IpcFailure::store)?;
        let cursor = usize::try_from(cursor_byte).map_err(|_| {
            IpcFailure::new(
                "idempotency_conflict",
                "the recorded Weave cursor exceeds this platform's addressable range",
                false,
            )
        })?;
        let source_text = std::str::from_utf8(&source_bytes).map_err(|_| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave source revision is not valid UTF-8",
                false,
            )
        })?;
        let expected_range = ByteRange::new(cursor_byte, cursor_byte).ok_or_else(|| {
            IpcFailure::new(
                "idempotency_conflict",
                "the replayed Weave cursor range is invalid",
                false,
            )
        })?;
        let family_matches = resolved.is_some_and(|resolved| {
            family.generations.len() == resolved.branch_count as usize
                && family.receipt.source_revision_id == Some(source_revision_id)
                && BlobId::digest(&source_bytes) == expected_visible_blob_id
                && cursor <= source_text.len()
                && source_text.is_char_boundary(cursor)
                && family
                    .generations
                    .iter()
                    .enumerate()
                    .all(|(index, started)| {
                        let Ok(case_index) = u32::try_from(index) else {
                            return false;
                        };
                        let (run_id, branch_id) = derive_weave_case_ids(command_id, case_index);
                        let sampling = sampling_for_weave_case(
                            command_id,
                            case_index,
                            resolved.max_tokens,
                            resolved.temperature,
                            resolved.preset,
                        );
                        serde_json::from_value::<SamplingConfig>(
                            started.generation.sampling.clone(),
                        )
                        .is_ok_and(|recorded_sampling| {
                            started.generation.run_id == run_id
                                && started.generation.branch_id == branch_id
                                && started.generation.document_id == document_id
                                && started.generation.source_revision_id == source_revision_id
                                && started.generation.target_range == expected_range
                                && started.generation.seed
                                    == u64::from(generation_seed(
                                        command_id,
                                        case_index,
                                        resolved.preset,
                                    ))
                                && recorded_sampling.fingerprint() == sampling.fingerprint()
                        })
                    })
        });
        if !family_matches {
            return Err(IpcFailure::new(
                "idempotency_conflict",
                "this command ID already identifies a different Weave request",
                false,
            ));
        }

        let run_order = family
            .generations
            .iter()
            .map(|started| started.generation.run_id)
            .collect::<Vec<_>>();
        let ordered_records = branch_records_for_runs(store, document_id, &run_order)?;
        (
            BlobId::digest(&source_text.as_bytes()[..cursor]),
            ordered_records,
        )
    };

    let branches = replay
        .1
        .into_iter()
        .map(|record| {
            let active = state
                .generations
                .route_for_run(record.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_snapshot(record, active))
        })
        .collect::<Result<Vec<_>, IpcFailure>>()?;
    Ok(Some(WeaveStarted {
        command_id: command_id.to_string(),
        request_id: format!("weave-{command_id}"),
        project_id: project_id.to_string(),
        session_id: session_id.to_string(),
        document_id: document_id.to_string(),
        source_revision_id: source_revision_id.to_string(),
        exact_prompt_blob_id: replay.0.to_string(),
        branches,
    }))
}

#[tauri::command]
async fn weave_status(
    project_id: String,
    session_id: String,
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<WeaveStarted>, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let recorded = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        let Some(family) = store
            .generation_family_for_command(command_id)
            .map_err(IpcFailure::store)?
        else {
            return Ok(None);
        };
        let first = family.generations.first().ok_or_else(|| {
            IpcFailure::new(
                "generation_provenance_missing",
                "the recorded Weave command contains no generation runs",
                false,
            )
        })?;
        let document_id = first.generation.document_id;
        let source_revision_id = first.generation.source_revision_id;
        let target = first.generation.target_range;
        if !target.is_empty()
            || family.generations.iter().any(|started| {
                started.generation.document_id != document_id
                    || started.generation.source_revision_id != source_revision_id
                    || started.generation.target_range != target
            })
        {
            return Err(IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave family does not share one exact continuation boundary",
                false,
            ));
        }
        let source_bytes = store
            .reconstruct_revision(source_revision_id)
            .map_err(IpcFailure::store)?;
        let cursor = usize::try_from(target.start).map_err(|_| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave cursor exceeds this platform's addressable range",
                false,
            )
        })?;
        let source_text = std::str::from_utf8(&source_bytes).map_err(|_| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave source revision is not valid UTF-8",
                false,
            )
        })?;
        if cursor > source_text.len() || !source_text.is_char_boundary(cursor) {
            return Err(IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave cursor is not a UTF-8 source boundary",
                false,
            ));
        }
        let run_order = family
            .generations
            .iter()
            .map(|started| started.generation.run_id)
            .collect::<Vec<_>>();
        let records = branch_records_for_runs(store, document_id, &run_order)?;
        (
            document_id,
            source_revision_id,
            BlobId::digest(&source_text.as_bytes()[..cursor]),
            records,
        )
    };
    let branches = recorded
        .3
        .into_iter()
        .map(|record| {
            let active = state
                .generations
                .route_for_run(record.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_snapshot(record, active))
        })
        .collect::<Result<Vec<_>, IpcFailure>>()?;
    Ok(Some(WeaveStarted {
        command_id: command_id.to_string(),
        request_id: format!("weave-{command_id}"),
        project_id,
        session_id,
        document_id: recorded.0.to_string(),
        source_revision_id: recorded.1.to_string(),
        exact_prompt_blob_id: recorded.2.to_string(),
        branches,
    }))
}

#[tauri::command]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn weave_start<R: Runtime>(
    project_id: String,
    session_id: String,
    command_id: String,
    document_id: String,
    relative_path: String,
    source_revision_id: String,
    expected_visible_blob_id: String,
    cursor_byte: u64,
    policy: WeavePolicySnapshot,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<WeaveStarted, IpcFailure> {
    ensure_application_running(&state, "a writing suggestion")?;
    let command_id = parse_command_id(&command_id)?;
    let document_id = document_id.parse::<DocumentId>().map_err(|_| {
        IpcFailure::new(
            "invalid_document_id",
            "document ID is not a valid ULID",
            false,
        )
    })?;
    let source_revision_id = source_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "source revision ID is not a valid ULID",
            false,
        )
    })?;
    let expected_visible_blob_id = expected_visible_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "visible blob ID is not a valid SHA-256 digest",
            false,
        )
    })?;
    let policy = validate_weave_policy(policy)?;

    // Serialize the loaded-model snapshot through native startup and family
    // registration. A switch cannot observe zero active branches in the gap.
    let application_admission = lock_application_admission(&state, "a writing suggestion")?;
    // Replay is read-only recovery, so focus/automation policy does not hide
    // durable private evidence. It is checked while holding the same admission
    // boundary as new work: concurrent first calls cannot both miss the row,
    // and an exact replay never consumes automatic budget or reaches native.
    if let Some(replay) = replay_weave_if_recorded(
        &state,
        &project_id,
        &session_id,
        command_id,
        document_id,
        &relative_path,
        source_revision_id,
        expected_visible_blob_id,
        cursor_byte,
        &policy,
    )? {
        return Ok(replay);
    }
    let _model_lifecycle = lock_model_lifecycle(&state)?;
    let authorized_model =
        AuthorizedWeaveModel::bind(policy, loaded_model(&state)?, &state.build_model_policy)?;
    let branch_count = authorized_model.branch_count();
    let loaded_model = authorized_model.loaded();
    let max_cases = loaded_model
        .descriptor
        .capabilities
        .max_cases
        .min(loaded_model.profile.max_parallel_cases);
    if branch_count > max_cases {
        return Err(IpcFailure::new(
            "model_branch_limit",
            format!("the verified model supports at most {max_cases} parallel branches"),
            false,
        ));
    }
    let model_environment = model_environment_from_verified(&loaded_model.descriptor)
        .map_err(|error| IpcFailure::backend(&error))?;

    let request_id = format!("weave-{command_id}");
    let (identity, exact_prefix, prompt_recipe, cases, queued_branches, runs) = {
        let mut session = lock_session(&state)?;
        authorized_model.admit(&session.agency)?;
        let active_session_id = session.active_session_id.ok_or_else(|| {
            IpcFailure::new(
                "corrupt_project_session",
                "the live project session is missing its session ID",
                false,
            )
        })?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        let loaded = store
            .read_document(&relative_path)
            .map_err(IpcFailure::store)?;
        ensure_document_id(&loaded, &document_id.to_string())?;
        let ResolvedWeavePolicy {
            preset,
            branch_count: resolved_branch_count,
            max_tokens,
            temperature,
        } = authorized_model.bind_document_kind(loaded.kind)?;
        debug_assert_eq!(resolved_branch_count, branch_count);
        if loaded.revision_id != source_revision_id {
            return Err(IpcFailure::new(
                "source_revision_conflict",
                "the manuscript revision changed before generation began",
                false,
            ));
        }
        if loaded.blob_id != expected_visible_blob_id {
            return Err(IpcFailure::new(
                "source_blob_conflict",
                "the visible manuscript bytes changed before generation began",
                false,
            ));
        }
        let cursor = usize::try_from(cursor_byte).map_err(|_| {
            IpcFailure::new(
                "invalid_cursor_boundary",
                "the generation cursor exceeds this platform's addressable range",
                false,
            )
        })?;
        if cursor > loaded.text.len() || !loaded.text.is_char_boundary(cursor) {
            return Err(IpcFailure::new(
                "invalid_cursor_boundary",
                "the generation cursor is not a UTF-8 boundary in the source revision",
                false,
            ));
        }
        if cursor == 0 {
            return Err(IpcFailure::new(
                "empty_completion_prefix",
                "write or place the cursor after at least one manuscript character before weaving",
                false,
            ));
        }
        let automatic_budget_reservation = match authorized_model.automatic_writer() {
            Some(writer) => Some(
                state
                    .automatic_budget
                    .reserve(writer, AutomaticBudgetScope {
                        project: store.manifest().project_id,
                        session: active_session_id,
                        document: document_id,
                        source_revision: source_revision_id,
                    })
                    .map_err(|error| match error {
                        AutomaticBudgetError::Exhausted => IpcFailure::new(
                            "automatic_revision_budget_exhausted",
                            format!(
                                "this immutable manuscript revision has already used its {AUTOMATIC_FAMILY_BUDGET_PER_REVISION_V2} automatic families ({AUTOMATIC_TOKEN_BUDGET_PER_REVISION_V2} generated-token ceiling)",
                            ),
                            false,
                        ),
                        AutomaticBudgetError::Capacity => IpcFailure::new(
                            "automatic_budget_capacity",
                            "the bounded automatic-budget ledger is full; close and reopen the project before requesting more automatic work",
                            false,
                        ),
                        AutomaticBudgetError::Poisoned => IpcFailure::new(
                            "automatic_budget_state_invalid",
                            "automatic generation is unavailable because its budget authority cannot be proven",
                            false,
                        ),
                    })?,
            ),
            None => None,
        };
        let exact_prefix = loaded.text[..cursor].to_owned();
        let exact_prompt_blob_id = store
            .store_provenance_blob(exact_prefix.as_bytes())
            .map_err(IpcFailure::store)?;
        if exact_prompt_blob_id != BlobId::digest(exact_prefix.as_bytes()) {
            return Err(IpcFailure::new(
                "prompt_identity_mismatch",
                "the persisted prompt bytes do not match the exact manuscript prefix",
                false,
            ));
        }
        let environment_artifact = store
            .record_model_environment(&model_environment)
            .map_err(IpcFailure::store)?;
        let prompt_recipe = PromptRecipe {
            mode: PromptMode::Completion,
            exact_prompt_blob_id,
            exact_prompt_token_ids: None,
            ordered_input_artifact_ids: vec![loaded.artifact_id],
            prompt_token_count: None,
        };
        let prompt_artifact = store
            .record_prompt_recipe(&prompt_recipe)
            .map_err(IpcFailure::store)?;
        let context_artifact = store
            .record_context_recipe(&ContextRecipe {
                source_revision_id,
                ordered_source_artifact_ids: Vec::new(),
                token_budget: u64::from(loaded_model.profile.context_tokens),
                retrieval_evidence_blob_id: None,
            })
            .map_err(IpcFailure::store)?;
        let authority_artifact = store
            .record_authority_policy(&AuthorityPolicy {
                policy_version: 1,
                writer_environment_artifact_ids: vec![environment_artifact.artifact_id],
                critic_environment_artifact_ids: Vec::new(),
            })
            .map_err(IpcFailure::store)?;
        let target_range = ByteRange::new(cursor_byte, cursor_byte).ok_or_else(|| {
            IpcFailure::new(
                "invalid_cursor_boundary",
                "the generation target range is invalid",
                false,
            )
        })?;
        let mut cases = Vec::with_capacity(branch_count as usize);
        for index in 0..branch_count {
            let (run_id, branch_id) = derive_weave_case_ids(command_id, index);
            let sampling =
                sampling_for_weave_case(command_id, index, max_tokens, temperature, preset);
            let generation = GenerationStart {
                run_id,
                branch_id,
                document_id,
                source_revision_id,
                target_range,
                model_environment_artifact_id: environment_artifact.artifact_id,
                prompt_recipe_artifact_id: prompt_artifact.artifact_id,
                context_recipe_artifact_id: context_artifact.artifact_id,
                authority_policy_artifact_id: authority_artifact.artifact_id,
                seed: u64::from(sampling.seed),
                sampling: serde_json::Value::Null,
            };
            cases.push(
                ContinuationCase::bind_sampling(generation, sampling).map_err(|error| {
                    IpcFailure::new("sampling_serialize_failed", error.to_string(), false)
                })?,
            );
        }
        let identity = GenerationFamilyIdentity {
            request_id: request_id.clone(),
            project_id: store.manifest().project_id,
            session_id: active_session_id,
            document_id,
        };
        let runs = cases
            .iter()
            .map(|case| (case.generation.run_id, case.generation.branch_id))
            .collect::<Vec<_>>();
        // Reserve cancellable routes while the admission mutex is still held.
        // A concurrent opt-out/Focus/close command can therefore never observe
        // an admitted-but-unregistered family. Cancellation arriving before
        // the native handle is attached is retained by GenerationRegistry.
        state
            .generations
            .reserve(identity.clone(), runs.clone())
            .map_err(|error| IpcFailure::generation_registry(&error))?;
        let family = match store.start_generation_family_with_command(
            command_id,
            cases.iter().map(|case| case.generation.clone()).collect(),
        ) {
            Ok(family) => family,
            Err(error) => {
                let _ = state.generations.complete_family(&request_id);
                return Err(IpcFailure::store(error));
            }
        };
        if let Some(reservation) = automatic_budget_reservation {
            reservation.commit();
        }
        let queued_branches = family
            .generations
            .into_iter()
            .map(|started| BranchSnapshot {
                run_id: started.generation.run_id.to_string(),
                branch_id: started.generation.branch_id.to_string(),
                document_id: started.generation.document_id.to_string(),
                candidate_id: None,
                source_revision_id: started.generation.source_revision_id.to_string(),
                target_start_byte: started.generation.target_range.start,
                target_end_byte: started.generation.target_range.end,
                text: String::new(),
                output_blob_id: None,
                output_byte_len: None,
                status: "queued",
                seed: started.generation.seed.to_string(),
                model_id: model_environment.model_identifier.clone(),
                selection: None,
                error: None,
                error_truncated: false,
                created_at_unix_ms: started.queued_event.occurred_at_ms,
            })
            .collect::<Vec<_>>();
        (
            identity,
            exact_prefix,
            prompt_recipe,
            cases,
            queued_branches,
            runs,
        )
    };
    let exact_prompt_blob_id = BlobId::digest(exact_prefix.as_bytes());
    let result_binding = GenerationResultBinding {
        exact_prompt_blob_id,
        model_environment: model_environment.clone(),
        model: loaded_model.descriptor.clone(),
        generations: cases
            .iter()
            .map(|case| (case.generation.run_id, case.generation.clone()))
            .collect(),
    };
    let native_request = authorized_model.into_exact_continuation_request(
        request_id.clone(),
        exact_prefix,
        prompt_recipe,
        cases,
    );
    let generation_owner = match native_request.submit(&state.backend) {
        Ok(owner) => owner,
        Err(error) => {
            if let Err(persistence) =
                fail_and_release_open_runs(&state, &identity, &runs, &error.to_string(), &app)
            {
                let _ = state
                    .generations
                    .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
                return Err(persistence);
            }
            return Err(IpcFailure::backend(&error));
        }
    };
    let handle = generation_owner.control();
    if let Err(error) = state.generations.attach_cancellation(
        &request_id,
        Arc::new(LlamaCancellation {
            handle: Arc::clone(&handle),
        }),
    ) {
        for (_, branch_id) in &runs {
            let _ = handle.cancel_branch(*branch_id);
        }
        if let Err(persistence) =
            fail_and_release_open_runs(&state, &identity, &runs, &error.to_string(), &app)
        {
            let _ = state
                .generations
                .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
            return Err(persistence);
        }
        return Err(IpcFailure::generation_registry(&error));
    }
    let worker_reservation = match state
        .generation_workers
        .reserve(&request_id, &application_admission)
    {
        Ok(reservation) => reservation,
        Err(error) => {
            for (_, branch_id) in &runs {
                let _ = handle.cancel_branch(*branch_id);
            }
            if let Err(persistence) =
                fail_and_release_open_runs(&state, &identity, &runs, &error.message, &app)
            {
                let _ = state
                    .generations
                    .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
                return Err(persistence);
            }
            return Err(error);
        }
    };
    let worker_app = app.clone();
    let worker_identity = identity.clone();
    let worker_runs = runs.clone();
    let worker_binding = result_binding.clone();
    let worker_handle = Arc::clone(&handle);
    let worker = match std::thread::Builder::new()
        .name("loom-desktop-generation".to_string())
        .spawn(move || {
            run_desktop_generation(
                &worker_app,
                &worker_identity,
                &worker_runs,
                &worker_binding,
                &worker_handle,
            );
        }) {
        Ok(worker) => worker,
        Err(error) => {
            for (_, branch_id) in &runs {
                let _ = handle.cancel_branch(*branch_id);
            }
            if let Err(persistence) =
                fail_and_release_open_runs(&state, &identity, &runs, &error.to_string(), &app)
            {
                let _ = state
                    .generations
                    .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
                return Err(persistence);
            }
            return Err(IpcFailure::new(
                "generation_worker_spawn_failed",
                format!("the desktop generation worker could not start: {error}"),
                true,
            ));
        }
    };
    if let Err(GenerationWorkerAttachError {
        failure,
        worker,
        owner,
    }) = worker_reservation.attach(worker, GenerationWorkerOwner::Llama(generation_owner))
    {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.cancel_all()));
        let desktop_panicked = worker.join().is_err();
        let backend_joined = owner.shutdown_joined();
        let failure = if desktop_panicked || backend_joined.worker_panicked() {
            state.generation_workers.record_join_failure()
        } else {
            failure
        };
        if let Err(persistence) =
            fail_and_release_open_runs(&state, &identity, &runs, &failure.message, &app)
        {
            let _ = state
                .generations
                .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
            return Err(persistence);
        }
        return Err(failure);
    }

    Ok(WeaveStarted {
        command_id: command_id.to_string(),
        request_id,
        project_id,
        session_id,
        document_id: document_id.to_string(),
        source_revision_id: source_revision_id.to_string(),
        exact_prompt_blob_id: exact_prompt_blob_id.to_string(),
        branches: queued_branches,
    })
}

fn generation_seed(command_id: CommandId, index: u32, preset: WeavePreset) -> u32 {
    let material = format!("{command_id}:{index}");
    let digest = BlobId::digest(material.as_bytes());
    let entropy = u32::from_le_bytes(
        digest.as_bytes()[..4]
            .try_into()
            .expect("a SHA-256 digest always contains four seed bytes"),
    );
    // The low two bits are a lossless policy-version tag. For one command and
    // case index, no two current presets can ever share a seed, even if every
    // other sampling field happens to match.
    (entropy & !0b11) | preset.seed_tag()
}

impl WeavePreset {
    const fn seed_tag(self) -> u32 {
        match self {
            Self::AutomaticProseV2 => 0,
            Self::AutomaticVerseV2 => 1,
            Self::ManualV2 => 2,
        }
    }
}

fn validate_weave_policy(policy: WeavePolicySnapshot) -> Result<ValidatedWeavePolicy, IpcFailure> {
    let validated = match policy {
        WeavePolicySnapshot::AutomaticV2 {} => ValidatedWeavePolicy::AutomaticV2,
        WeavePolicySnapshot::ManualV2 {
            branch_count,
            max_tokens,
            temperature,
        } => ValidatedWeavePolicy::ManualV2 {
            branch_count,
            max_tokens,
            temperature,
        },
    };
    if validated.branch_count() == 0 || validated.branch_count() > 4 {
        return Err(IpcFailure::new(
            "invalid_branch_count",
            "a manual Weave must request between one and four branches",
            false,
        ));
    }
    if validated.max_tokens() == 0 || validated.max_tokens() > 2_048 {
        return Err(IpcFailure::new(
            "invalid_generation_budget",
            "a manual Weave must request between one and 2,048 tokens per branch",
            false,
        ));
    }
    if !validated.temperature().is_finite() || !(0.0..=2.0).contains(&validated.temperature()) {
        return Err(IpcFailure::new(
            "invalid_temperature",
            "manual Weave temperature must be a finite value from 0 through 2",
            false,
        ));
    }
    Ok(validated)
}

impl ValidatedWeavePolicy {
    const fn branch_count(&self) -> u32 {
        match self {
            Self::AutomaticV2 => AUTOMATIC_WEAVE_BRANCH_COUNT_V2,
            Self::ManualV2 { branch_count, .. } => *branch_count,
        }
    }

    const fn max_tokens(&self) -> u32 {
        match self {
            Self::AutomaticV2 => AUTOMATIC_WEAVE_MAX_TOKENS_V2,
            Self::ManualV2 { max_tokens, .. } => *max_tokens,
        }
    }

    const fn temperature(&self) -> f32 {
        match self {
            Self::AutomaticV2 => AUTOMATIC_WEAVE_TEMPERATURE_V2,
            Self::ManualV2 { temperature, .. } => *temperature,
        }
    }

    fn bind_document_kind(&self, kind: DocumentKind) -> Result<ResolvedWeavePolicy, IpcFailure> {
        let preset = match (self, kind) {
            (Self::AutomaticV2, DocumentKind::Prose) => WeavePreset::AutomaticProseV2,
            (Self::AutomaticV2, DocumentKind::Verse) => WeavePreset::AutomaticVerseV2,
            (Self::AutomaticV2, DocumentKind::Hybrid) => {
                return Err(IpcFailure::new(
                    "automatic_hybrid_boundary_unresolved",
                    "automatic suggestions require an authoritative prose or verse block boundary",
                    false,
                ));
            }
            (Self::ManualV2 { .. }, _) => WeavePreset::ManualV2,
        };
        Ok(ResolvedWeavePolicy {
            preset,
            branch_count: self.branch_count(),
            max_tokens: self.max_tokens(),
            temperature: self.temperature(),
        })
    }
}

fn sampling_for_weave_case(
    command_id: CommandId,
    index: u32,
    max_tokens: u32,
    temperature: f32,
    preset: WeavePreset,
) -> SamplingConfig {
    let repetition_resistant_prose = preset == WeavePreset::AutomaticProseV2;
    SamplingConfig {
        seed: generation_seed(command_id, index, preset),
        temperature,
        dynamic_temperature_range: 0.0,
        dynamic_temperature_exponent: 1.0,
        top_k: 40,
        top_p: 0.95,
        min_p: 0.0,
        typical_p: 1.0,
        xtc_probability: 0.0,
        xtc_threshold: 0.1,
        repeat_last_n: 64,
        repeat_penalty: if repetition_resistant_prose {
            1.08
        } else {
            1.0
        },
        frequency_penalty: 0.0,
        presence_penalty: 0.0,
        dry_multiplier: if repetition_resistant_prose { 0.8 } else { 0.0 },
        dry_base: 1.75,
        dry_allowed_length: if repetition_resistant_prose { 4 } else { 2 },
        dry_penalty_last_n: if repetition_resistant_prose { 256 } else { -1 },
        sampler_order: vec![
            SamplerKind::Penalties,
            SamplerKind::Dry,
            SamplerKind::TopK,
            SamplerKind::TypicalP,
            SamplerKind::TopP,
            SamplerKind::MinP,
            SamplerKind::Xtc,
            SamplerKind::Temperature,
        ],
        max_tokens,
        stop: Vec::new(),
    }
}

fn loaded_model(state: &State<'_, PluginState>) -> Result<LoadedModel, IpcFailure> {
    loaded_model_for_state(state)
}

fn loaded_model_for_state(state: &PluginState) -> Result<LoadedModel, IpcFailure> {
    let registry = lock_model_registry(state)?;
    match &*registry {
        ModelRegistry::Loaded(model) => Ok((**model).clone()),
        ModelRegistry::Loading { .. } => Err(IpcFailure::new(
            "model_load_in_progress",
            "wait for local model verification to finish before weaving",
            true,
        )),
        ModelRegistry::Unloading(_) => Err(IpcFailure::new(
            "model_unload_in_progress",
            "wait for the selected local model to finish unloading",
            true,
        )),
        ModelRegistry::ResidencyUnknown { reason } => Err(model_residency_unknown(reason)),
        ModelRegistry::Empty => Err(IpcFailure::new(
            "model_not_loaded",
            "load and verify a local raw-completion model before weaving",
            false,
        )),
    }
}

fn run_desktop_generation<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    binding: &GenerationResultBinding,
    handle: &Arc<LlamaGenerationControl>,
) {
    let result = loop {
        match handle.receive_event_timeout(Duration::from_millis(10)) {
            Ok(Some(event)) => {
                if let Err(error) = persist_backend_event(app, identity, event) {
                    for (_, branch_id) in runs {
                        let _ = handle.cancel_branch(*branch_id);
                    }
                    break Err(error.message);
                }
            }
            Ok(None) | Err(LlamaBackendError::ResultDisconnected) => {}
            Err(error) => break Err(error.to_string()),
        }
        match handle.receive_result_timeout(Duration::ZERO) {
            Ok(result) => break Ok(result),
            Err(LlamaBackendError::ResultTimeout) => {}
            Err(error) => break Err(error.to_string()),
        }
    };

    let state = app.state::<PluginState>();
    let persistence = match result {
        Ok(result) => {
            let drained = drain_backend_events(app, identity, handle);
            drained.and_then(|()| persist_generation_result(app, identity, runs, binding, result))
        }
        Err(error) => Err(IpcFailure::new("generation_runtime_failed", error, true)),
    };
    let terminalized = match persistence {
        Ok(()) => Ok(()),
        Err(primary) => fail_open_runs(&state, identity, runs, &primary.message, app).map_err(
            |fallback| {
                IpcFailure::new(
                    "generation_terminal_persistence_failed",
                    format!(
                        "generation result persistence failed: {}; fallback terminal persistence also failed: {}",
                        primary.message, fallback.message
                    ),
                    true,
                )
            },
        ),
    };
    let finalized = terminalized
        .and_then(|()| release_family_after_terminal_persistence(&state, identity, runs));
    if let Err(error) = finalized {
        let _ = state
            .generations
            .mark_terminal_persistence_failure(&identity.request_id, error.message);
    }
}

fn drain_backend_events<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    handle: &LlamaGenerationControl,
) -> Result<(), IpcFailure> {
    loop {
        match handle.receive_event_timeout(Duration::ZERO) {
            Ok(Some(event)) => persist_backend_event(app, identity, event)?,
            Ok(None) | Err(LlamaBackendError::ResultDisconnected) => return Ok(()),
            Err(error) => return Err(IpcFailure::backend(&error)),
        }
    }
}

fn persist_backend_event<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    event: LoomEvent,
) -> Result<(), IpcFailure> {
    let LoomEvent::Generation(event) = event else {
        // Candidate and terminal identities emitted by the backend are
        // provisional. The store mints and emits the only promotable IDs.
        return Ok(());
    };
    if matches!(
        event.kind,
        GenerationEventKind::Queued
            | GenerationEventKind::CandidateReady { .. }
            | GenerationEventKind::CancellationRequested
    ) {
        return Ok(());
    }
    let canonical = {
        let state = app.state::<PluginState>();
        let mut session = lock_session_internal(&state)?;
        let store = require_bound_store(
            &mut session,
            &identity.project_id.to_string(),
            &identity.session_id.to_string(),
        )?;
        store
            .append_generation_event(event.run_id, event.kind)
            .map_err(IpcFailure::store)?
    };
    emit_desktop_event(app, identity, LoomEvent::Generation(canonical))
}

#[allow(clippy::too_many_lines)]
fn persist_generation_result<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    binding: &GenerationResultBinding,
    result: ExactContinuationResult,
) -> Result<(), IpcFailure> {
    let rebuilt_environment = model_environment_from_verified(&result.model)
        .map_err(|error| IpcFailure::backend(&error))?;
    if result.request_id != identity.request_id
        || result.exact_prompt_blob_id != binding.exact_prompt_blob_id
        || BlobId::digest(result.exact_manuscript_prefix.as_bytes()) != binding.exact_prompt_blob_id
        || result.model_environment != binding.model_environment
        || result.model != binding.model
        || rebuilt_environment != binding.model_environment
        || result.candidates.len() != runs.len()
        || binding.generations.len() != runs.len()
    {
        return Err(IpcFailure::new(
            "generation_provenance_mismatch",
            "the native batch result does not match its persisted prompt, model environment, or active branch family",
            false,
        ));
    }
    let expected = runs.iter().copied().collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for (input_index, candidate) in result.candidates.into_iter().enumerate() {
        let expected_branch = expected.get(&candidate.generation.run_id).ok_or_else(|| {
            IpcFailure::new(
                "generation_result_identity_mismatch",
                "the native batch returned an unknown generation run",
                false,
            )
        })?;
        let expected_generation = binding
            .generations
            .get(&candidate.generation.run_id)
            .ok_or_else(|| {
                IpcFailure::new(
                    "generation_result_identity_mismatch",
                    "the native batch returned an unknown persisted generation run",
                    false,
                )
            })?;
        if *expected_branch != candidate.generation.branch_id
            || candidate.generation.document_id != identity.document_id
            || &candidate.generation != expected_generation
            || !seen.insert(candidate.generation.run_id)
        {
            return Err(IpcFailure::new(
                "generation_result_identity_mismatch",
                "the native batch returned a branch under the wrong document identity",
                false,
            ));
        }
        validate_candidate_receipt_binding(
            &candidate,
            &identity.request_id,
            binding.exact_prompt_blob_id,
            binding.model_environment.environment_id,
            &binding.model.local_model_id,
            input_index,
        )
        .map_err(|error| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                format!("native backend receipt validation failed: {error}"),
                false,
            )
        })?;

        let canonical_events = {
            let state = app.state::<PluginState>();
            let mut session = lock_session_internal(&state)?;
            let store = require_bound_store(
                &mut session,
                &identity.project_id.to_string(),
                &identity.session_id.to_string(),
            )?;
            let raw_event_blob = store
                .store_provenance_blob(&candidate.raw_event_stream_bytes)
                .map_err(IpcFailure::store)?;
            if raw_event_blob != candidate.token_trace.raw_event_stream_blob_id {
                return Err(IpcFailure::new(
                    "generation_provenance_mismatch",
                    "the native raw event stream does not match its preserved digest",
                    false,
                ));
            }
            let backend_receipt_blob = store
                .store_provenance_blob(&candidate.backend_receipt_bytes)
                .map_err(IpcFailure::store)?;
            let declared_receipt = candidate
                .token_trace
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.backend_receipt_blob_id)
                .ok_or_else(|| {
                    IpcFailure::new(
                        "generation_provenance_missing",
                        "the native generation did not preserve a backend receipt digest",
                        false,
                    )
                })?;
            if backend_receipt_blob != declared_receipt {
                return Err(IpcFailure::new(
                    "generation_provenance_mismatch",
                    "the native backend receipt does not match its preserved digest",
                    false,
                ));
            }

            match candidate.terminal.status {
                GenerationTerminalStatus::Completed => {
                    let outcome = store
                        .finish_unverified_generation_candidate_for_diagnostics(
                            candidate.generation.run_id,
                            TerminalCandidateInput {
                                output_bytes: candidate.output_text.into_bytes(),
                                token_trace: candidate.token_trace,
                            },
                        )
                        .map_err(IpcFailure::store)?;
                    vec![
                        LoomEvent::Generation(outcome.candidate_ready_event),
                        LoomEvent::GenerationTerminal(outcome.terminal_event),
                    ]
                }
                status @ (GenerationTerminalStatus::Cancelled
                | GenerationTerminalStatus::Pruned
                | GenerationTerminalStatus::Rejected) => {
                    let outcome = store
                        .finish_generation_with_evidence(
                            candidate.generation.run_id,
                            TerminalGenerationInput {
                                status,
                                error: None,
                                evidence: TerminalEvidenceInput {
                                    partial_output_bytes: candidate.output_text.into_bytes(),
                                    token_trace: candidate.token_trace,
                                },
                            },
                        )
                        .map_err(IpcFailure::store)?;
                    vec![LoomEvent::GenerationTerminal(outcome.terminal_event)]
                }
                GenerationTerminalStatus::Failed => {
                    let outcome = store
                        .finish_generation_with_evidence(
                            candidate.generation.run_id,
                            TerminalGenerationInput {
                                status: GenerationTerminalStatus::Failed,
                                error: Some(format!(
                                    "native generation failed: {}",
                                    candidate.finish_reason
                                )),
                                evidence: TerminalEvidenceInput {
                                    partial_output_bytes: candidate.output_text.into_bytes(),
                                    token_trace: candidate.token_trace,
                                },
                            },
                        )
                        .map_err(IpcFailure::store)?;
                    vec![LoomEvent::GenerationTerminal(outcome.terminal_event)]
                }
            }
        };
        for event in canonical_events {
            let _ = emit_desktop_event(app, identity, event);
        }
    }
    Ok(())
}

fn fail_open_runs<R: Runtime>(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    error: &str,
    app: &AppHandle<R>,
) -> Result<(), IpcFailure> {
    let terminals = terminalize_open_runs(state, identity, runs, error)?;
    for terminal in terminals {
        let _ = emit_desktop_event(app, identity, terminal);
    }
    Ok(())
}

fn fail_and_release_open_runs<R: Runtime>(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    error: &str,
    app: &AppHandle<R>,
) -> Result<(), IpcFailure> {
    fail_open_runs(state, identity, runs, error, app)?;
    release_family_after_terminal_persistence(state, identity, runs)
}

fn terminalize_open_runs(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    error: &str,
) -> Result<Vec<LoomEvent>, IpcFailure> {
    let message = if error.trim().is_empty() {
        "local generation failed without an error message".to_string()
    } else {
        error.to_string()
    };
    let mut session = lock_session_internal(state)?;
    let store = require_bound_store(
        &mut session,
        &identity.project_id.to_string(),
        &identity.session_id.to_string(),
    )?;
    let mut terminals = Vec::new();
    for (run_id, _) in runs {
        match store
            .generation_terminal_count(*run_id)
            .map_err(IpcFailure::store)?
        {
            0 => terminals.push(LoomEvent::GenerationTerminal(
                store
                    .finish_generation(
                        *run_id,
                        GenerationTerminalStatus::Failed,
                        Some(message.clone()),
                    )
                    .map_err(IpcFailure::store)?,
            )),
            1 => {}
            count => {
                return Err(IpcFailure::new(
                    "generation_terminal_count_invalid",
                    format!(
                        "generation run {run_id} has {count} terminal events; expected exactly one"
                    ),
                    false,
                ));
            }
        }
    }
    Ok(terminals)
}

fn release_family_after_terminal_persistence(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
) -> Result<(), IpcFailure> {
    {
        let mut session = lock_session_internal(state)?;
        let store = require_bound_store(
            &mut session,
            &identity.project_id.to_string(),
            &identity.session_id.to_string(),
        )?;
        for (run_id, _) in runs {
            let count = store
                .generation_terminal_count(*run_id)
                .map_err(IpcFailure::store)?;
            if count != 1 {
                return Err(IpcFailure::new(
                    "generation_terminal_not_durable",
                    format!("generation run {run_id} has {count} durable terminal events"),
                    true,
                ));
            }
        }
    }
    state
        .generations
        .complete_family(&identity.request_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?
        .ok_or_else(|| {
            IpcFailure::new(
                "generation_family_not_active",
                "the terminalized generation family was not active in the lifecycle registry",
                false,
            )
        })?;
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn generation_cancel<R: Runtime>(
    project_id: String,
    session_id: String,
    command_id: String,
    run_id: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let run_id = run_id.parse::<GenerationRunId>().map_err(|_| {
        IpcFailure::new(
            "invalid_generation_run_id",
            "generation run ID is not a valid ULID",
            false,
        )
    })?;
    let route = state
        .generations
        .route_for_run(run_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?
        .ok_or_else(|| {
            IpcFailure::new(
                "generation_not_active",
                "the requested generation is no longer active",
                false,
            )
        })?;
    if route.identity.project_id.to_string() != project_id
        || route.identity.session_id.to_string() != session_id
    {
        return Err(IpcFailure::new(
            "stale_project_session",
            "this generation belongs to another project session",
            false,
        ));
    }
    let outcome = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        store
            .request_cancel_generation_with_command(command_id, CancelGenerationCommand { run_id })
            .map_err(IpcFailure::store)?
    };
    // Persist the user's request before delivering the process-local side
    // effect. Reaching a terminal state in this interval is benign: a cancel
    // request is not a promise that the terminal status will be Cancelled.
    let _delivered = state
        .generations
        .cancel_run(route.identity.project_id, route.identity.session_id, run_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    emit_desktop_event(
        &app,
        &route.identity,
        LoomEvent::Generation(outcome.event.clone()),
    )?;
    let mut receipt = Receipt::from(outcome.receipt);
    receipt.request_fingerprint = Some(outcome.request_fingerprint.to_string());
    receipt.replayed = outcome.replayed;
    Ok(receipt)
}

#[tauri::command]
async fn candidate_keep(
    project_id: String,
    session_id: String,
    command_id: String,
    candidate_id: String,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let outcome = store
        .keep_alternative_with_command(
            command_id,
            loom_types::KeepAlternativeCommand { candidate_id },
        )
        .map_err(IpcFailure::store)?;
    let mut receipt = Receipt::from(outcome.receipt);
    receipt.request_fingerprint = Some(outcome.request_fingerprint.to_string());
    receipt.replayed = outcome.replayed;
    Ok(receipt)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn candidate_promote(
    project_id: String,
    session_id: String,
    command_id: String,
    candidate_id: String,
    expected_source_revision_id: String,
    expected_visible_blob_id: String,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let expected_source_revision_id =
        expected_source_revision_id
            .parse::<RevisionId>()
            .map_err(|_| {
                IpcFailure::new(
                    "invalid_revision_id",
                    "source revision ID is not a valid ULID",
                    false,
                )
            })?;
    let expected_visible_blob_id = expected_visible_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "visible blob ID is not a valid SHA-256 digest",
            false,
        )
    })?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let outcome = store
        .accept_diagnostic_candidate_with_command(
            command_id,
            loom_types::PromoteCandidateCommand {
                candidate_id,
                expected_source_revision_id,
                expected_visible_blob_id,
            },
        )
        .map_err(IpcFailure::store)?;
    let result_blob_id = outcome.save.blob_id.to_string();
    let request_fingerprint = outcome.request_fingerprint.to_string();
    let replayed = outcome.replayed;
    let visible_projection = outcome.visible_projection;
    let mut receipt = Receipt::from(outcome.save.receipt);
    receipt.result_blob_id = Some(result_blob_id);
    receipt.request_fingerprint = Some(request_fingerprint);
    receipt.replayed = replayed;
    receipt.visible_projection = Some(visible_projection);
    Ok(receipt)
}

/// Imports one controller-produced research packet through a native picker,
/// admits its exact mixed-authorship record into the current store, and stages
/// the resulting live lease for a separate foreground confirmation. The
/// renderer can request the picker but cannot provide packet bytes, a path, a
/// source revision, a command ID, or a live admission lease.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn research_promotion_import<R: Runtime>(
    project_id: String,
    session_id: String,
    window: tauri::Window<R>,
    state: State<'_, PluginState>,
) -> Result<Option<ResearchPromotionPrompt>, IpcFailure> {
    let Some(path) = choose_research_promotion_packet(window.app_handle())? else {
        return Ok(None);
    };
    let focused = window.is_focused().map_err(|error| {
        IpcFailure::new(
            "foreground_focus_unavailable",
            format!("could not verify native window focus after packet selection: {error}"),
            true,
        )
    })?;
    if !focused {
        return Err(IpcFailure::new(
            "foreground_window_not_focused",
            "return focus to the manuscript window before importing a research selection",
            true,
        ));
    }
    let packet = read_research_promotion_packet(&path)?;
    stage_imported_research_promotion(&state, &project_id, &session_id, window.label(), packet)
        .map(Some)
}

fn choose_research_promotion_packet<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Option<PathBuf>, IpcFailure> {
    app.dialog()
        .file()
        .add_filter("Loom research promotion", &["json"])
        .blocking_pick_file()
        .map(|selected| {
            selected.into_path().map_err(|error| {
                IpcFailure::new(
                    "selected_research_packet_unavailable",
                    format!("the selected research packet is not a local filesystem path: {error}"),
                    false,
                )
            })
        })
        .transpose()
}

fn read_research_promotion_packet(path: &Path) -> Result<ResearchPromotionPacket, IpcFailure> {
    let link_metadata = std::fs::symlink_metadata(path).map_err(|error| {
        IpcFailure::new(
            "research_packet_unavailable",
            format!("could not inspect the selected research packet: {error}"),
            false,
        )
    })?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        return Err(IpcFailure::new(
            "research_packet_not_regular_file",
            "the selected research packet must be a regular file, not a symbolic link",
            false,
        ));
    }
    let file = File::open(path).map_err(|error| {
        IpcFailure::new(
            "research_packet_unavailable",
            format!("could not open the selected research packet: {error}"),
            false,
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        IpcFailure::new(
            "research_packet_unavailable",
            format!("could not inspect the opened research packet: {error}"),
            false,
        )
    })?;
    if !metadata.is_file()
        || metadata.len() > u64::try_from(MAX_RESEARCH_PROMOTION_PACKET_BYTES).unwrap_or(u64::MAX)
    {
        return Err(IpcFailure::new(
            "research_packet_too_large",
            format!(
                "the selected research packet exceeds the {MAX_RESEARCH_PROMOTION_PACKET_BYTES} byte limit"
            ),
            false,
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(MAX_RESEARCH_PROMOTION_PACKET_BYTES)
            .min(MAX_RESEARCH_PROMOTION_PACKET_BYTES),
    );
    file.take(
        u64::try_from(MAX_RESEARCH_PROMOTION_PACKET_BYTES)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .map_err(|error| {
        IpcFailure::new(
            "research_packet_unavailable",
            format!("could not read the selected research packet: {error}"),
            false,
        )
    })?;
    if bytes.len() > MAX_RESEARCH_PROMOTION_PACKET_BYTES {
        return Err(IpcFailure::new(
            "research_packet_too_large",
            format!(
                "the selected research packet exceeds the {MAX_RESEARCH_PROMOTION_PACKET_BYTES} byte limit"
            ),
            false,
        ));
    }
    let packet: ResearchPromotionPacket = serde_json::from_slice(&bytes).map_err(|error| {
        IpcFailure::new(
            "research_packet_invalid",
            format!("the selected research packet is not valid Loom research JSON: {error}"),
            false,
        )
    })?;
    if packet.schema != RESEARCH_PROMOTION_PACKET_SCHEMA {
        return Err(IpcFailure::new(
            "research_packet_schema",
            "the selected research packet uses an unsupported schema",
            false,
        ));
    }
    if packet.result_text.len() > MAX_RESEARCH_PROMOTION_PREVIEW_BYTES {
        return Err(IpcFailure::new(
            "research_promotion_preview_too_large",
            "the research result exceeds the bounded foreground review surface",
            false,
        ));
    }
    Ok(packet)
}

fn stage_imported_research_promotion(
    state: &PluginState,
    project_id: &str,
    session_id: &str,
    trusted_window_label: &str,
    packet: ResearchPromotionPacket,
) -> Result<ResearchPromotionPrompt, IpcFailure> {
    let document_id = parse_document_id(&packet.document_id)?;
    let result_bytes = packet.result_text.as_bytes();
    let mixed_assembly_id = packet.record.id();
    let output_blob_id = packet.record.output_blob_id();
    let output_byte_len = packet.record.output_byte_len();
    let (typed_project_id, typed_session_id, source_revision_id, source_blob_id, admission) = {
        let mut session = lock_session(state)?;
        let store = require_bound_store(&mut session, project_id, session_id)?;
        let typed_project_id = store.manifest().project_id;
        let typed_session_id = parse_command_id(session_id)?;
        let summary = store
            .list_documents()
            .map_err(IpcFailure::store)?
            .into_iter()
            .find(|summary| summary.document_id == document_id)
            .ok_or_else(|| {
                IpcFailure::new(
                    "research_promotion_document_missing",
                    "the research packet names a document outside the active project",
                    false,
                )
            })?;
        let source = store
            .read_document(&summary.relative_path)
            .map_err(IpcFailure::store)?;
        let admission = store
            .record_mixed_authorship_assembly(packet.record, result_bytes)
            .map_err(IpcFailure::store)?;
        (
            typed_project_id,
            typed_session_id,
            source.revision_id,
            source.blob_id,
            admission,
        )
    };
    let request = PromotionCommandRequest::new(
        typed_project_id,
        source_revision_id,
        source_blob_id,
        PromotionSubject::MixedAuthorship { mixed_assembly_id },
        admission.admission_record_id().as_blob_id(),
        output_blob_id,
        output_byte_len,
        CommandId::new(),
        now_unix_ms().max(1),
    )
    .map_err(|error| {
        IpcFailure::new(
            "research_promotion_request_invalid",
            error.to_string(),
            false,
        )
    })?;
    state.stage_research_promotion(
        typed_project_id,
        typed_session_id,
        trusted_window_label,
        PendingPromotionSubject::MixedAuthorship(admission),
        request,
    )
}

// Tauri owns these deserialized command arguments and requires value parameters.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn research_promotion_pending(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<ResearchPromotionPrompt>, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let project_id = store.manifest().project_id;
    let session_id = parse_command_id(&session_id)?;
    let now = now_unix_ms();
    let mut pending = state.research_promotions.lock().map_err(|_| {
        IpcFailure::new(
            "research_promotion_state_unavailable",
            "the pending research-promotion registry is unavailable",
            false,
        )
    })?;
    pending.by_command.retain(|_, value| {
        value.challenge.expires_at_unix_ms >= now
            && value.project_id == project_id
            && value.session_id == session_id
    });
    Ok(pending
        .by_command
        .values()
        .map(|value| {
            ResearchPromotionPrompt::from_parts(
                &value.request,
                &value.challenge,
                &value.result_text,
            )
        })
        .collect())
}

// Tauri owns these deserialized command arguments and requires value parameters.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn research_promotion_confirm<R: Runtime>(
    project_id: String,
    session_id: String,
    input: ResearchPromotionConfirmInput,
    window: tauri::Window<R>,
    state: State<'_, PluginState>,
) -> Result<ResearchPromotionResult, IpcFailure> {
    let native_focus = state
        .foreground_commands
        .sample_tauri_window_focus(&window)
        .map_err(|error| {
            IpcFailure::new("foreground_focus_unavailable", error.to_string(), true)
        })?;
    confirm_research_promotion(&state, &project_id, &session_id, native_focus, &input)
}

fn confirm_research_promotion(
    state: &PluginState,
    project_id: &str,
    session_id: &str,
    native_focus: NativeWindowFocusSample,
    input: &ResearchPromotionConfirmInput,
) -> Result<ResearchPromotionResult, IpcFailure> {
    let command_id = parse_command_id(&input.command_id)?;
    let typed_session_id = parse_command_id(session_id)?;
    let mut session = lock_session(state)?;
    let store = require_bound_store(&mut session, project_id, session_id)?;
    let typed_project_id = store.manifest().project_id;
    let pending = state
        .research_promotions
        .lock()
        .map_err(|_| {
            IpcFailure::new(
                "research_promotion_state_unavailable",
                "the pending research-promotion registry is unavailable",
                false,
            )
        })?
        .by_command
        .remove(&command_id)
        .ok_or_else(|| {
            IpcFailure::new(
                "research_promotion_not_pending",
                "this research promotion is no longer awaiting confirmation",
                false,
            )
        })?;
    if pending.project_id != typed_project_id || pending.session_id != typed_session_id {
        return Err(IpcFailure::new(
            "stale_project_session",
            "the research promotion belongs to another project session",
            false,
        ));
    }
    let binding = ForegroundCommandBinding {
        application_session_id: typed_session_id,
        window_id: native_focus.window_id().clone(),
        document_id: parse_document_id(&input.document_id)?,
        candidate_fingerprint: input.candidate_fingerprint.parse().map_err(|_| {
            IpcFailure::new(
                "invalid_candidate_fingerprint",
                "candidate fingerprint is not a valid SHA-256 digest",
                false,
            )
        })?,
        command_id,
        promotion_fingerprint: input.promotion_fingerprint.parse().map_err(|_| {
            IpcFailure::new(
                "invalid_promotion_fingerprint",
                "promotion fingerprint is not a valid SHA-256 digest",
                false,
            )
        })?,
    };
    let command = state
        .foreground_commands
        .consume_with_native_focus(
            loom_host::ForegroundCommandAttempt {
                nonce: parse_command_id(&input.nonce)?,
                binding,
            },
            native_focus,
        )
        .map_err(|error| {
            IpcFailure::new("foreground_command_rejected", error.to_string(), false)
        })?;
    let PendingResearchPromotion {
        request,
        recorded_request,
        subject,
        ..
    } = pending;
    let outcome = store
        .record_foreground_promotion_command(recorded_request, subject.lease(), &request, command)
        .map_err(IpcFailure::store)?;
    let foreground_receipt_blob_id = outcome.foreground_receipt.receipt_blob_id().to_string();
    let mut receipt = Receipt::from(outcome.save.receipt);
    receipt.result_revision_id = Some(outcome.save.revision_id.to_string());
    receipt.result_blob_id = Some(outcome.save.blob_id.to_string());
    receipt.request_fingerprint = Some(request.command_request_fingerprint().to_string());
    receipt.visible_projection = Some(outcome.visible_projection);
    Ok(ResearchPromotionResult {
        receipt,
        foreground_receipt_blob_id,
    })
}

/// Test-only narrow edge used to prove that renderer bytes alone cannot mint
/// foreground authority. The production research command above additionally
/// consumes the live research lease and commits the selected manuscript.
#[cfg(test)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureForegroundCommandInput {
    nonce: String,
    application_session_id: String,
    document_id: String,
    candidate_fingerprint: String,
    command_id: String,
    promotion_fingerprint: String,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct FixtureForegroundCommandReceipt {
    claim: &'static str,
    monotonic_event_index: u64,
}

#[cfg(test)]
#[tauri::command]
#[allow(dead_code, clippy::needless_pass_by_value)]
fn fixture_foreground_command_consume<R: Runtime>(
    input: FixtureForegroundCommandInput,
    window: tauri::Window<R>,
    state: State<'_, PluginState>,
) -> Result<FixtureForegroundCommandReceipt, IpcFailure> {
    let native_focus = state
        .foreground_commands
        .sample_tauri_window_focus(&window)
        .map_err(|error| {
            IpcFailure::new("foreground_focus_unavailable", error.to_string(), true)
        })?;
    consume_fixture_foreground_command(&state, native_focus, &input)
}

#[cfg(test)]
fn consume_fixture_foreground_command(
    state: &PluginState,
    native_focus: NativeWindowFocusSample,
    input: &FixtureForegroundCommandInput,
) -> Result<FixtureForegroundCommandReceipt, IpcFailure> {
    let application_session_id = parse_command_id(&input.application_session_id)?;
    let active_session_id = lock_session_internal(state)?.active_session_id;
    if active_session_id != Some(application_session_id) {
        return Err(IpcFailure::new(
            "stale_project_session",
            "the foreground command belongs to another project session",
            false,
        ));
    }
    let binding = loom_host::ForegroundCommandBinding {
        application_session_id,
        window_id: native_focus.window_id().clone(),
        document_id: input.document_id.parse().map_err(|_| {
            IpcFailure::new(
                "invalid_document_id",
                "document ID is not a valid ULID",
                false,
            )
        })?,
        candidate_fingerprint: input.candidate_fingerprint.parse().map_err(|_| {
            IpcFailure::new(
                "invalid_candidate_fingerprint",
                "candidate fingerprint is not a valid SHA-256 digest",
                false,
            )
        })?,
        command_id: parse_command_id(&input.command_id)?,
        promotion_fingerprint: input.promotion_fingerprint.parse().map_err(|_| {
            IpcFailure::new(
                "invalid_promotion_fingerprint",
                "promotion fingerprint is not a valid SHA-256 digest",
                false,
            )
        })?,
    };
    let command = state
        .foreground_commands
        .consume_with_native_focus(
            loom_host::ForegroundCommandAttempt {
                nonce: parse_command_id(&input.nonce)?,
                binding,
            },
            native_focus,
        )
        .map_err(|error| {
            IpcFailure::new("foreground_command_rejected", error.to_string(), false)
        })?;
    Ok(FixtureForegroundCommandReceipt {
        claim: "trusted_application_host_accepted_one_focused_command",
        monotonic_event_index: command.monotonic_event_index(),
    })
}

fn parse_command_id(value: &str) -> Result<CommandId, IpcFailure> {
    value.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "command ID is not a valid ULID",
            false,
        )
    })
}

fn parse_candidate_id(value: &str) -> Result<CandidateId, IpcFailure> {
    value.parse::<CandidateId>().map_err(|_| {
        IpcFailure::new(
            "invalid_candidate_id",
            "candidate ID is not a valid ULID",
            false,
        )
    })
}

fn parse_document_id(value: &str) -> Result<DocumentId, IpcFailure> {
    value.parse::<DocumentId>().map_err(|_| {
        IpcFailure::new(
            "invalid_document_id",
            "document ID is not a valid ULID",
            false,
        )
    })
}

fn parse_generation_run_id(value: &str) -> Result<GenerationRunId, IpcFailure> {
    value.parse::<GenerationRunId>().map_err(|_| {
        IpcFailure::new(
            "invalid_generation_run_id",
            "generation run ID is not a valid ULID",
            false,
        )
    })
}

fn emit_desktop_event<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    event: LoomEvent,
) -> Result<(), IpcFailure> {
    app.emit(
        "loom://generation",
        DesktopLoomEvent {
            project_id: identity.project_id.to_string(),
            session_id: identity.session_id.to_string(),
            document_id: identity.document_id.to_string(),
            request_id: identity.request_id.clone(),
            event,
        },
    )
    .map_err(|error| {
        IpcFailure::new(
            "generation_event_emit_failed",
            format!("the desktop could not publish generation state: {error}"),
            true,
        )
    })
}

fn prepare_application_exit_request<R: Runtime>(app: &AppHandle<R>) -> bool {
    let Some(state) = app.try_state::<PluginState>() else {
        return false;
    };
    record_application_exit_request(&state)
}

/// `RunEvent::Exit` is the last synchronous boundary before Tauri calls
/// `cleanup_before_exit`. On macOS, Dock Quit and `AppleEvent` Quit can reach
/// this boundary without an interceptable `ExitRequested`, so this fallback
/// owns the same admission barrier and performs all joins on the event-loop
/// thread before AppKit/static teardown may continue.
fn quiesce_unpreventable_runtime_exit<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<PluginState>() else {
        return;
    };
    if state.exit_authorized.load(Ordering::Acquire) {
        return;
    }
    state.close_requested.store(true, Ordering::Release);
    let mut phase = state
        .application
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *phase == ApplicationPhase::ExitAuthorized || state.exit_authorized.load(Ordering::Acquire) {
        return;
    }
    *phase = ApplicationPhase::Closing;

    // Close authority before any potentially slow worker or model drain.
    let _ = state.foreground_commands.revoke_all();
    if let Ok(mut pending) = state.research_promotions.lock() {
        pending.by_command.clear();
    }

    // Owning `phase` is an application-wide admission barrier. Worker
    // reservations borrow an admission guard until their JoinHandle and
    // cancellation authority are attached, making a detached start
    // impossible at this point in safe code.
    let desktop_workers = state.join_desktop_workers_for_exit();
    let _model_lifecycle = state
        .model_lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut model_registry = state
        .model
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let native_runtime = state.native_runtime.shutdown_for_process_exit();
    *model_registry = ModelRegistry::Empty;
    let proof = ApplicationShutdownProof::from_process_exit(native_runtime, desktop_workers);
    let joined = proof.joined_worker_count();
    *phase = ApplicationPhase::ExitAuthorized;
    state.exit_authorized.store(true, Ordering::Release);
    eprintln!("Loom joined {joined} owned worker(s) before unpreventable runtime exit");
}

fn emit_application_close_request<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit("loom://application-close-requested", ());
}

fn record_application_exit_request(state: &PluginState) -> bool {
    if state.exit_authorized.load(Ordering::Acquire) {
        true
    } else {
        // AppKit invokes this path on its event thread. Recording intent must
        // never wait for the admission mutex held while native work starts.
        state.close_requested.store(true, Ordering::Release);
        false
    }
}

fn begin_application_close(state: &PluginState) -> Result<ApplicationCloseAttempt<'_>, IpcFailure> {
    state.close_requested.store(true, Ordering::Release);
    let mut phase = lock_application_phase(state)?;
    match *phase {
        ApplicationPhase::Running => {
            *phase = ApplicationPhase::Closing;
            if let Err(error) = state.foreground_commands.revoke_all() {
                *phase = ApplicationPhase::Running;
                state.close_requested.store(false, Ordering::Release);
                return Err(IpcFailure::new(
                    "foreground_command_state_unavailable",
                    error.to_string(),
                    false,
                ));
            }
            state
                .research_promotions
                .lock()
                .map_err(|_| {
                    *phase = ApplicationPhase::Running;
                    state.close_requested.store(false, Ordering::Release);
                    IpcFailure::new(
                        "research_promotion_state_unavailable",
                        "the pending research-promotion registry is unavailable",
                        false,
                    )
                })?
                .by_command
                .clear();
            Ok(ApplicationCloseAttempt {
                state,
                phase,
                authorized: false,
            })
        }
        ApplicationPhase::Closing => Err(IpcFailure::new(
            "application_close_in_progress",
            "Loom is already proving that native work is safe to close",
            true,
        )),
        ApplicationPhase::ExitAuthorized => Err(IpcFailure::new(
            "application_exit_authorized",
            "Loom has already authorized native process exit",
            false,
        )),
    }
}

fn ensure_application_running(state: &PluginState, action: &str) -> Result<(), IpcFailure> {
    drop(lock_application_admission(state, action)?);
    Ok(())
}

fn lock_application_admission<'a>(
    state: &'a PluginState,
    action: &str,
) -> Result<std::sync::MutexGuard<'a, ApplicationPhase>, IpcFailure> {
    if state.close_requested.load(Ordering::Acquire) {
        return Err(IpcFailure::new(
            "application_quiescing",
            format!("Loom will not start {action} while the application is closing"),
            true,
        ));
    }
    let phase = lock_application_phase(state)?;
    // The second load establishes admission before a later close request, or
    // observes that request and refuses the work while still holding phase.
    if *phase == ApplicationPhase::Running && !state.close_requested.load(Ordering::Acquire) {
        Ok(phase)
    } else {
        Err(IpcFailure::new(
            "application_quiescing",
            format!("Loom will not start {action} while the application is closing"),
            true,
        ))
    }
}

fn lock_application_phase(
    state: &PluginState,
) -> Result<std::sync::MutexGuard<'_, ApplicationPhase>, IpcFailure> {
    state.application.lock().map_err(|_| {
        IpcFailure::new(
            "application_state_poisoned",
            "the application lifecycle entered an invalid state; Loom will not infer safe exit",
            false,
        )
    })
}

impl ApplicationCloseAttempt<'_> {
    fn authorize(mut self, proof: ApplicationShutdownProof) -> ReadyToExit {
        debug_assert_eq!(*self.phase, ApplicationPhase::Closing);
        *self.phase = ApplicationPhase::ExitAuthorized;
        self.state.exit_authorized.store(true, Ordering::Release);
        self.authorized = true;
        ReadyToExit { proof }
    }
}

impl Drop for ApplicationCloseAttempt<'_> {
    fn drop(&mut self) {
        if self.authorized {
            return;
        }
        if *self.phase == ApplicationPhase::Closing {
            *self.phase = ApplicationPhase::Running;
        }
    }
}

fn exit_application<R: Runtime>(app: &AppHandle<R>, permit: ReadyToExit) {
    let ReadyToExit { proof } = permit;
    let _joined_worker_count = proof.joined_worker_count();
    app.exit(0);
}

impl ApplicationShutdownProof {
    fn joined_worker_count(&self) -> usize {
        let native_workers = match &self.native_runtime {
            ApplicationNativeShutdown::Graceful(proof) => proof.joined_worker_count(),
            ApplicationNativeShutdown::ProcessExit(proof) => proof.joined_worker_count(),
        };
        native_workers.saturating_add(self.desktop_workers.joined_worker_count())
    }

    #[cfg(test)]
    fn from_graceful(
        native_runtime: JoinedLlamaRuntime,
        desktop_workers: DesktopWorkersJoined,
    ) -> Self {
        Self {
            native_runtime: ApplicationNativeShutdown::Graceful(native_runtime),
            desktop_workers,
        }
    }

    fn from_process_exit(
        native_runtime: ProcessExitJoinedLlamaRuntime,
        desktop_workers: DesktopWorkersJoined,
    ) -> Self {
        Self {
            native_runtime: ApplicationNativeShutdown::ProcessExit(native_runtime),
            desktop_workers,
        }
    }
}

impl DesktopWorkersJoined {
    fn joined_worker_count(&self) -> usize {
        self.model_loads
            .count()
            .saturating_add(self.generation_workers.joined_worker_count())
            .saturating_add(self.download_workers.count())
    }
}

impl PluginState {
    fn join_desktop_workers(&self) -> Result<DesktopWorkersJoined, IpcFailure> {
        let model_loads = self.model_loads.close_and_drain();
        self.downloads
            .cancel_all_active(now_unix_ms())
            .map_err(|error| IpcFailure::model_download_registry(&error))?;
        let download_workers = self.download_workers.join_all()?;
        let active_downloads = self
            .downloads
            .active_count()
            .map_err(|error| IpcFailure::model_download_registry(&error))?;
        if active_downloads != 0 {
            return Err(IpcFailure::new(
                "model_download_terminal_missing",
                format!(
                    "{active_downloads} joined model download worker(s) failed to record terminal state"
                ),
                false,
            ));
        }
        let generation_workers = self.generation_workers.join_all()?;
        if !model_loads.belongs_to(&self.model_loads)
            || !generation_workers.belongs_to(&self.generation_workers)
            || !download_workers.belongs_to(&self.download_workers)
        {
            return Err(IpcFailure::new(
                "desktop_worker_identity_mismatch",
                "desktop worker shutdown authority came from a different registry instance",
                false,
            ));
        }
        Ok(DesktopWorkersJoined {
            model_loads,
            generation_workers,
            download_workers,
        })
    }

    /// Infallible final event-loop drain. Every worker owning a `JoinHandle` is
    /// removed under a poison-recovering registry lock, cancelled, and joined
    /// before the returned exact-registry facts are assembled.
    fn join_desktop_workers_for_exit(&self) -> DesktopWorkersJoined {
        let model_loads = self.model_loads.close_and_drain();
        // Running worker slots retain the authoritative cancellation handles.
        // Avoid fallible semantic registries at this unpreventable boundary.
        let download_workers = self.download_workers.join_all_for_exit();
        let generation_workers = self.generation_workers.join_all_for_exit();
        DesktopWorkersJoined {
            model_loads,
            generation_workers,
            download_workers,
        }
    }
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn application_close<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let close_attempt = begin_application_close(&state)?;
    if state
        .generations
        .active_branch_count()
        .map_err(|error| IpcFailure::generation_registry(&error))?
        != 0
    {
        return Err(IpcFailure::new(
            "generation_active",
            "cancel or finish every active strand before closing Loom",
            true,
        ));
    }
    {
        let session = lock_session(&state)?;
        if session.phase != SessionPhase::Closed {
            return Err(IpcFailure::new(
                "project_must_close_first",
                "Loom refuses to close the window while a project session is active",
                false,
            ));
        }
    }
    let desktop_workers = state.join_desktop_workers()?;
    let _model_lifecycle = lock_model_lifecycle(&state)?;
    let mut model_registry = lock_model_registry(&state)?;
    ensure_model_registry_ready_for_application_shutdown(&model_registry)?;
    let native_runtime = match state.native_runtime.shutdown_joined() {
        Ok(joined) => ApplicationNativeShutdown::Graceful(joined),
        Err(error) => {
            eprintln!("Loom graceful native shutdown required the process-exit drain: {error}");
            ApplicationNativeShutdown::ProcessExit(state.native_runtime.shutdown_for_process_exit())
        }
    };
    *model_registry = ModelRegistry::Empty;
    let proof = ApplicationShutdownProof {
        native_runtime,
        desktop_workers,
    };
    let permit = close_attempt.authorize(proof);
    exit_application(&app, permit);
    Ok(())
}

fn ensure_model_registry_ready_for_application_shutdown(
    registry: &ModelRegistry,
) -> Result<(), IpcFailure> {
    match registry {
        ModelRegistry::Empty | ModelRegistry::Loaded(_) => Ok(()),
        ModelRegistry::Loading { .. } => Err(IpcFailure::new(
            "model_load_in_progress",
            "wait for local model verification before closing Loom",
            true,
        )),
        ModelRegistry::Unloading(_) => Err(IpcFailure::new(
            "model_unload_in_progress",
            "wait for local model teardown before closing Loom",
            true,
        )),
        ModelRegistry::ResidencyUnknown { reason } => Err(model_residency_unknown(reason)),
    }
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn application_close_abort(state: State<'_, PluginState>) -> Result<(), IpcFailure> {
    abort_application_close(&state)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn application_close_pending(state: State<'_, PluginState>) -> Result<bool, IpcFailure> {
    application_close_is_pending(&state)
}

fn application_close_is_pending(state: &PluginState) -> Result<bool, IpcFailure> {
    if state.close_requested.load(Ordering::Acquire) {
        return Ok(true);
    }
    Ok(*lock_application_phase(state)? != ApplicationPhase::Running)
}

fn abort_application_close(state: &PluginState) -> Result<(), IpcFailure> {
    let phase = match state.application.try_lock() {
        Ok(phase) => phase,
        Err(TryLockError::WouldBlock) => {
            return Err(IpcFailure::new(
                "application_close_in_progress",
                "wait for the current native close proof before resuming Loom",
                true,
            ));
        }
        Err(TryLockError::Poisoned(_)) => {
            return Err(IpcFailure::new(
                "application_state_poisoned",
                "the application lifecycle entered an invalid state; Loom will not infer safe exit",
                false,
            ));
        }
    };
    match *phase {
        ApplicationPhase::Running => {
            state.model_loads.reopen_after_aborted_close()?;
            state.close_requested.store(false, Ordering::Release);
            Ok(())
        }
        ApplicationPhase::Closing => Err(IpcFailure::new(
            "application_close_in_progress",
            "wait for the current native close proof before resuming Loom",
            true,
        )),
        ApplicationPhase::ExitAuthorized => Err(IpcFailure::new(
            "application_exit_authorized",
            "native process exit has already been authorized",
            false,
        )),
    }
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn focus_mode_set(
    project_id: String,
    session_id: String,
    enabled: bool,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let mut session = lock_session(&state)?;
    let project_id_typed = require_bound_store(&mut session, &project_id, &session_id)?
        .manifest()
        .project_id;
    let session_id_typed = session.active_session_id.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its session ID",
            false,
        )
    })?;
    session.agency.set_focus_mode(enabled);
    drop(session);
    if enabled {
        state
            .generations
            .cancel_session(project_id_typed, session_id_typed)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
    }
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn suggestions_set(
    project_id: String,
    session_id: String,
    enabled: bool,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let mut session = lock_session(&state)?;
    let project_id_typed = require_bound_store(&mut session, &project_id, &session_id)?
        .manifest()
        .project_id;
    let session_id_typed = session.active_session_id.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its session ID",
            false,
        )
    })?;
    session.agency.set_automation_enabled(enabled);
    drop(session);
    if !enabled {
        state
            .generations
            .cancel_session(project_id_typed, session_id_typed)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
    }
    Ok(())
}

fn snapshot_for(
    store: &ProjectStore,
    session_id: CommandId,
) -> Result<ProjectSnapshot, IpcFailure> {
    let documents = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .map(|summary| -> Result<DocumentSummary, IpcFailure> {
            let (active_blob_id, word_count, externally_modified) =
                if summary.active_revision_id.is_some() {
                    let reconciliation = store
                        .reconciliation_snapshot(&summary.relative_path)
                        .map_err(IpcFailure::store)?;
                    let word_count = reconciliation
                        .visible
                        .as_ref()
                        .map_or(0, |visible| count_words(&visible.text));
                    (
                        Some(reconciliation.active_blob_id.to_string()),
                        word_count,
                        !reconciliation.visible_matches_active,
                    )
                } else {
                    (None, 0, false)
                };
            Ok(DocumentSummary {
                document_id: summary.document_id.to_string(),
                title: title_for_path(&summary.relative_path),
                relative_path: summary.relative_path,
                kind: summary.kind,
                revision_id: summary.active_revision_id.map(|id| id.to_string()),
                active_blob_id,
                word_count,
                externally_modified,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = store
        .root()
        .to_str()
        .ok_or_else(|| {
            IpcFailure::new(
                "non_utf8_project_path",
                "project path is not valid UTF-8",
                false,
            )
        })?
        .to_owned();
    Ok(ProjectSnapshot {
        project_id: store.manifest().project_id.to_string(),
        session_id: session_id.to_string(),
        title: store.manifest().name.clone(),
        root,
        schema_version: store.manifest().schema_version,
        documents,
        pending_recovery: store.pending_outbox_count().map_err(IpcFailure::store)?,
    })
}

fn open_document_from(document: LoadedDocument, draft: Option<TransientDraft>) -> OpenDocument {
    let word_count = count_words(&document.text);
    OpenDocument {
        visible_blob_id: document.blob_id.to_string(),
        summary: DocumentSummary {
            document_id: document.document_id.to_string(),
            title: title_for_path(&document.relative_path),
            relative_path: document.relative_path,
            kind: document.kind,
            revision_id: Some(document.revision_id.to_string()),
            active_blob_id: Some(document.blob_id.to_string()),
            word_count,
            externally_modified: false,
        },
        text: document.text,
        transient_draft: draft.map(|draft| transient_draft_snapshot(draft, false)),
    }
}

fn transient_draft_snapshot(draft: TransientDraft, replayed: bool) -> TransientDraftSnapshot {
    TransientDraftSnapshot {
        document_id: draft.document_id.to_string(),
        source_revision_id: draft.source_revision_id.to_string(),
        blob_id: draft.blob_id.to_string(),
        version: draft.version.to_string(),
        kind: draft.kind,
        text: draft.text,
        updated_at_unix_ms: draft.updated_at_ms,
        replayed,
    }
}

fn transient_draft_write_receipt(
    draft: &TransientDraft,
    replayed: bool,
) -> TransientDraftWriteReceipt {
    TransientDraftWriteReceipt {
        document_id: draft.document_id.to_string(),
        source_revision_id: draft.source_revision_id.to_string(),
        blob_id: draft.blob_id.to_string(),
        version: draft.version.to_string(),
        kind: draft.kind,
        updated_at_unix_ms: draft.updated_at_ms,
        replayed,
    }
}

fn ensure_document_id(document: &LoadedDocument, expected: &str) -> Result<(), IpcFailure> {
    ensure_document_identity(&document.document_id.to_string(), expected)
}

fn ensure_document_identity(actual: &str, expected: &str) -> Result<(), IpcFailure> {
    if actual != expected {
        return Err(IpcFailure::new(
            "document_identity_mismatch",
            "the document identity does not match the authorized project entry",
            false,
        ));
    }
    Ok(())
}

fn ensure_registered_document(
    store: &ProjectStore,
    relative_path: &str,
    document_id: &str,
) -> Result<(), IpcFailure> {
    registered_document_kind(store, relative_path, document_id).map(|_| ())
}

fn registered_document_kind(
    store: &ProjectStore,
    relative_path: &str,
    document_id: &str,
) -> Result<DocumentKind, IpcFailure> {
    let stored_document = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .find(|candidate| candidate.relative_path == relative_path)
        .ok_or_else(|| {
            IpcFailure::new(
                "document_not_found",
                "the requested document is not registered in this project",
                false,
            )
        })?;
    ensure_document_identity(&stored_document.document_id.to_string(), document_id)?;
    Ok(stored_document.kind)
}

fn lock_session(state: &PluginState) -> Result<std::sync::MutexGuard<'_, Session>, IpcFailure> {
    state.session.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => IpcFailure::new(
            "project_busy",
            "another bounded project operation is still running; retry shortly",
            true,
        ),
        TryLockError::Poisoned(_) => IpcFailure::new(
            "session_poisoned",
            "the project session entered an invalid state; restart Loom",
            false,
        ),
    })
}

fn lock_session_internal(
    state: &PluginState,
) -> Result<std::sync::MutexGuard<'_, Session>, IpcFailure> {
    state.session.lock().map_err(|_| {
        IpcFailure::new(
            "session_poisoned",
            "the project session entered an invalid state; restart Loom",
            false,
        )
    })
}

fn lock_model_registry(
    state: &PluginState,
) -> Result<std::sync::MutexGuard<'_, ModelRegistry>, IpcFailure> {
    state.model.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => IpcFailure::new(
            "model_registry_busy",
            "another bounded model operation is still running; retry shortly",
            true,
        ),
        TryLockError::Poisoned(_) => IpcFailure::new(
            "model_registry_poisoned",
            "the model registry entered an invalid state; restart Loom",
            false,
        ),
    })
}

fn lock_model_lifecycle(state: &PluginState) -> Result<std::sync::MutexGuard<'_, ()>, IpcFailure> {
    state
        .model_lifecycle
        .try_lock()
        .map_err(|error| match error {
            TryLockError::WouldBlock => IpcFailure::new(
                "model_lifecycle_busy",
                "another bounded model lifecycle operation is still running; retry shortly",
                true,
            ),
            TryLockError::Poisoned(_) => IpcFailure::new(
                "model_lifecycle_poisoned",
                "the model lifecycle entered an invalid state; restart Loom",
                false,
            ),
        })
}

fn ensure_no_active_generations(
    state: &State<'_, PluginState>,
    action: &str,
) -> Result<(), IpcFailure> {
    if state
        .generations
        .active_branch_count()
        .map_err(|error| IpcFailure::generation_registry(&error))?
        == 0
    {
        return Ok(());
    }
    Err(IpcFailure::new(
        "generation_active",
        format!("finish or cancel active strands before {action}"),
        true,
    ))
}

fn require_bound_store<'a>(
    session: &'a mut Session,
    project_id: &str,
    session_id: &str,
) -> Result<&'a mut ProjectStore, IpcFailure> {
    if session.phase != SessionPhase::Open {
        return Err(IpcFailure::new(
            "project_not_open",
            "open a Loom project first",
            false,
        ));
    }
    let active_session_id = session
        .active_session_id
        .ok_or_else(|| IpcFailure::new("project_not_open", "open a Loom project first", false))?;
    if active_session_id.to_string() != session_id {
        return Err(IpcFailure::new(
            "stale_project_session",
            "this command belongs to an expired project session",
            false,
        ));
    }
    let store = session
        .store
        .as_mut()
        .ok_or_else(|| IpcFailure::new("project_not_open", "open a Loom project first", false))?;
    if store.manifest().project_id.to_string() != project_id {
        return Err(IpcFailure::new(
            "project_identity_mismatch",
            "this command does not belong to the open project",
            false,
        ));
    }
    Ok(store)
}

fn title_for_path(path: &str) -> String {
    PathBuf::from(path)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(|stem| stem.replace(['-', '_'], " "))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| Path::new(path).to_string_lossy().into_owned())
}

fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

impl From<CommandReceipt> for Receipt {
    fn from(receipt: CommandReceipt) -> Self {
        Self {
            command_id: receipt.command_id.to_string(),
            command_kind: receipt.command.as_str().to_owned(),
            project_id: receipt.project_id.to_string(),
            schema_version: receipt.project_schema_version,
            source_revision_id: receipt.source_revision_id.map(|id| id.to_string()),
            result_revision_id: receipt
                .resulting_revision_ids
                .last()
                .map(ToString::to_string),
            result_blob_id: None,
            request_fingerprint: None,
            replayed: false,
            visible_projection: None,
            artifact_ids: receipt
                .resulting_artifact_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            completed_at_unix_ms: receipt.completed_at_ms,
        }
    }
}

impl From<IdempotentSaveOutcome> for Receipt {
    fn from(outcome: IdempotentSaveOutcome) -> Self {
        let result_blob_id = outcome.save.blob_id.to_string();
        let request_fingerprint = outcome.request_fingerprint.to_string();
        let replayed = outcome.replayed;
        let visible_projection = outcome.visible_projection;
        let mut receipt = Self::from(outcome.save.receipt);
        receipt.result_blob_id = Some(result_blob_id);
        receipt.request_fingerprint = Some(request_fingerprint);
        receipt.replayed = replayed;
        receipt.visible_projection = Some(visible_projection);
        receipt
    }
}

impl From<ExternalReconciliationOutcome> for Receipt {
    fn from(outcome: ExternalReconciliationOutcome) -> Self {
        let result_blob_id = outcome.save.blob_id.to_string();
        let request_fingerprint = outcome.request_fingerprint.to_string();
        let replayed = outcome.replayed;
        let visible_projection = outcome.visible_projection;
        let mut receipt = Self::from(outcome.save.receipt);
        receipt.result_blob_id = Some(result_blob_id);
        receipt.request_fingerprint = Some(request_fingerprint);
        receipt.replayed = replayed;
        receipt.visible_projection = Some(visible_projection);
        receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use loom_backend_llama::{
        CapabilitySupport, NativeEvidenceCapabilities, VerifiedCapabilitySet,
    };
    use loom_types::ModelEnvironmentId;

    #[derive(Debug, Default)]
    struct RecordingCancellation {
        branches: Mutex<Vec<BranchId>>,
        signal: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    }

    impl BranchCancellation for RecordingCancellation {
        fn cancel_branch(&self, branch_id: BranchId) -> bool {
            self.branches
                .lock()
                .expect("recording cancellation lock")
                .push(branch_id);
            if let Some(signal) = self.signal.lock().expect("signal lock").take() {
                let _ = signal.send(());
            }
            true
        }
    }

    #[derive(Debug)]
    struct NoopGenerationWorkerCancellation;

    impl FixtureGenerationWorkerCancellation for NoopGenerationWorkerCancellation {
        fn cancel_all(&self) {}
    }

    fn fixture_generation_worker_owner(
        cancellation: Arc<dyn FixtureGenerationWorkerCancellation>,
    ) -> GenerationWorkerOwner {
        GenerationWorkerOwner::fixture(cancellation, std::thread::spawn(|| {}))
    }

    fn noop_generation_worker_owner() -> GenerationWorkerOwner {
        fixture_generation_worker_owner(Arc::new(NoopGenerationWorkerCancellation))
    }

    #[derive(Debug)]
    struct FlagGenerationWorkerCancellation {
        cancelled: Arc<AtomicBool>,
    }

    impl FixtureGenerationWorkerCancellation for FlagGenerationWorkerCancellation {
        fn cancel_all(&self) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    #[derive(Debug)]
    struct PanickingGenerationWorkerCancellation;

    impl FixtureGenerationWorkerCancellation for PanickingGenerationWorkerCancellation {
        fn cancel_all(&self) {
            panic!("fixture cancellation panic");
        }
    }

    fn empty_application_shutdown_proof(state: &PluginState) -> ApplicationShutdownProof {
        let desktop_workers = state
            .join_desktop_workers()
            .expect("join empty desktop runtime");
        let native_runtime = state
            .native_runtime
            .shutdown_joined()
            .expect("join empty native runtime");
        ApplicationShutdownProof::from_graceful(native_runtime, desktop_workers)
    }

    #[test]
    fn model_load_lifetime_outlives_a_cancelled_async_command_until_worker_return() {
        let registry = Arc::new(ModelLoadRegistry::default());
        let application = Mutex::new(ApplicationPhase::Running);
        let admission = application.lock().expect("application admission");
        let command = registry.reserve(&admission).expect("model load permit");
        let worker = command.worker_guard();
        drop(admission);
        drop(command);

        assert_eq!(registry.state.lock().expect("model load state").active, 1);
        drop(worker);
        assert_eq!(registry.state.lock().expect("model load state").active, 0);
    }

    #[test]
    fn native_exit_request_quiesces_until_a_private_permit_authorizes_exit() {
        let state = PluginState::default();

        assert!(!record_application_exit_request(&state));
        assert_eq!(
            *state.application.lock().expect("application phase"),
            ApplicationPhase::Running
        );
        assert_eq!(
            ensure_application_running(&state, "new work")
                .expect_err("quiescence must close admission")
                .code,
            "application_quiescing"
        );

        let attempt = begin_application_close(&state).expect("begin close proof");
        assert_eq!(*attempt.phase, ApplicationPhase::Closing);
        let proof = empty_application_shutdown_proof(&state);
        let _permit = attempt.authorize(proof);
        assert_eq!(
            *state.application.lock().expect("application phase"),
            ApplicationPhase::ExitAuthorized
        );
        assert!(record_application_exit_request(&state));
    }

    #[test]
    fn failed_close_attempt_restores_admission_and_explicit_abort_is_idempotent() {
        let state = PluginState::default();
        {
            let _attempt = begin_application_close(&state).expect("begin close proof");
            assert_eq!(
                abort_application_close(&state)
                    .expect_err("an executing proof cannot be aborted concurrently")
                    .code,
                "application_close_in_progress"
            );
        }
        assert_eq!(
            ensure_application_running(&state, "new work")
                .expect_err("failed proof remains quiesced until explicit abort")
                .code,
            "application_quiescing"
        );
        abort_application_close(&state).expect("abort failed proof");
        ensure_application_running(&state, "new work").expect("abort restores running");

        assert!(!record_application_exit_request(&state));
        abort_application_close(&state).expect("abort quiescence");
        abort_application_close(&state).expect("abort replay");
        ensure_application_running(&state, "new work").expect("abort restores running");
    }

    #[test]
    fn pending_close_handshake_covers_an_exit_request_before_renderer_listener_installation() {
        let state = PluginState::default();
        assert!(!application_close_is_pending(&state).expect("running query"));

        assert!(!record_application_exit_request(&state));
        assert!(application_close_is_pending(&state).expect("quiescing query"));

        abort_application_close(&state).expect("abort quiescence");
        assert!(!application_close_is_pending(&state).expect("resumed query"));
    }

    #[test]
    fn final_runtime_exit_path_is_not_optional_plugin_drop_cleanup() {
        let source = include_str!("lib.rs");
        assert!(source.contains("if let RunEvent::Exit = event"));
        assert!(source.contains("quiesce_unpreventable_runtime_exit(app)"));
        assert!(source.contains("cleanup_before_exit"));
    }

    #[test]
    fn native_exit_recording_never_blocks_on_an_owned_application_admission_boundary() {
        let state = Arc::new(PluginState::default());
        let admission =
            lock_application_admission(&state, "fixture work").expect("admit fixture work");
        let (sent, received) = std::sync::mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            sent.send(record_application_exit_request(&worker_state))
                .expect("send exit disposition");
        });

        assert!(
            !received
                .recv_timeout(Duration::from_millis(20))
                .expect("event-thread exit recording must be nonblocking")
        );
        worker.join().expect("join exit-request worker");
        drop(admission);
        assert_eq!(
            *state.application.lock().expect("application phase"),
            ApplicationPhase::Running
        );
        assert_eq!(
            ensure_application_running(&state, "new work")
                .expect_err("recorded close intent must stop later admission")
                .code,
            "application_quiescing"
        );
    }

    #[test]
    fn generation_worker_reservation_prevents_exit_race_and_join_is_owned() {
        let application = Mutex::new(ApplicationPhase::Running);
        let admission = application.lock().expect("application admission");
        let workers = GenerationWorkerRegistry::default();
        let reservation = workers
            .reserve("request-one", &admission)
            .expect("reserve worker");
        assert_eq!(
            workers
                .join_all()
                .expect_err("reserved worker must block teardown")
                .code,
            "generation_worker_starting"
        );
        reservation
            .attach(std::thread::spawn(|| {}), noop_generation_worker_owner())
            .map_err(|error| error.failure)
            .expect("attach worker");
        assert_eq!(workers.join_all().expect("join owned worker").count(), 1);
        assert_eq!(workers.join_all().expect("join replay").count(), 0);
    }

    #[test]
    fn generation_worker_panic_latches_fail_closed_teardown() {
        let application = Mutex::new(ApplicationPhase::Running);
        let admission = application.lock().expect("application admission");
        let workers = GenerationWorkerRegistry::default();
        workers
            .reserve("request-panic", &admission)
            .expect("reserve worker")
            .attach(
                std::thread::spawn(|| panic!("fixture desktop panic")),
                noop_generation_worker_owner(),
            )
            .map_err(|error| error.failure)
            .expect("attach worker");

        assert_eq!(
            workers
                .join_all()
                .expect_err("worker panic must fail close")
                .code,
            "generation_worker_join_failed"
        );
        assert_eq!(
            workers
                .reserve("request-after-panic", &admission)
                .expect_err("panic evidence remains latched")
                .code,
            "generation_worker_join_failed"
        );
    }

    #[test]
    fn download_worker_reservation_is_joined_and_bound_to_its_exact_registry() {
        let application = Mutex::new(ApplicationPhase::Running);
        let admission = application.lock().expect("application admission");
        let workers = DownloadWorkerRegistry::default();
        let other = DownloadWorkerRegistry::default();
        let command_id = CommandId::new();
        let reservation = workers
            .reserve(command_id, &admission)
            .expect("reserve download worker");
        assert_eq!(
            workers
                .join_all()
                .expect_err("reserved download must block teardown")
                .code,
            "download_worker_starting"
        );
        reservation
            .attach(std::thread::spawn(|| {}), DownloadCancellation::default())
            .map_err(|error| error.failure)
            .expect("attach download worker");
        let joined = workers.join_all().expect("join download worker");
        assert_eq!(joined.count(), 1);
        assert!(joined.belongs_to(&workers));
        assert!(!joined.belongs_to(&other));
    }

    #[test]
    fn plugin_drop_joins_every_owned_desktop_worker_before_state_destruction() {
        let state = PluginState::default();
        let generation_stopped = Arc::new(AtomicBool::new(false));
        let download_stopped = Arc::new(AtomicBool::new(false));

        let generation_signal = Arc::clone(&generation_stopped);
        {
            let admission =
                lock_application_admission(&state, "fixture generation").expect("admission");
            state
                .generation_workers
                .reserve("drop-generation", &admission)
                .expect("reserve generation worker")
                .attach(
                    std::thread::spawn(move || {
                        generation_signal.store(true, Ordering::Release);
                    }),
                    noop_generation_worker_owner(),
                )
                .map_err(|error| error.failure)
                .expect("attach generation worker");
        }
        let download_signal = Arc::clone(&download_stopped);
        {
            let admission =
                lock_application_admission(&state, "fixture download").expect("admission");
            state
                .download_workers
                .reserve(CommandId::new(), &admission)
                .expect("reserve download worker")
                .attach(
                    std::thread::spawn(move || {
                        download_signal.store(true, Ordering::Release);
                    }),
                    DownloadCancellation::default(),
                )
                .map_err(|error| error.failure)
                .expect("attach download worker");
        }

        drop(state);
        assert!(generation_stopped.load(Ordering::Acquire));
        assert!(download_stopped.load(Ordering::Acquire));
    }

    #[test]
    fn unpreventable_exit_retains_cancellation_and_joins_every_desktop_worker() {
        let state = PluginState::default();
        let generation_cancelled = Arc::new(AtomicBool::new(false));
        let generation_finished = Arc::new(AtomicBool::new(false));
        let backend_forwarder_finished = Arc::new(AtomicBool::new(false));
        let panicking_cancellation_worker_finished = Arc::new(AtomicBool::new(false));
        let release_panicking_outer = Arc::new(AtomicBool::new(false));
        let download_finished = Arc::new(AtomicBool::new(false));
        let download_cancellation = DownloadCancellation::default();

        {
            let admission =
                lock_application_admission(&state, "fixture workers").expect("admission");
            let finished = Arc::clone(&panicking_cancellation_worker_finished);
            let release_outer = Arc::clone(&release_panicking_outer);
            state
                .generation_workers
                .reserve("exit-generation-panicking-cancellation", &admission)
                .expect("reserve generation with panicking cancellation")
                .attach(
                    std::thread::spawn(move || {
                        while !release_outer.load(Ordering::Acquire) {
                            std::thread::yield_now();
                        }
                        finished.store(true, Ordering::Release);
                    }),
                    fixture_generation_worker_owner(Arc::new(
                        PanickingGenerationWorkerCancellation,
                    )),
                )
                .map_err(|error| error.failure)
                .expect("attach generation with panicking cancellation");

            let finished = Arc::clone(&generation_finished);
            let backend_wait_cancelled = Arc::clone(&generation_cancelled);
            let backend_finished = Arc::clone(&backend_forwarder_finished);
            state
                .generation_workers
                .reserve("exit-generation", &admission)
                .expect("reserve generation")
                .attach(
                    std::thread::spawn(move || {
                        finished.store(true, Ordering::Release);
                    }),
                    GenerationWorkerOwner::fixture(
                        Arc::new(FlagGenerationWorkerCancellation {
                            cancelled: Arc::clone(&generation_cancelled),
                        }),
                        std::thread::spawn(move || {
                            while !backend_wait_cancelled.load(Ordering::Acquire) {
                                std::thread::yield_now();
                            }
                            backend_finished.store(true, Ordering::Release);
                        }),
                    ),
                )
                .map_err(|error| error.failure)
                .expect("attach generation");

            let wait_cancelled = download_cancellation.clone();
            let finished = Arc::clone(&download_finished);
            state
                .download_workers
                .reserve(CommandId::new(), &admission)
                .expect("reserve download")
                .attach(
                    std::thread::spawn(move || {
                        while !wait_cancelled.is_cancelled() {
                            std::thread::yield_now();
                        }
                        finished.store(true, Ordering::Release);
                    }),
                    download_cancellation,
                )
                .map_err(|error| error.failure)
                .expect("attach download");
        }

        release_panicking_outer.store(true, Ordering::Release);
        let joined = state.join_desktop_workers_for_exit();
        assert_eq!(joined.joined_worker_count(), 5);
        assert!(generation_cancelled.load(Ordering::Acquire));
        assert!(generation_finished.load(Ordering::Acquire));
        assert!(backend_forwarder_finished.load(Ordering::Acquire));
        assert!(panicking_cancellation_worker_finished.load(Ordering::Acquire));
        assert!(download_finished.load(Ordering::Acquire));
    }

    fn test_policy_expectation(expected_bytes: &[u8]) -> PolicyWriterExpectation {
        PolicyWriterExpectation {
            profile_id: "test-writer".to_owned(),
            rank: 0,
            role: ModelRole::Writer,
            prompt_mode: PromptMode::Completion,
            model_sha256: BlobId::digest(expected_bytes),
            model_file_bytes: u64::try_from(expected_bytes.len()).expect("fixture byte length"),
        }
    }

    fn test_capabilities() -> VerifiedCapabilitySet {
        VerifiedCapabilitySet {
            chat: CapabilitySupport::Unsupported,
            completion_text: CapabilitySupport::Supported,
            completion_token_ids: CapabilitySupport::Supported,
            fill_in_middle_contract_id: None,
            generated_token_ids: CapabilitySupport::Supported,
            token_observations: CapabilitySupport::Unsupported,
            probability_stages: Vec::new(),
            log_probability_stages: Vec::new(),
            max_cases: 4,
            ordered_outputs: CapabilitySupport::Supported,
            per_case_sampling: CapabilitySupport::Supported,
            per_case_cancellation: CapabilitySupport::Supported,
            sequence_snapshot: CapabilitySupport::Unsupported,
            sequence_restore: CapabilitySupport::Unsupported,
            per_case_restore: CapabilitySupport::Unsupported,
            token_exact_shared_prefix: CapabilitySupport::Unsupported,
            evidence: NativeEvidenceCapabilities::default(),
            media: Vec::new(),
        }
    }

    fn test_descriptor(
        path: &Path,
        expectation: &PolicyWriterExpectation,
        stable_model_id: &str,
    ) -> VerifiedModelDescriptor {
        VerifiedModelDescriptor {
            model_environment_id: ModelEnvironmentId::digest(stable_model_id.as_bytes()),
            stable_model_id: stable_model_id.to_owned(),
            local_model_id: stable_model_id.to_owned(),
            model_path: path.to_path_buf(),
            display_name: stable_model_id.to_owned(),
            architecture: Some("test".to_owned()),
            parameter_count: None,
            model_file_bytes: expectation.model_file_bytes,
            model_sha256: expectation.model_sha256.to_string(),
            tokenizer_sha256: BlobId::digest(b"test-tokenizer").to_string(),
            chat_template_sha256: BlobId::digest(b"no-chat-template").to_string(),
            projector_sha256: None,
            binding_version: "test-binding".to_owned(),
            build_id: "test-build".to_owned(),
            backend: "test-backend".to_owned(),
            context_tokens: 4_096,
            batch_tokens: 512,
            max_parallel_cases: 4,
            rope_config_sha256: BlobId::digest(b"test-rope").to_string(),
            kv_layout_sha256: BlobId::digest(b"test-kv").to_string(),
            capabilities: test_capabilities(),
        }
    }

    fn test_loaded_model(path: &Path, stable_model_id: &str) -> LoadedModel {
        let expectation = test_policy_expectation(stable_model_id.as_bytes());
        LoadedModel {
            profile: LocalModelProfile::for_gguf(path),
            descriptor: test_descriptor(path, &expectation, stable_model_id),
        }
    }

    fn test_policy_loaded_model(policy: &BuildModelPolicy, path: &Path) -> LoadedModel {
        let writer = policy.writers().first().expect("writer policy");
        let expectation =
            policy_writer_expectation(policy, writer.profile_id()).expect("known writer policy");
        LoadedModel {
            profile: LocalModelProfile::for_gguf(path),
            descriptor: test_descriptor(path, &expectation, "policy-writer"),
        }
    }

    fn test_automatic_model_authority() -> AuthorizedWeaveModel {
        let policy = BuildModelPolicy::writer_gemma4_base_v2();
        AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::AutomaticV2,
            test_policy_loaded_model(&policy, Path::new("/tmp/policy-writer.gguf")),
            &policy,
        )
        .expect("exact policy writer authority")
    }

    fn assert_loaded_model_id(state: &PluginState, expected: &str) {
        let registry = state.model.lock().expect("model registry");
        let ModelRegistry::Loaded(loaded) = &*registry else {
            panic!("expected the previous model to remain loaded");
        };
        assert_eq!(loaded.descriptor.stable_model_id, expected);
    }

    #[test]
    fn loaded_registry_latches_unknown_residency_when_native_slot_access_is_absent() {
        let state = PluginState::default();
        *state.model.lock().expect("model registry") = ModelRegistry::Loaded(Box::new(
            test_loaded_model(Path::new("/tmp/not-resident.gguf"), "not-resident"),
        ));

        let error = unload_registered_model(&state)
            .expect_err("loaded authority requires a proved native slot release");

        assert_eq!(error.code, "model_residency_unknown");
        assert!(matches!(
            &*state.model.lock().expect("model registry"),
            ModelRegistry::ResidencyUnknown { .. }
        ));
        assert_eq!(
            unload_registered_model(&state)
                .expect_err("unknown residency must remain fail closed")
                .code,
            "model_residency_unknown"
        );
    }

    #[test]
    fn unknown_native_residency_blocks_inference_and_application_teardown() {
        let state = PluginState::default();
        *state.model.lock().expect("model registry") = ModelRegistry::ResidencyUnknown {
            reason: "fixture cleanup failure".to_owned(),
        };

        assert_eq!(
            loaded_model_for_state(&state)
                .expect_err("unknown residency cannot mint generation authority")
                .code,
            "model_residency_unknown"
        );
        assert_eq!(
            unload_registered_model(&state)
                .expect_err("unknown residency cannot mint exit authority")
                .code,
            "model_residency_unknown"
        );
    }

    #[test]
    fn same_size_wrong_policy_digest_never_reaches_native_inspection() {
        let temporary = tempfile::tempdir().expect("temporary model directory");
        let path = temporary.path().join("writer.gguf");
        let expected = b"correct identity";
        let wrong = b"incorrect idents";
        assert_eq!(expected.len(), wrong.len());
        std::fs::write(&path, wrong).expect("write same-size wrong model");
        let canonical = path.canonicalize().expect("canonical model path");
        let expectation = test_policy_expectation(expected);
        let inspected = Cell::new(false);

        let error = inspect_preverified_policy_file(&canonical, &expectation, || {
            inspected.set(true);
            Ok(())
        })
        .expect_err("wrong digest must fail before native inspection");

        assert_eq!(error.failure.code, "policy_model_digest_mismatch");
        assert!(!error.native_inspection_started);
        assert!(!inspected.get());
    }

    #[test]
    fn unknown_policy_profile_is_rejected_without_path_resolution() {
        let error = policy_writer_expectation(&BuildModelPolicy::none_v1(), "not-present")
            .expect_err("unknown profile must fail closed");
        assert_eq!(error.code, "unknown_policy_model_profile");
    }

    #[test]
    fn strict_policy_candidate_can_reopen_an_exact_path_after_restart() {
        let temporary = tempfile::tempdir().expect("temporary model directory");
        let path = temporary.path().join("remembered-writer.gguf");
        std::fs::write(&path, b"GGUFremembered-writer-fixture").expect("write GGUF fixture");
        let canonical = path.canonicalize().expect("canonical fixture path");

        let reopened = discover_strict_policy_candidate(&canonical)
            .expect("strict policy path is independently rediscovered");
        assert_eq!(reopened.resolved_path, canonical);
        assert!(matches!(reopened.header, GgufHeaderStatus::Verified));
    }

    #[test]
    fn read_only_build_policy_identity_preserves_versioned_activation_and_digest() {
        let v1 = BuildModelPolicy::writer_gemma4_base_v1().identity();
        let v2 = BuildModelPolicy::writer_gemma4_base_v2().identity();

        assert_eq!(
            v1.name(),
            loom_types::BuildModelPolicyName::WriterGemma4BaseV1
        );
        assert_eq!(
            v1.activation(),
            loom_types::SuggestionActivation::ProjectOptIn
        );
        assert_eq!(
            v1.canonical_sha256().to_string(),
            "c0492fb2285ad0922f89ab7288d63ef68fd17f5133f00ea4276622a15c2dc4e6"
        );
        assert_eq!(
            v2.name(),
            loom_types::BuildModelPolicyName::WriterGemma4BaseV2
        );
        assert_eq!(
            v2.activation(),
            loom_types::SuggestionActivation::QuietDefault
        );
        assert_eq!(
            v2.canonical_sha256().to_string(),
            "2d402d213b60ba65c4d018907e9eba67ccfbc1e97081cc0505f9713ae2dd89d2"
        );
    }

    #[test]
    fn automatic_writer_authority_rejects_none_and_arbitrary_resident_models() {
        let writer_policy = BuildModelPolicy::writer_gemma4_base_v2();
        let exact_writer =
            test_policy_loaded_model(&writer_policy, Path::new("/tmp/policy-writer.gguf"));
        let none_error = AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::AutomaticV2,
            exact_writer,
            &BuildModelPolicy::none_v1(),
        )
        .expect_err("none-v1 cannot authorize automatic generation");
        assert_eq!(none_error.code, "automatic_writer_not_in_build_policy");

        let arbitrary = test_loaded_model(
            Path::new("/tmp/arbitrary-completion-model.gguf"),
            "arbitrary-completion-model",
        );
        let arbitrary_error = AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::AutomaticV2,
            arbitrary,
            &writer_policy,
        )
        .expect_err("an arbitrary capable resident model is not a build writer");
        assert_eq!(arbitrary_error.code, "automatic_writer_not_in_build_policy");
    }

    #[test]
    fn automatic_writer_authority_requires_raw_completion_and_generated_tokens() {
        let policy = BuildModelPolicy::writer_gemma4_base_v2();
        let mut without_completion =
            test_policy_loaded_model(&policy, Path::new("/tmp/incapable-policy-writer.gguf"));
        without_completion.descriptor.capabilities.completion_text = CapabilitySupport::Unsupported;
        let completion_error = AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::AutomaticV2,
            without_completion,
            &policy,
        )
        .expect_err("automatic writer must prove raw completion support");
        assert_eq!(completion_error.code, "policy_model_capability_mismatch");

        let mut without_tokens =
            test_policy_loaded_model(&policy, Path::new("/tmp/incapable-policy-writer.gguf"));
        without_tokens.descriptor.capabilities.generated_token_ids = CapabilitySupport::Unsupported;

        let token_error =
            AuthorizedWeaveModel::bind(ValidatedWeavePolicy::AutomaticV2, without_tokens, &policy)
                .expect_err("automatic writer capability proof must fail closed");

        assert_eq!(token_error.code, "policy_model_capability_mismatch");
    }

    #[test]
    fn exact_policy_writer_mints_typed_automatic_authority() {
        let policy = BuildModelPolicy::writer_gemma4_base_v2();
        let authorized = AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::AutomaticV2,
            test_policy_loaded_model(&policy, Path::new("/tmp/policy-writer.gguf")),
            &policy,
        )
        .expect("exact writer must be admitted");

        assert_eq!(
            authorized.automatic_binding(),
            Some((
                BuildWriterProfileId::Gemma4E2bBaseQ8LoomV1,
                0,
                policy.identity(),
            ))
        );
    }

    #[test]
    fn manual_weave_authority_remains_independent_of_build_writer_policy() {
        let authorized = AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::ManualV2 {
                branch_count: 1,
                max_tokens: 32,
                temperature: 0.8,
            },
            test_loaded_model(
                Path::new("/tmp/arbitrary-manual-model.gguf"),
                "arbitrary-manual-model",
            ),
            &BuildModelPolicy::none_v1(),
        )
        .expect("manual requests may use an explicitly loaded model");

        assert_eq!(authorized.automatic_binding(), None);
    }

    #[test]
    fn rejected_automatic_writers_leave_budget_ledger_untouched() {
        let authority = AutomaticBudgetAuthority::default();
        let writer_policy = BuildModelPolicy::writer_gemma4_base_v2();
        let arbitrary_rejection = AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::AutomaticV2,
            test_loaded_model(Path::new("/tmp/arbitrary.gguf"), "arbitrary"),
            &writer_policy,
        );
        assert_eq!(
            arbitrary_rejection
                .expect_err("arbitrary model cannot mint the opaque request authority")
                .code,
            "automatic_writer_not_in_build_policy"
        );

        let none_rejection = AuthorizedWeaveModel::bind(
            ValidatedWeavePolicy::AutomaticV2,
            test_policy_loaded_model(&writer_policy, Path::new("/tmp/policy-writer.gguf")),
            &BuildModelPolicy::none_v1(),
        );
        assert_eq!(
            none_rejection
                .expect_err("none-v1 cannot mint the opaque request authority")
                .code,
            "automatic_writer_not_in_build_policy"
        );

        let ledger = authority.ledger.lock().expect("automatic budget ledger");
        assert_eq!(ledger.active_session, None);
        assert!(ledger.families_by_scope.is_empty());
    }

    #[test]
    fn policy_candidate_and_exact_match_preserve_source_order_rank() {
        let policy = BuildModelPolicy::writer_gemma4_base_v1();
        let writer = policy.writers().first().expect("writer policy");
        let expected =
            policy_writer_expectation(&policy, writer.profile_id()).expect("known writer profile");
        let candidate = policy_candidate_summary(&policy, writer.model_file_bytes())
            .expect("unique size candidate");

        assert_eq!(expected.rank, 0);
        assert_eq!(candidate.rank, expected.rank);
        assert_eq!(candidate.profile_id, expected.profile_id);
    }

    #[test]
    fn pre_native_policy_failure_restores_the_previous_model() {
        let state = PluginState::default();
        let staged_path = PathBuf::from("/tmp/new-writer.gguf");
        let staged_profile = LocalModelProfile::for_gguf(&staged_path);
        let previous = test_loaded_model(Path::new("/tmp/previous.gguf"), "previous-model");
        *state.model.lock().expect("model registry") = ModelRegistry::Loading {
            path: staged_path.clone(),
            previous: Some(Box::new(previous)),
        };

        let error = resolve_policy_model_inspection(
            &state,
            &staged_path,
            &staged_profile,
            Err(PolicyInspectionFailure {
                failure: IpcFailure::new("policy_model_digest_mismatch", "wrong digest", false),
                native_inspection_started: false,
            }),
        )
        .expect_err("failed verification must not replace the previous model");

        assert_eq!(error.code, "policy_model_digest_mismatch");
        assert_loaded_model_id(&state, "previous-model");
    }

    #[test]
    fn failed_native_inspection_that_never_acquired_restores_previous_model() {
        let state = PluginState::default();
        let staged_path = PathBuf::from("/tmp/incapable-writer.gguf");
        let staged_profile = LocalModelProfile::for_gguf(&staged_path);
        let expectation = test_policy_expectation(b"incapable-writer");
        let mut descriptor = test_descriptor(&staged_path, &expectation, "staged-model");
        descriptor.capabilities.generated_token_ids = CapabilitySupport::Unsupported;
        let validation = validate_policy_model_descriptor(&descriptor, &staged_path, &expectation)
            .expect_err("policy capabilities must be proven");
        assert_eq!(validation.code, "policy_model_capability_mismatch");

        let previous = test_loaded_model(Path::new("/tmp/previous.gguf"), "previous-model");
        *state.model.lock().expect("model registry") = ModelRegistry::Loading {
            path: staged_path.clone(),
            previous: Some(Box::new(previous)),
        };
        let error = resolve_policy_model_inspection(
            &state,
            &staged_path,
            &staged_profile,
            Err(PolicyInspectionFailure {
                failure: validation,
                native_inspection_started: true,
            }),
        )
        .expect_err("capability failure must not commit the staged model");

        assert_eq!(error.code, "policy_model_capability_mismatch");
        assert_loaded_model_id(&state, "previous-model");
    }

    #[test]
    fn failed_native_inspection_that_never_acquired_restores_empty_registry() {
        let state = PluginState::default();
        let staged_path = PathBuf::from("/tmp/rejected-writer.gguf");
        let staged_profile = LocalModelProfile::for_gguf(&staged_path);
        *state.model.lock().expect("model registry") = ModelRegistry::Loading {
            path: staged_path.clone(),
            previous: None,
        };

        let error = resolve_policy_model_inspection(
            &state,
            &staged_path,
            &staged_profile,
            Err(PolicyInspectionFailure {
                failure: IpcFailure::new(
                    "native_model_rejected",
                    "native inspection rejected the model before acquisition",
                    false,
                ),
                native_inspection_started: true,
            }),
        )
        .expect_err("the original inspection failure must be retained");

        assert_eq!(error.code, "native_model_rejected");
        assert!(matches!(
            &*state.model.lock().expect("model registry"),
            ModelRegistry::Empty
        ));
    }

    #[test]
    fn changed_registry_and_absent_staged_release_latches_unknown_residency() {
        let state = PluginState::default();
        *state.model.lock().expect("model registry") = ModelRegistry::Loaded(Box::new(
            test_loaded_model(Path::new("/tmp/other.gguf"), "other-model"),
        ));
        let staged_path = PathBuf::from("/tmp/staged.gguf");
        let staged_profile = LocalModelProfile::for_gguf(&staged_path);

        let error = release_staged_model(&state, &staged_path, &staged_profile)
            .expect_err("an absent staged slot cannot prove cleanup after authority changed");

        assert_eq!(error.code, "model_residency_unknown");
        assert!(matches!(
            &*state.model.lock().expect("model registry"),
            ModelRegistry::ResidencyUnknown { .. }
        ));
    }

    #[test]
    fn loaded_identity_mismatch_has_no_unverified_size_hint() {
        let policy = BuildModelPolicy::writer_gemma4_base_v1();
        let writer = policy.writers().first().expect("writer policy");
        let path = Path::new("/tmp/same-size-wrong-identity.gguf");
        let mut mismatch = test_loaded_model(path, "same-size-wrong-identity");
        mismatch.descriptor.model_file_bytes = writer.model_file_bytes();
        let summary = model_summary(&mismatch, true, &policy);

        assert!(summary.policy_candidate.is_none());
        assert!(summary.policy_verified.is_none());
        assert!(summary.tested_profile.is_none());
    }

    fn start_persisted_test_generation(
        store: &mut ProjectStore,
    ) -> (GenerationRunId, BranchId, DocumentId) {
        let initial = store
            .read_document(INITIAL_DOCUMENT)
            .expect("read initial manuscript");
        store
            .save_document_if_source(
                INITIAL_DOCUMENT,
                DocumentContent::Prose("Once ".to_owned()),
                "establish generation prefix",
                initial.revision_id,
                initial.blob_id,
            )
            .expect("save generation prefix");
        let loaded = store
            .read_document(INITIAL_DOCUMENT)
            .expect("read generation prefix");
        let environment = ModelEnvironment {
            environment_id: loom_types::ModelEnvironmentId::digest(b"test-close-environment"),
            model_identifier: "test-close-model".to_owned(),
            model_fingerprint: BlobId::digest(b"test-close-model"),
            tokenizer_fingerprint: BlobId::digest(b"test-close-tokenizer"),
            backend_identifier: "test-close-backend".to_owned(),
            capabilities: serde_json::json!({"completion": true}),
        };
        let environment_artifact = store
            .record_model_environment(&environment)
            .expect("record test environment")
            .artifact_id;
        let prompt_blob = store
            .store_provenance_blob(loaded.text.as_bytes())
            .expect("store test prompt");
        let prompt_artifact = store
            .record_prompt_recipe(&PromptRecipe {
                mode: PromptMode::Completion,
                exact_prompt_blob_id: prompt_blob,
                exact_prompt_token_ids: None,
                ordered_input_artifact_ids: vec![loaded.artifact_id],
                prompt_token_count: None,
            })
            .expect("record test prompt recipe")
            .artifact_id;
        let context_artifact = store
            .record_context_recipe(&ContextRecipe {
                source_revision_id: loaded.revision_id,
                ordered_source_artifact_ids: Vec::new(),
                token_budget: 128,
                retrieval_evidence_blob_id: None,
            })
            .expect("record test context")
            .artifact_id;
        let policy_artifact = store
            .record_authority_policy(&AuthorityPolicy {
                policy_version: 1,
                writer_environment_artifact_ids: vec![environment_artifact],
                critic_environment_artifact_ids: Vec::new(),
            })
            .expect("record test authority")
            .artifact_id;
        let run_id = GenerationRunId::new();
        let branch_id = BranchId::new();
        let cursor = u64::try_from(loaded.text.len()).expect("test prefix length");
        store
            .start_generation(GenerationStart {
                run_id,
                branch_id,
                document_id: loaded.document_id,
                source_revision_id: loaded.revision_id,
                target_range: ByteRange::new(cursor, cursor).expect("test target range"),
                model_environment_artifact_id: environment_artifact,
                prompt_recipe_artifact_id: prompt_artifact,
                context_recipe_artifact_id: context_artifact,
                authority_policy_artifact_id: policy_artifact,
                seed: 7,
                sampling: serde_json::json!({"temperature": 0.8}),
            })
            .expect("persist queued generation");
        (run_id, branch_id, loaded.document_id)
    }

    struct ReconciliationFixture {
        _temporary: tempfile::TempDir,
        store: ProjectStore,
        base: LoadedDocument,
        project_id: String,
        session_id: String,
    }

    impl ReconciliationFixture {
        fn new(base_text: &str) -> Self {
            let temporary = tempfile::tempdir().expect("temporary project parent");
            let root = temporary.path().join("Reconciliation Novel");
            let mut store =
                initialize_project(&root, "Reconciliation Novel".to_owned()).expect("initialize");
            let initial = store
                .read_document(INITIAL_DOCUMENT)
                .expect("read initial document");
            if !base_text.is_empty() {
                store
                    .save_document_if_source(
                        INITIAL_DOCUMENT,
                        DocumentContent::Prose(base_text.to_owned()),
                        "establish reconciliation base",
                        initial.revision_id,
                        initial.blob_id,
                    )
                    .expect("save reconciliation base");
            }
            let base = store
                .read_document(INITIAL_DOCUMENT)
                .expect("read reconciliation base");
            let project_id = store.manifest().project_id.to_string();
            Self {
                _temporary: temporary,
                store,
                base,
                project_id,
                session_id: CommandId::new().to_string(),
            }
        }

        fn set_external(&self, text: &str) -> BlobId {
            std::fs::write(self.store.root().join(INITIAL_DOCUMENT), text)
                .expect("write external document");
            BlobId::digest(text.as_bytes())
        }

        fn preview_request(&self, app_text: Option<&str>) -> PreviewRequest {
            PreviewRequest {
                project_id: self.project_id.clone(),
                session_id: self.session_id.clone(),
                document_id: self.base.document_id.to_string(),
                relative_path: INITIAL_DOCUMENT.to_owned(),
                expected_revision_id: self.base.revision_id,
                expected_base_blob_id: self.base.blob_id,
                app_text: app_text.map(str::to_owned),
            }
        }

        fn apply_request(
            &self,
            external_blob_id: BlobId,
            resolved_text: &str,
            command_id: CommandId,
        ) -> ApplyRequest {
            ApplyRequest {
                document_id: self.base.document_id.to_string(),
                relative_path: INITIAL_DOCUMENT.to_owned(),
                expected_revision_id: self.base.revision_id,
                expected_base_blob_id: self.base.blob_id,
                expected_visible_blob_id: external_blob_id,
                resolved_content: DocumentContent::Prose(resolved_text.to_owned()),
                reason: "author resolved external edit".to_owned(),
                command_id,
            }
        }
    }

    #[test]
    fn title_is_readable_without_changing_path_identity() {
        assert_eq!(title_for_path("manuscript/001-opening.md"), "001 opening");
    }

    #[test]
    fn word_count_handles_whitespace_without_normalizing_document() {
        assert_eq!(count_words("first\n  second\tthird"), 3);
    }

    #[test]
    fn generation_seeds_are_deterministic_and_branch_specific() {
        let command_id = CommandId::new();
        assert_eq!(
            generation_seed(command_id, 0, WeavePreset::AutomaticProseV2),
            generation_seed(command_id, 0, WeavePreset::AutomaticProseV2)
        );
        assert_ne!(
            generation_seed(command_id, 0, WeavePreset::AutomaticProseV2),
            generation_seed(command_id, 1, WeavePreset::AutomaticProseV2)
        );
        assert_ne!(
            generation_seed(command_id, 0, WeavePreset::AutomaticProseV2),
            generation_seed(command_id, 0, WeavePreset::AutomaticVerseV2)
        );
        assert_ne!(
            generation_seed(command_id, 0, WeavePreset::AutomaticVerseV2),
            generation_seed(command_id, 0, WeavePreset::ManualV2)
        );
    }

    #[test]
    fn automatic_budget_reservations_are_affine_and_revision_bounded() {
        let authority = AutomaticBudgetAuthority::default();
        let automatic = test_automatic_model_authority();
        let writer = automatic
            .automatic_writer()
            .expect("automatic writer witness");
        let scope = AutomaticBudgetScope {
            project: ProjectId::new(),
            session: CommandId::new(),
            document: DocumentId::new(),
            source_revision: RevisionId::new(),
        };
        authority
            .reserve(writer, scope)
            .expect("first family")
            .commit();
        authority
            .reserve(writer, scope)
            .expect("replacement family")
            .commit();
        assert_eq!(
            authority
                .reserve(writer, scope)
                .expect_err("revision budget exhausted"),
            AutomaticBudgetError::Exhausted
        );
        assert_eq!(AUTOMATIC_TOKEN_BUDGET_PER_REVISION_V2, 288);
    }

    #[test]
    fn uncommitted_automatic_budget_reservation_refunds_on_drop() {
        let authority = AutomaticBudgetAuthority::default();
        let automatic = test_automatic_model_authority();
        let writer = automatic
            .automatic_writer()
            .expect("automatic writer witness");
        let scope = AutomaticBudgetScope {
            project: ProjectId::new(),
            session: CommandId::new(),
            document: DocumentId::new(),
            source_revision: RevisionId::new(),
        };
        let abandoned = authority.reserve(writer, scope).expect("pending family");
        let committed = authority
            .reserve(writer, scope)
            .expect("second pending family");
        assert_eq!(
            authority
                .reserve(writer, scope)
                .expect_err("pending slots count"),
            AutomaticBudgetError::Exhausted
        );
        drop(abandoned);
        authority
            .reserve(writer, scope)
            .expect("refunded family")
            .commit();
        committed.commit();
        assert_eq!(
            authority
                .reserve(writer, scope)
                .expect_err("committed work remains spent"),
            AutomaticBudgetError::Exhausted
        );
    }

    #[test]
    fn automatic_budget_renews_only_for_new_authoritative_revision_or_session() {
        let authority = AutomaticBudgetAuthority::default();
        let automatic = test_automatic_model_authority();
        let writer = automatic
            .automatic_writer()
            .expect("automatic writer witness");
        let project_id = ProjectId::new();
        let session_id = CommandId::new();
        let document_id = DocumentId::new();
        let first = AutomaticBudgetScope {
            project: project_id,
            session: session_id,
            document: document_id,
            source_revision: RevisionId::new(),
        };
        authority
            .reserve(writer, first)
            .expect("first revision")
            .commit();
        authority
            .reserve(writer, first)
            .expect("first replacement")
            .commit();

        let next_revision = AutomaticBudgetScope {
            source_revision: RevisionId::new(),
            ..first
        };
        authority
            .reserve(writer, next_revision)
            .expect("new revision renews")
            .commit();
        let next_session = AutomaticBudgetScope {
            session: CommandId::new(),
            ..next_revision
        };
        authority
            .reserve(writer, next_session)
            .expect("new project session renews")
            .commit();
    }

    #[test]
    fn automatic_sampling_is_typed_and_repetition_resistant() {
        let command_id = CommandId::new();
        let automatic =
            sampling_for_weave_case(command_id, 0, 48, 0.8, WeavePreset::AutomaticProseV2);
        let verse = sampling_for_weave_case(command_id, 0, 48, 0.8, WeavePreset::AutomaticVerseV2);
        let manual = sampling_for_weave_case(command_id, 0, 48, 0.8, WeavePreset::ManualV2);

        assert_ne!(automatic.seed, verse.seed);
        assert_ne!(verse.seed, manual.seed);
        assert_eq!(automatic.max_tokens, 48);
        assert_eq!(automatic.temperature.to_bits(), 0.8_f32.to_bits());
        assert_eq!(automatic.repeat_penalty.to_bits(), 1.08_f32.to_bits());
        assert_eq!(automatic.dry_multiplier.to_bits(), 0.8_f32.to_bits());
        assert_eq!(automatic.dry_allowed_length, 4);
        assert_eq!(automatic.dry_penalty_last_n, 256);
        assert_eq!(verse.repeat_penalty.to_bits(), 1.0_f32.to_bits());
        assert_eq!(verse.dry_multiplier.to_bits(), 0.0_f32.to_bits());
        assert_eq!(
            manual.repeat_penalty.to_bits(),
            SamplingConfig::default().repeat_penalty.to_bits()
        );
        assert_eq!(
            manual.dry_multiplier.to_bits(),
            SamplingConfig::default().dry_multiplier.to_bits()
        );
    }

    #[test]
    fn weave_v2_sampling_has_stable_exact_bit_fingerprints() {
        let command_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV"
            .parse::<CommandId>()
            .expect("fixed command ID");
        let prose = sampling_for_weave_case(
            command_id,
            0,
            AUTOMATIC_WEAVE_MAX_TOKENS_V2,
            AUTOMATIC_WEAVE_TEMPERATURE_V2,
            WeavePreset::AutomaticProseV2,
        );
        let verse = sampling_for_weave_case(
            command_id,
            0,
            AUTOMATIC_WEAVE_MAX_TOKENS_V2,
            AUTOMATIC_WEAVE_TEMPERATURE_V2,
            WeavePreset::AutomaticVerseV2,
        );
        let manual = sampling_for_weave_case(command_id, 0, 48, 0.8, WeavePreset::ManualV2);
        assert_eq!(
            prose.fingerprint().sha256_hex(),
            "8958697e23818dd62c364f46d14d12e977a10b2428be17d255954a25bd3d529c"
        );
        assert_eq!(
            verse.fingerprint().sha256_hex(),
            "60bf288ce682c665732d7975bca0722a8f9d8e71fdfa59e453e0c4c0734feef9"
        );
        assert_eq!(
            manual.fingerprint().sha256_hex(),
            "7db5d3b7e3450e90f85074910b816dbce5b102adff36ebf6d62beb4e800bf0bc"
        );
    }

    #[test]
    fn automatic_policy_has_no_runtime_budget_fields() {
        let parsed: WeavePolicySnapshot =
            serde_json::from_str(r#"{"kind":"automatic_v2"}"#).expect("automatic policy");
        assert_eq!(
            validate_weave_policy(parsed).expect("validate automatic"),
            ValidatedWeavePolicy::AutomaticV2
        );
        assert!(
            serde_json::from_str::<WeavePolicySnapshot>(
                r#"{"kind":"automatic_v2","max_tokens":2048}"#
            )
            .is_err()
        );
    }

    #[test]
    fn automatic_policy_is_bound_by_rust_to_the_authoritative_document_kind() {
        let policy = ValidatedWeavePolicy::AutomaticV2;
        assert_eq!(
            policy
                .bind_document_kind(DocumentKind::Prose)
                .expect("prose")
                .preset,
            WeavePreset::AutomaticProseV2
        );
        assert_eq!(
            policy
                .bind_document_kind(DocumentKind::Verse)
                .expect("verse")
                .preset,
            WeavePreset::AutomaticVerseV2
        );
        assert_eq!(
            policy
                .bind_document_kind(DocumentKind::Hybrid)
                .expect_err("hybrid has no authoritative caret block")
                .code,
            "automatic_hybrid_boundary_unresolved"
        );
    }

    #[test]
    fn branch_snapshot_never_presents_an_orphan_as_live_generation() {
        let record = StoredBranchRecord {
            run_id: GenerationRunId::new(),
            branch_id: BranchId::new(),
            document_id: DocumentId::new(),
            source_revision_id: RevisionId::new(),
            target_range: ByteRange::new(7, 7).expect("target range"),
            model_identifier: "sha256:model".to_string(),
            seed: 42,
            status: StoredBranchStatus::Interrupted,
            candidate_id: None,
            output_text: None,
            output_blob_id: None,
            output_byte_len: None,
            error: None,
            selection: None,
            created_at_ms: 12,
        };
        let interrupted = branch_snapshot(record.clone(), false);
        assert_eq!(interrupted.status, "interrupted");
        let live = branch_snapshot(record, true);
        assert_eq!(live.status, "generating");
    }

    #[test]
    fn branch_cursor_preserves_u64_sequence_as_decimal_text() {
        let cursor = BranchPageCursor {
            sequence: u64::MAX,
            run_id: GenerationRunId::new(),
        };
        let snapshot = BranchCursorSnapshot::from(cursor);
        let json = serde_json::to_value(&snapshot).expect("serialize branch cursor");
        assert_eq!(json["sequence"], u64::MAX.to_string());
        assert_eq!(
            BranchPageCursor::try_from(snapshot).expect("parse branch cursor"),
            cursor
        );
    }

    #[test]
    fn project_creation_refuses_existing_default_manuscript_without_touching_it() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Existing Novel");
        let manuscript = root.join(INITIAL_DOCUMENT);
        std::fs::create_dir_all(manuscript.parent().expect("manuscript parent"))
            .expect("create manuscript parent");
        let original = "Already written.\n\nStill here.\n";
        std::fs::write(&manuscript, original).expect("write existing manuscript");

        let error = initialize_project(&root, "Existing Novel".to_owned())
            .expect_err("creation must refuse an ambiguous existing manuscript");

        assert_eq!(error.code, "existing_manuscript_requires_import");
        assert_eq!(
            std::fs::read_to_string(&manuscript).expect("read visible manuscript"),
            original
        );
        assert!(!root.join(".loom").exists());
    }

    #[test]
    fn default_project_opens_directly_and_reuses_the_same_plain_text_workspace() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let first = open_or_initialize_default_project(&root).expect("create default project");
        let project_id = first.manifest().project_id;
        let document = first
            .read_document(INITIAL_DOCUMENT)
            .expect("read initial manuscript");
        assert_eq!(document.text, "");
        assert_eq!(
            std::fs::read_to_string(root.join(INITIAL_DOCUMENT)).expect("read visible manuscript"),
            ""
        );
        drop(first);

        let second = open_or_initialize_default_project(&root).expect("reopen default project");
        assert_eq!(second.manifest().project_id, project_id);
        assert_eq!(
            second
                .read_document(INITIAL_DOCUMENT)
                .expect("read reopened manuscript")
                .text,
            ""
        );
    }

    #[test]
    fn default_project_repairs_interruption_after_manifest_before_document() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let (store, _) =
            ProjectStore::initialize(&root, "My Writing").expect("initialize sidecar only");
        let project_id = store.manifest().project_id;
        assert!(store.list_documents().expect("list documents").is_empty());
        drop(store);

        let repaired = open_or_initialize_default_project(&root)
            .expect("repair interrupted default initialization");
        assert_eq!(repaired.manifest().project_id, project_id);
        assert_eq!(
            repaired
                .read_document(INITIAL_DOCUMENT)
                .expect("read repaired manuscript")
                .text,
            ""
        );
        assert_eq!(
            std::fs::read(root.join(INITIAL_DOCUMENT)).expect("read visible manuscript"),
            b""
        );
    }

    #[test]
    fn default_project_adopts_exact_visible_bytes_when_sidecar_is_absent() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let manuscript = root.join(INITIAL_DOCUMENT);
        std::fs::create_dir_all(manuscript.parent().expect("manuscript parent"))
            .expect("create manuscript directory");
        let original = "  opening\r\n\r\ncafé\t \r\n".as_bytes();
        std::fs::write(&manuscript, original).expect("write surviving manuscript");

        let recovered =
            open_or_initialize_default_project(&root).expect("adopt surviving manuscript");
        assert_eq!(
            recovered
                .read_document(INITIAL_DOCUMENT)
                .expect("read adopted manuscript")
                .text
                .as_bytes(),
            original
        );
        assert_eq!(
            std::fs::read(&manuscript).expect("read unchanged visible manuscript"),
            original
        );
        assert!(root.join(".loom/project.json").is_file());
    }

    #[test]
    fn default_project_recovers_after_complete_sidecar_loss_without_rewriting_text() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let first = open_or_initialize_default_project(&root).expect("create default project");
        let first_project_id = first.manifest().project_id;
        drop(first);
        let original = "surviving\r\nexternal  text\r\n".as_bytes();
        std::fs::write(root.join(INITIAL_DOCUMENT), original).expect("write surviving text");
        std::fs::rename(root.join(".loom"), root.join("lost-sidecar"))
            .expect("preserve lost sidecar outside the active location");

        let recovered =
            open_or_initialize_default_project(&root).expect("recreate sidecar from manuscript");
        assert_ne!(recovered.manifest().project_id, first_project_id);
        assert_eq!(
            recovered
                .read_document(INITIAL_DOCUMENT)
                .expect("read recovered manuscript")
                .text
                .as_bytes(),
            original
        );
        assert_eq!(
            std::fs::read(root.join(INITIAL_DOCUMENT)).expect("read preserved manuscript"),
            original
        );
    }

    #[test]
    fn default_project_never_recreates_a_registered_external_deletion() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let store = open_or_initialize_default_project(&root).expect("create default project");
        let project_id = store.manifest().project_id;
        drop(store);
        std::fs::remove_file(root.join(INITIAL_DOCUMENT)).expect("simulate external deletion");

        let reopened =
            open_or_initialize_default_project(&root).expect("reopen deleted manuscript project");
        assert_eq!(reopened.manifest().project_id, project_id);
        assert!(
            reopened
                .list_documents()
                .expect("list registered document")
                .iter()
                .any(|document| document.relative_path == INITIAL_DOCUMENT)
        );
        assert!(!root.join(INITIAL_DOCUMENT).exists());
    }

    #[cfg(unix)]
    #[test]
    fn default_project_refuses_visible_symlink_before_creating_sidecar() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let manuscript = root.join(INITIAL_DOCUMENT);
        std::fs::create_dir_all(manuscript.parent().expect("manuscript parent"))
            .expect("create manuscript directory");
        let outside = temporary.path().join("outside.md");
        let outside_bytes = b"outside remains untouched\n";
        std::fs::write(&outside, outside_bytes).expect("write outside target");
        symlink(&outside, &manuscript).expect("create visible symlink");

        let error = open_or_initialize_default_project(&root)
            .expect_err("default project must refuse visible symlink");
        assert_eq!(error.code, "default_document_symlink");
        assert!(!root.join(".loom").exists());
        assert_eq!(
            std::fs::read(&outside).expect("outside target survives"),
            outside_bytes
        );
    }

    #[test]
    fn close_cancels_active_family_waits_for_terminal_release_and_replays() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Closing Novel");
        let mut store = initialize_project(&root, "Closing Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id;
        let session_id = CommandId::new();
        let (run_id, branch_id, document_id) = start_persisted_test_generation(&mut store);
        let state = Arc::new(PluginState::default());
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
            session.agency.set_automation_enabled(true);
        }
        let request_id = "close-active-family".to_owned();
        let (signal, cancelled) = std::sync::mpsc::channel();
        let cancellation = Arc::new(RecordingCancellation {
            branches: Mutex::new(Vec::new()),
            signal: Mutex::new(Some(signal)),
        });
        state
            .generations
            .register(loom_host::GenerationFamilyRegistration {
                identity: GenerationFamilyIdentity {
                    request_id: request_id.clone(),
                    project_id,
                    session_id,
                    document_id,
                },
                branches: vec![(run_id, branch_id)],
                cancellation: cancellation.clone(),
            })
            .expect("register active family");
        let completing_state = Arc::clone(&state);
        let completing_identity = GenerationFamilyIdentity {
            request_id: request_id.clone(),
            project_id,
            session_id,
            document_id,
        };
        let completion = std::thread::spawn(move || {
            cancelled
                .recv_timeout(Duration::from_secs(1))
                .expect("close must request cancellation");
            terminalize_open_runs(
                &completing_state,
                &completing_identity,
                &[(run_id, branch_id)],
                "cancelled while closing",
            )
            .expect("persist terminal generation before release");
            release_family_after_terminal_persistence(
                &completing_state,
                &completing_identity,
                &[(run_id, branch_id)],
            )
            .expect("release terminal family");
        });

        let command_id = CommandId::new();
        let receipt = close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            command_id,
            Duration::from_secs(1),
        )
        .expect("close after cancellation terminalizes");
        completion.join().expect("completion thread");
        assert_eq!(
            *cancellation.branches.lock().expect("cancelled branches"),
            vec![branch_id]
        );
        assert_eq!(receipt.command_id, command_id.to_string());
        assert_eq!(
            state.session.lock().expect("session lock").phase,
            SessionPhase::Closed
        );
        let reopened = ProjectStore::open(&root).expect("reopen closed project");
        assert_eq!(
            reopened
                .generation_terminal_count(run_id)
                .expect("count durable terminal"),
            1
        );
        drop(reopened);

        let replay = close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            command_id,
            Duration::ZERO,
        )
        .expect("same close command replays");
        assert_eq!(replay.command_id, receipt.command_id);
        assert_eq!(replay.closed_at_unix_ms, receipt.closed_at_unix_ms);
    }

    #[test]
    fn close_timeout_is_bounded_and_leaves_session_revoked_for_exact_retry() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Slow Closing Novel");
        let store = initialize_project(&root, "Slow Closing Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id;
        let session_id = CommandId::new();
        let state = PluginState::default();
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
            session.agency.set_automation_enabled(true);
        }
        let run_id = GenerationRunId::new();
        let branch_id = BranchId::new();
        let cancellation = Arc::new(RecordingCancellation::default());
        state
            .generations
            .register(loom_host::GenerationFamilyRegistration {
                identity: GenerationFamilyIdentity {
                    request_id: "slow-close".to_owned(),
                    project_id,
                    session_id,
                    document_id: DocumentId::new(),
                },
                branches: vec![(run_id, branch_id)],
                cancellation: cancellation.clone(),
            })
            .expect("register active family");
        let window_id = ForegroundWindowId::new("main").expect("window identity");
        state
            .foreground_commands
            .observe_window_focus(window_id.clone(), true)
            .expect("focus fixture window");
        let foreground_binding = ForegroundCommandBinding {
            application_session_id: session_id,
            window_id,
            document_id: DocumentId::new(),
            candidate_fingerprint: BlobId::digest(b"close-timeout-candidate"),
            command_id: CommandId::new(),
            promotion_fingerprint: BlobId::digest(b"close-timeout-promotion"),
        };
        let foreground_challenge = state
            .foreground_commands
            .issue(foreground_binding.clone(), FOREGROUND_PROMOTION_TTL)
            .expect("issue foreground challenge before close");

        let error = close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            CommandId::new(),
            Duration::ZERO,
        )
        .expect_err("zero wait must return a retryable bounded result");
        assert_eq!(error.code, "generation_cancellation_in_progress");
        assert!(error.retryable);
        let session = state.session.lock().expect("session lock");
        assert_eq!(session.phase, SessionPhase::Open);
        assert!(session.agency.focus_mode());
        assert!(!session.agency.automation_enabled());
        drop(session);
        let native_focus = state
            .foreground_commands
            .bind_test_native_window_focus_sample(foreground_binding.window_id.clone(), true);
        assert_eq!(
            state.foreground_commands.consume_with_native_focus(
                loom_host::ForegroundCommandAttempt {
                    nonce: foreground_challenge.nonce,
                    binding: foreground_binding,
                },
                native_focus,
            ),
            Err(loom_host::ForegroundCommandError::StaleNonce),
            "close must revoke authority before a generation drain can time out",
        );
        assert_eq!(
            *cancellation.branches.lock().expect("cancelled branches"),
            vec![branch_id]
        );
        state
            .generations
            .complete_family("slow-close")
            .expect("release test family");
    }

    #[test]
    fn close_repairs_recorded_terminal_persistence_failure_before_releasing_store() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Persistence Repair Novel");
        let mut store =
            initialize_project(&root, "Persistence Repair Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id;
        let session_id = CommandId::new();
        let (run_id, branch_id, document_id) = start_persisted_test_generation(&mut store);
        let state = PluginState::default();
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
            session.agency.set_automation_enabled(true);
        }
        let request_id = "persistence-repair";
        state
            .generations
            .register(loom_host::GenerationFamilyRegistration {
                identity: GenerationFamilyIdentity {
                    request_id: request_id.to_owned(),
                    project_id,
                    session_id,
                    document_id,
                },
                branches: vec![(run_id, branch_id)],
                cancellation: Arc::new(RecordingCancellation::default()),
            })
            .expect("register failed-persistence family");
        state
            .generations
            .mark_terminal_persistence_failure(request_id, "simulated SQLite interruption")
            .expect("record persistence failure");

        close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            CommandId::new(),
            Duration::ZERO,
        )
        .expect("close repairs terminal before releasing project");
        assert_eq!(
            state.session.lock().expect("session lock").phase,
            SessionPhase::Closed
        );
        let reopened = ProjectStore::open(&root).expect("reopen repaired project");
        assert_eq!(
            reopened
                .generation_terminal_count(run_id)
                .expect("count repaired terminal"),
            1
        );
    }

    #[test]
    fn project_commands_reject_stale_session_and_cross_project_identity() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Bound Novel");
        let store = initialize_project(&root, "Bound Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id.to_string();
        let session_id = CommandId::new();
        let mut session = Session {
            phase: SessionPhase::Open,
            store: Some(store),
            active_session_id: Some(session_id),
            agency: AgencyGate::default(),
            last_close: None,
        };

        assert!(require_bound_store(&mut session, &project_id, &session_id.to_string()).is_ok());
        let stale = require_bound_store(&mut session, &project_id, &CommandId::new().to_string())
            .expect_err("stale session must fail");
        assert_eq!(stale.code, "stale_project_session");
        let foreign = require_bound_store(
            &mut session,
            &loom_types::ProjectId::new().to_string(),
            &session_id.to_string(),
        )
        .expect_err("foreign project must fail");
        assert_eq!(foreign.code, "project_identity_mismatch");
    }

    #[test]
    fn checkpoint_draft_version_preserves_decimal_u64_and_allows_lost_first_reply() {
        let first = parse_checkpoint_draft_version(Some("0".to_owned()))
            .expect("parse lost first acknowledgement")
            .expect("draft version");
        assert_eq!(first, 0);

        let maximum = parse_checkpoint_draft_version(Some(u64::MAX.to_string()))
            .expect("parse maximum u64")
            .expect("maximum version");
        assert_eq!(maximum, u64::MAX);
        assert!(parse_checkpoint_draft_version(Some("-1".to_owned())).is_err());
        assert_eq!(
            parse_checkpoint_draft_version(None).expect("no draft claim"),
            None
        );
    }

    #[test]
    fn project_summary_keeps_active_identity_across_external_change_and_deletion() {
        let fixture = ReconciliationFixture::new("one two\n");
        fixture.set_external("one two three\n");

        let changed = snapshot_for(&fixture.store, CommandId::new()).expect("changed snapshot");
        let summary = &changed.documents[0];
        assert_eq!(
            summary.active_blob_id.as_deref(),
            Some(fixture.base.blob_id.to_string().as_str())
        );
        assert_eq!(
            summary.revision_id.as_deref(),
            Some(fixture.base.revision_id.to_string().as_str())
        );
        assert_eq!(summary.word_count, 3);
        assert!(summary.externally_modified);
        let serialized = serde_json::to_value(summary).expect("serialize document summary");
        assert_eq!(
            serialized
                .get("active_blob_id")
                .and_then(|value| value.as_str()),
            Some(fixture.base.blob_id.to_string().as_str())
        );
        assert!(serialized.get("activeBlobId").is_none());

        std::fs::remove_file(fixture.store.root().join(INITIAL_DOCUMENT))
            .expect("delete visible document");
        let deleted = snapshot_for(&fixture.store, CommandId::new()).expect("deleted snapshot");
        let summary = &deleted.documents[0];
        assert_eq!(
            summary.active_blob_id.as_deref(),
            Some(fixture.base.blob_id.to_string().as_str())
        );
        assert_eq!(summary.word_count, 0);
        assert!(summary.externally_modified);
    }

    #[test]
    fn reconciliation_preview_returns_exact_canonical_inputs_and_bound_hashes() {
        let fixture = ReconciliationFixture::new("alpha\nmiddle\nomega\n");
        let external_visible = "alpha\r\nmiddle\r\nOMEGA\r\n";
        let external_visible_blob_id = fixture.set_external(external_visible);

        let preview = reconciliation_preview_for_store(
            &fixture.store,
            fixture.preview_request(Some("ALPHA\nmiddle\nomega\n")),
        )
        .expect("preview non-overlapping edits");

        assert_eq!(preview.project_id, fixture.project_id);
        assert_eq!(preview.session_id, fixture.session_id);
        assert_eq!(preview.document_id, fixture.base.document_id.to_string());
        assert_eq!(
            preview.active_revision_id,
            fixture.base.revision_id.to_string()
        );
        assert_eq!(preview.base_blob_id, fixture.base.blob_id.to_string());
        assert_eq!(preview.base_text, "alpha\nmiddle\nomega\n");
        assert_eq!(preview.app_text, "ALPHA\nmiddle\nomega\n");
        assert_eq!(preview.external_visible_text, external_visible);
        assert_eq!(preview.external_text, "alpha\nmiddle\nOMEGA\n");
        assert_eq!(
            preview.external_visible_blob_id,
            external_visible_blob_id.to_string()
        );
        assert_eq!(
            preview.external_blob_id,
            BlobId::digest(preview.external_text.as_bytes()).to_string()
        );
        assert_eq!(preview.app_source, ReconciliationAppSource::Caller);
        assert_eq!(preview.draft_version, None);
        assert_eq!(
            preview.outcome,
            MergeOutcome::Merged {
                content: "ALPHA\nmiddle\nOMEGA\n".to_owned()
            }
        );
        let serialized = serde_json::to_value(&preview).expect("serialize preview contract");
        assert_eq!(
            serialized
                .get("project_id")
                .and_then(|value| value.as_str()),
            Some(fixture.project_id.as_str())
        );
        assert_eq!(
            serialized
                .get("app_source")
                .and_then(|value| value.as_str()),
            Some("caller")
        );
        assert!(serialized.get("projectId").is_none());
        assert!(serialized.get("appSource").is_none());
    }

    #[test]
    fn reconciliation_preview_rejects_deleted_hybrid_and_unbound_inputs() {
        let fixture = ReconciliationFixture::new("bound base\n");
        fixture.set_external("bound external\n");

        let mut wrong_revision = fixture.preview_request(None);
        wrong_revision.expected_revision_id = RevisionId::new();
        assert_eq!(
            reconciliation_preview_for_store(&fixture.store, wrong_revision)
                .expect_err("wrong revision must fail")
                .code,
            "source_revision_conflict"
        );

        let mut unsafe_path = fixture.preview_request(None);
        unsafe_path.relative_path = "../outside.md".to_owned();
        assert_eq!(
            reconciliation_preview_for_store(&fixture.store, unsafe_path)
                .expect_err("path authority must not expand")
                .code,
            "unsafe_relative_path"
        );

        std::fs::remove_file(fixture.store.root().join(INITIAL_DOCUMENT))
            .expect("delete external document");
        assert_eq!(
            reconciliation_preview_for_store(&fixture.store, fixture.preview_request(None))
                .expect_err("deleted external document must fail")
                .code,
            "external_file_deleted"
        );

        let temporary = tempfile::tempdir().expect("hybrid project parent");
        let root = temporary.path().join("Hybrid Novel");
        let mut store = initialize_project(&root, "Hybrid Novel".to_owned()).expect("initialize");
        store
            .create_document_if_absent(
                "manuscript/mixed.md",
                DocumentContent::Hybrid(vec![loom_document::HybridBlock {
                    kind: loom_document::HybridBlockKind::Prose,
                    text: "mixed base\n".to_owned(),
                }]),
                "create hybrid fixture",
            )
            .expect("create hybrid document");
        let hybrid = store
            .read_document("manuscript/mixed.md")
            .expect("read hybrid document");
        std::fs::write(root.join("manuscript/mixed.md"), "mixed external\n")
            .expect("write hybrid external edit");
        let project_id = store.manifest().project_id.to_string();
        let error = reconciliation_preview_for_store(
            &store,
            PreviewRequest {
                project_id,
                session_id: CommandId::new().to_string(),
                document_id: hybrid.document_id.to_string(),
                relative_path: "manuscript/mixed.md".to_owned(),
                expected_revision_id: hybrid.revision_id,
                expected_base_blob_id: hybrid.blob_id,
                app_text: None,
            },
        )
        .expect_err("hybrid reconciliation must fail closed");
        assert_eq!(error.code, "hybrid_reconciliation_unsupported");
    }

    #[test]
    fn preview_uses_current_draft_and_reports_conflicts_without_writing() {
        let mut fixture = ReconciliationFixture::new("dawn over water\n");
        let draft = fixture
            .store
            .upsert_transient_draft(
                INITIAL_DOCUMENT,
                fixture.base.revision_id,
                0,
                DocumentContent::Prose("winter over water\n".to_owned()),
            )
            .expect("write current draft")
            .draft;
        let external = "summer over water\n";
        fixture.set_external(external);

        let preview =
            reconciliation_preview_for_store(&fixture.store, fixture.preview_request(None))
                .expect("preview competing edits");

        assert_eq!(preview.app_source, ReconciliationAppSource::TransientDraft);
        assert_eq!(preview.draft_version, Some(draft.version.to_string()));
        assert!(matches!(
            preview.outcome,
            MergeOutcome::Conflict { ref conflicts } if !conflicts.is_empty()
        ));
        assert_eq!(
            std::fs::read_to_string(fixture.store.root().join(INITIAL_DOCUMENT))
                .expect("read untouched external file"),
            external
        );
        assert_eq!(
            fixture
                .store
                .reconciliation_snapshot(INITIAL_DOCUMENT)
                .expect("snapshot after preview")
                .active_revision_id,
            fixture.base.revision_id
        );
        assert_eq!(
            fixture
                .store
                .load_transient_draft(INITIAL_DOCUMENT)
                .expect("load draft after preview")
                .expect("draft remains")
                .version,
            draft.version
        );
    }

    #[test]
    fn preview_rejects_a_draft_from_an_old_active_revision() {
        let mut fixture = ReconciliationFixture::new("first base\n");
        fixture
            .store
            .upsert_transient_draft(
                INITIAL_DOCUMENT,
                fixture.base.revision_id,
                0,
                DocumentContent::Prose("old-source draft\n".to_owned()),
            )
            .expect("write draft");
        fixture
            .store
            .save_document_if_source(
                INITIAL_DOCUMENT,
                DocumentContent::Prose("new active base\n".to_owned()),
                "advance active revision",
                fixture.base.revision_id,
                fixture.base.blob_id,
            )
            .expect("advance revision without clearing draft");
        fixture.base = fixture
            .store
            .read_document(INITIAL_DOCUMENT)
            .expect("load new active revision");
        fixture.set_external("external against new base\n");

        let error = reconciliation_preview_for_store(&fixture.store, fixture.preview_request(None))
            .expect_err("stale draft must block reconciliation preview");

        assert_eq!(error.code, "stale_transient_draft");
    }

    #[test]
    fn reconciliation_apply_is_replay_safe_and_never_clears_the_draft() {
        let mut fixture = ReconciliationFixture::new("base manuscript\n");
        let draft = fixture
            .store
            .upsert_transient_draft(
                INITIAL_DOCUMENT,
                fixture.base.revision_id,
                0,
                DocumentContent::Prose("recoverable app draft\n".to_owned()),
            )
            .expect("write recoverable draft")
            .draft;
        let external_blob_id = fixture.set_external("external manuscript\n");
        let command_id = CommandId::new();
        let wrong_hash_request = fixture.apply_request(
            BlobId::digest(b"not the external document"),
            "author resolved manuscript\n",
            CommandId::new(),
        );
        let wrong_hash = reconcile_apply_for_store(&mut fixture.store, wrong_hash_request)
            .expect_err("apply must bind the exact external visible hash");
        assert_eq!(wrong_hash.code, "external_file_conflict");
        assert_eq!(
            std::fs::read_to_string(fixture.store.root().join(INITIAL_DOCUMENT))
                .expect("read external file after rejected apply"),
            "external manuscript\n"
        );

        let first_request =
            fixture.apply_request(external_blob_id, "author resolved manuscript\n", command_id);
        let first = reconcile_apply_for_store(&mut fixture.store, first_request)
            .expect("apply explicit resolution");

        assert_eq!(first.command_kind, "reconcile_external");
        assert!(!first.replayed);
        assert_eq!(
            first.visible_projection,
            Some(VisibleProjectionState::Applied)
        );
        assert_eq!(
            fixture
                .store
                .read_document(INITIAL_DOCUMENT)
                .expect("read reconciled document")
                .text,
            "author resolved manuscript\n"
        );
        assert_eq!(
            fixture
                .store
                .load_transient_draft(INITIAL_DOCUMENT)
                .expect("load preserved draft")
                .expect("draft must remain explicit")
                .version,
            draft.version
        );

        let replay_request =
            fixture.apply_request(external_blob_id, "author resolved manuscript\n", command_id);
        let replay = reconcile_apply_for_store(&mut fixture.store, replay_request)
            .expect("replay a committed apply after a lost reply");
        assert!(replay.replayed);
        assert_eq!(
            replay.visible_projection,
            Some(VisibleProjectionState::Applied)
        );
        assert_eq!(replay.result_revision_id, first.result_revision_id);
        assert_eq!(replay.result_blob_id, first.result_blob_id);
        assert_eq!(
            fixture
                .store
                .load_transient_draft(INITIAL_DOCUMENT)
                .expect("load draft after replay")
                .expect("replay must not clear draft")
                .version,
            draft.version
        );
    }

    #[test]
    fn receipt_json_exposes_pending_projection_without_discarding_semantic_identity() {
        let receipt = Receipt {
            command_id: "01COMMAND".into(),
            command_kind: "checkpoint".into(),
            project_id: "01PROJECT".into(),
            schema_version: 4,
            source_revision_id: Some("01SOURCE".into()),
            result_revision_id: Some("01RESULT".into()),
            result_blob_id: Some("abc123".into()),
            request_fingerprint: Some("def456".into()),
            replayed: false,
            visible_projection: Some(VisibleProjectionState::PendingConflict {
                outbox_id: 17,
                relative_path: "manuscript/001-opening.md".into(),
            }),
            artifact_ids: vec!["01ARTIFACT".into()],
            completed_at_unix_ms: 42,
        };

        let value = serde_json::to_value(receipt).expect("serialize IPC receipt");
        assert_eq!(value["result_revision_id"], "01RESULT");
        assert_eq!(value["visible_projection"]["status"], "pending_conflict");
        assert_eq!(value["visible_projection"]["outbox_id"], 17);
        assert_eq!(
            value["visible_projection"]["relative_path"],
            "manuscript/001-opening.md"
        );
    }

    #[test]
    fn fixture_tauri_command_consumes_authority_at_native_edge() {
        let state = PluginState::default();
        let application_session_id = CommandId::new();
        state
            .session
            .lock()
            .expect("session lock")
            .active_session_id = Some(application_session_id);
        let window_id = ForegroundWindowId::new("main").expect("fixture window");
        state
            .foreground_commands
            .observe_window_focus(window_id.clone(), true)
            .expect("focus fixture window");
        let binding = loom_host::ForegroundCommandBinding {
            application_session_id,
            window_id: window_id.clone(),
            document_id: DocumentId::new(),
            candidate_fingerprint: BlobId::digest(b"fixture candidate"),
            command_id: CommandId::new(),
            promotion_fingerprint: BlobId::digest(b"fixture pending promotion"),
        };
        let challenge = state
            .foreground_commands
            .issue(binding.clone(), Duration::from_secs(30))
            .expect("issue fixture challenge");
        let input = FixtureForegroundCommandInput {
            nonce: challenge.nonce.to_string(),
            application_session_id: binding.application_session_id.to_string(),
            document_id: binding.document_id.to_string(),
            candidate_fingerprint: binding.candidate_fingerprint.to_string(),
            command_id: binding.command_id.to_string(),
            promotion_fingerprint: binding.promotion_fingerprint.to_string(),
        };
        let native_focus = state
            .foreground_commands
            .bind_test_native_window_focus_sample(window_id.clone(), true);
        let receipt = consume_fixture_foreground_command(&state, native_focus, &input)
            .expect("consume at fixture Tauri edge");
        assert_eq!(
            receipt.claim,
            "trusted_application_host_accepted_one_focused_command"
        );
        assert_eq!(receipt.monotonic_event_index, 1);
        let replay_focus = state
            .foreground_commands
            .bind_test_native_window_focus_sample(window_id, true);
        let replay = consume_fixture_foreground_command(&state, replay_focus, &input)
            .expect_err("replayed IPC payload must fail");
        assert_eq!(replay.code, "foreground_command_rejected");
    }

    #[test]
    fn production_research_packet_reader_admits_only_bounded_regular_files() {
        use loom_research_types::{
            MixedAuthorshipAssemblyId, MixedAuthorshipAssemblyRecord, OperationGraph,
            PipelineOperation, PipelineOperationId, PipelineOperationKind,
        };

        let temporary = tempfile::tempdir().expect("temporary packet directory");
        let packet_path = temporary.path().join("reviewed-research.json");
        let result_text = "Reviewed controller result";
        let output_operation_id = PipelineOperationId::new();
        let graph = OperationGraph::new(
            vec![
                PipelineOperation::new(
                    output_operation_id,
                    PipelineOperationKind::LiteralText {
                        content_blob_id: BlobId::digest(result_text.as_bytes()),
                    },
                    Vec::new(),
                )
                .expect("literal output"),
            ],
            output_operation_id,
        )
        .expect("operation graph");
        let record = MixedAuthorshipAssemblyRecord::new(
            MixedAuthorshipAssemblyId::new(),
            result_text.as_bytes(),
            graph,
        )
        .expect("mixed-authorship record");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema": RESEARCH_PROMOTION_PACKET_SCHEMA,
            "document_id": DocumentId::new(),
            "record": record,
            "result_text": result_text,
        }))
        .expect("packet JSON");
        std::fs::write(&packet_path, bytes).expect("write packet fixture");

        let packet = read_research_promotion_packet(&packet_path).expect("read bounded packet");
        assert_eq!(packet.schema, RESEARCH_PROMOTION_PACKET_SCHEMA);
        assert_eq!(packet.result_text, result_text);

        let directory_error = read_research_promotion_packet(temporary.path())
            .expect_err("directories cannot be promotion packets");
        assert_eq!(directory_error.code, "research_packet_not_regular_file");
    }

    #[test]
    // One end-to-end regression keeps lease staging, native consumption,
    // manuscript mutation, and replay rejection in the same proof.
    #[allow(clippy::too_many_lines)]
    fn production_research_confirmation_promotes_in_one_store_contract() {
        use loom_research_types::{
            MixedAuthorshipAssemblyId, MixedAuthorshipAssemblyRecord, OperationGraph,
            PipelineOperation, PipelineOperationId, PipelineOperationKind,
        };

        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Research Promotion Novel");
        let store =
            initialize_project(&root, "Research Promotion Novel".to_owned()).expect("initialize");
        let source = store
            .read_document(INITIAL_DOCUMENT)
            .expect("read source manuscript");
        let result_bytes = b"Foreground-authorized research result.";
        let output_operation_id = PipelineOperationId::new();
        let graph = OperationGraph::new(
            vec![
                PipelineOperation::new(
                    output_operation_id,
                    PipelineOperationKind::LiteralText {
                        content_blob_id: BlobId::digest(result_bytes),
                    },
                    Vec::new(),
                )
                .expect("literal output"),
            ],
            output_operation_id,
        )
        .expect("operation graph");
        let record = MixedAuthorshipAssemblyRecord::new(
            MixedAuthorshipAssemblyId::new(),
            result_bytes,
            graph,
        )
        .expect("mixed-authorship record");
        let project_id = store.manifest().project_id;
        let session_id = CommandId::new();
        let state = PluginState::default();
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
        }
        let window_id = ForegroundWindowId::new("main").expect("window ID");
        state
            .foreground_commands
            .observe_window_focus(window_id.clone(), true)
            .expect("focus main window");
        let prompt = stage_imported_research_promotion(
            &state,
            &project_id.to_string(),
            &session_id.to_string(),
            "main",
            ResearchPromotionPacket {
                schema: RESEARCH_PROMOTION_PACKET_SCHEMA.to_owned(),
                document_id: source.document_id.to_string(),
                record,
                result_text: String::from_utf8(result_bytes.to_vec()).expect("UTF-8 result"),
            },
        )
        .expect("import, admit, and stage production prompt");
        let input = ResearchPromotionConfirmInput {
            command_id: prompt.command_id,
            nonce: prompt.nonce,
            document_id: prompt.document_id,
            candidate_fingerprint: prompt.candidate_fingerprint,
            promotion_fingerprint: prompt.promotion_fingerprint,
        };
        let native_focus = state
            .foreground_commands
            .bind_test_native_window_focus_sample(window_id.clone(), true);
        let result = confirm_research_promotion(
            &state,
            &project_id.to_string(),
            &session_id.to_string(),
            native_focus,
            &input,
        )
        .expect("confirm production promotion");
        assert_eq!(
            result.receipt.result_blob_id,
            Some(BlobId::digest(result_bytes).to_string())
        );
        assert_eq!(
            result.receipt.visible_projection,
            Some(VisibleProjectionState::Applied)
        );
        let mut session = state.session.lock().expect("session lock");
        let promoted = session
            .store
            .as_mut()
            .expect("store")
            .read_document(INITIAL_DOCUMENT)
            .expect("read promoted manuscript");
        assert_eq!(promoted.text, "Foreground-authorized research result.");
        drop(session);
        let replay_focus = state
            .foreground_commands
            .bind_test_native_window_focus_sample(window_id, true);
        let replay = confirm_research_promotion(
            &state,
            &project_id.to_string(),
            &session_id.to_string(),
            replay_focus,
            &input,
        )
        .expect_err("the same command cannot be replayed");
        assert_eq!(replay.code, "research_promotion_not_pending");
    }
}
