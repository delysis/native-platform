use platform_contracts_v0::{ArtifactIdentityV0, ContentDigest, EvidenceClaimV0, TerminalClass};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0: &str = "delysis.vertical_fixture_manifest.v0";
pub const VERTICAL_OBSERVATION_SCHEMA_V0: &str = "delysis.vertical_observation.v0";
pub const VERTICAL_FIXTURE_LOCK_SCHEMA_V0: &str = "delysis.vertical_fixture_lock.v0";
pub const W1_CONTRACT_REVISION: &str = "cbab33555ab9355a6ac453d659c55ec9e0666821";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureClassV0 {
    ModelFree,
    Real,
    State,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerticalIdV0 {
    MomChatCancelRetry,
    MomAttachment,
    FteHostedFixtureLoopback,
    SpeechPeerCancellation,
    InformationInstallQuery,
    LoomSuggestionPromotion,
    LoomResearchDiagnosticAdmittedDistinction,
    QuitRelaunchFakeOwners,
    CurrentExactQwen,
    CurrentExactGemma,
    CurrentParakeetModelAudio,
    AppleInstalledVoice,
    MomPriorReleaseStore,
    LoomPriorProjectStore,
    FteLegacyDatabase,
    InformationResourceStore,
    CorruptedDisposableCaches,
    PartialPublicationStates,
}

pub const ALL_VERTICAL_IDS: [VerticalIdV0; 18] = [
    VerticalIdV0::MomChatCancelRetry,
    VerticalIdV0::MomAttachment,
    VerticalIdV0::FteHostedFixtureLoopback,
    VerticalIdV0::SpeechPeerCancellation,
    VerticalIdV0::InformationInstallQuery,
    VerticalIdV0::LoomSuggestionPromotion,
    VerticalIdV0::LoomResearchDiagnosticAdmittedDistinction,
    VerticalIdV0::QuitRelaunchFakeOwners,
    VerticalIdV0::CurrentExactQwen,
    VerticalIdV0::CurrentExactGemma,
    VerticalIdV0::CurrentParakeetModelAudio,
    VerticalIdV0::AppleInstalledVoice,
    VerticalIdV0::MomPriorReleaseStore,
    VerticalIdV0::LoomPriorProjectStore,
    VerticalIdV0::FteLegacyDatabase,
    VerticalIdV0::InformationResourceStore,
    VerticalIdV0::CorruptedDisposableCaches,
    VerticalIdV0::PartialPublicationStates,
];

impl VerticalIdV0 {
    #[must_use]
    pub const fn class(self) -> FixtureClassV0 {
        match self {
            Self::MomChatCancelRetry
            | Self::MomAttachment
            | Self::FteHostedFixtureLoopback
            | Self::SpeechPeerCancellation
            | Self::InformationInstallQuery
            | Self::LoomSuggestionPromotion
            | Self::LoomResearchDiagnosticAdmittedDistinction
            | Self::QuitRelaunchFakeOwners => FixtureClassV0::ModelFree,
            Self::CurrentExactQwen
            | Self::CurrentExactGemma
            | Self::CurrentParakeetModelAudio
            | Self::AppleInstalledVoice => FixtureClassV0::Real,
            Self::MomPriorReleaseStore
            | Self::LoomPriorProjectStore
            | Self::FteLegacyDatabase
            | Self::InformationResourceStore
            | Self::CorruptedDisposableCaches
            | Self::PartialPublicationStates => FixtureClassV0::State,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSourceV0 {
    pub repository_id: String,
    pub commit: String,
    pub production_tree: ArtifactIdentityV0,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAvailabilityV0 {
    CheckedIn,
    ExternalExact,
    RuntimeInventory,
    GeneratedFixture,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureArtifactV0 {
    pub identity: ArtifactIdentityV0,
    pub availability: ArtifactAvailabilityV0,
    pub relative_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateIdentityV0 {
    pub state_id: String,
    pub schema_id: String,
    pub baseline: FixtureArtifactV0,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrerequisiteKindV0 {
    ExactExternalArtifact,
    PlatformInventory,
    LocalRuntime,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrerequisiteV0 {
    pub prerequisite_id: String,
    pub kind: PrerequisiteKindV0,
    pub identity: ArtifactIdentityV0,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayProgramV0 {
    Cargo,
    Npm,
    RepositoryScript { relative_path: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkBoundaryV0 {
    Denied,
    LoopbackOnly,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRecipeV0 {
    pub program: ReplayProgramV0,
    pub argv: Vec<String>,
    pub required_environment: Vec<String>,
    pub network: NetworkBoundaryV0,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceDispositionV0 {
    Rejected,
    Unavailable,
    Superseded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegativeEvidenceV0 {
    pub evidence_id: String,
    pub disposition: EvidenceDispositionV0,
    pub artifact: ArtifactIdentityV0,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureCaseV0 {
    pub case_id: String,
    pub source: GitSourceV0,
    pub inputs: Vec<FixtureArtifactV0>,
    pub state_identities: Vec<StateIdentityV0>,
    pub prerequisites: Vec<PrerequisiteV0>,
    pub replay: Vec<ReplayRecipeV0>,
    pub expected_projection: ArtifactIdentityV0,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerticalFixtureManifestV0 {
    pub schema: String,
    pub vertical_id: VerticalIdV0,
    pub class: FixtureClassV0,
    pub contract_revision: String,
    pub cases: Vec<FixtureCaseV0>,
    pub omitted_claims: Vec<String>,
    pub negative_evidence: Vec<NegativeEvidenceV0>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFactV0 {
    pub sequence: u64,
    pub operation_id: String,
    pub attempt_id: Option<String>,
    pub correlation_id: Option<String>,
    pub kind: String,
    pub payload: Option<ArtifactIdentityV0>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateDispositionV0 {
    Unchanged,
    Created,
    Updated,
    Removed,
    Quarantined,
    Recovered,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableStateFactV0 {
    pub state_id: String,
    pub schema_id: String,
    pub before: Option<ArtifactIdentityV0>,
    pub after: Option<ArtifactIdentityV0>,
    pub disposition: StateDispositionV0,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleFactV0 {
    pub operation_id: String,
    pub attempt_id: Option<String>,
    pub correlation_id: Option<String>,
    pub terminal: TerminalClass,
    pub released: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipFactsV0 {
    pub active_operations: usize,
    pub retained_tasks: usize,
    pub expected_workers: usize,
    pub joined_workers: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FactValueV0 {
    Boolean(bool),
    Integer(i64),
    Text(String),
    Digest(ContentDigest),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceProjectionV0 {
    pub ordered_events: Vec<EventFactV0>,
    pub durable_state: Vec<DurableStateFactV0>,
    pub lifecycle: Vec<LifecycleFactV0>,
    pub ownership: OwnershipFactsV0,
    pub output_facts: BTreeMap<String, FactValueV0>,
    pub fail_closed_facts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationEnvelopeV0 {
    pub schema: String,
    pub vertical_id: VerticalIdV0,
    pub case_id: String,
    pub implementation_revision: String,
    pub observed_prerequisites: Vec<PrerequisiteV0>,
    pub evidence: EvidenceClaimV0,
    pub projection: EquivalenceProjectionV0,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Caller-owned bytes for one exact-external prerequisite.
pub struct PrerequisiteArtifactBytesV0<'a> {
    /// Matches [`PrerequisiteV0::prerequisite_id`] in the selected case.
    pub prerequisite_id: &'a str,
    /// Bytes authenticated against the manifest identity; never retained.
    pub bytes: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerticalFixtureLockEntryV0 {
    pub vertical_id: VerticalIdV0,
    pub class: FixtureClassV0,
    pub manifest: ArtifactIdentityV0,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerticalFixtureLockV0 {
    pub schema: String,
    pub protocol_commit: String,
    pub contract_revision: String,
    pub entries: Vec<VerticalFixtureLockEntryV0>,
}
