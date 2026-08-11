use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use loom_research_types::{
    CampaignId, FrozenTrialStage, StageId, TrialCaseId, TrialRunId, TrialRunOrigin,
};
use loom_types::{ArtifactId, BlobId, ProjectId};
use rusqlite::{Connection, OptionalExtension};

use crate::store::ProjectLease;
use crate::{ResearchBudgetMaximum, Result, StoreError};

const MAX_SNAPSHOT_CAMPAIGN_TRIALS: usize = 65_536;
const MAX_SNAPSHOT_TRIAL_DEPENDENCIES: usize = 256;
const MAX_SNAPSHOT_TRIAL_STAGES: usize = FrozenTrialStage::ALL.len();
const MAX_SNAPSHOT_STAGE_DEPENDENCIES: usize = 8;

/// Which live headless engine owns one exact frozen research subject.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ResearchSessionKind {
    Campaign,
    Trial,
}

impl ResearchSessionKind {
    pub(crate) const fn domain_tag(self) -> u8 {
        match self {
            Self::Campaign => 0,
            Self::Trial => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ResearchSessionKey {
    pub(crate) kind: ResearchSessionKind,
    pub(crate) subject_fingerprint: BlobId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResearchSubjectLocator {
    Campaign(BlobId),
    TrialRun(TrialRunId),
}

impl ResearchSubjectLocator {
    pub(crate) const fn kind(self) -> ResearchSessionKind {
        match self {
            Self::Campaign(_) => ResearchSessionKind::Campaign,
            Self::TrialRun(_) => ResearchSessionKind::Trial,
        }
    }

    pub(crate) fn subject_fingerprint(self) -> BlobId {
        match self {
            Self::Campaign(fingerprint) => fingerprint,
            Self::TrialRun(run_id) => trial_run_subject_fingerprint(run_id),
        }
    }

    pub(crate) const fn trial_run_id(self) -> Option<TrialRunId> {
        match self {
            Self::Campaign(_) => None,
            Self::TrialRun(run_id) => Some(run_id),
        }
    }
}

pub(crate) fn trial_run_subject_fingerprint(run_id: TrialRunId) -> BlobId {
    let mut material = Vec::with_capacity(42);
    material.extend_from_slice(b"loom/trial-run-subject/v1\0");
    material.extend_from_slice(&run_id.as_ulid().to_bytes());
    BlobId::digest(&material)
}

pub(crate) struct ResearchSessionRegistryState {
    pub(crate) active: Mutex<BTreeSet<ResearchSessionKey>>,
    // A live store or subject lease retains the operating-system project lock.
    _project_lease: Arc<ProjectLease>,
}

pub(crate) type ResearchSessionRegistry = Arc<ResearchSessionRegistryState>;

impl ResearchSessionRegistryState {
    pub(crate) fn new(project_lease: Arc<ProjectLease>) -> ResearchSessionRegistry {
        Arc::new(Self {
            active: Mutex::new(BTreeSet::new()),
            _project_lease: project_lease,
        })
    }
}

/// Exact normalized campaign row plus all currently persisted trial nodes.
///
/// This is an inspectable fact, not authority. Its fields are store-owned and
/// have no public constructor; a campaign verifier must compare every field
/// with its private frozen specification before it can execute.
#[derive(Debug, Eq, PartialEq)]
pub struct PersistedCampaignSubjectSnapshot {
    campaign_id: CampaignId,
    campaign_fingerprint: BlobId,
    project_id: ProjectId,
    manifest_source_fingerprint: BlobId,
    manifest_fingerprint: BlobId,
    project_input_fingerprint: BlobId,
    seed: u64,
    maximum: ResearchBudgetMaximum,
    record_fingerprint: BlobId,
    trials: Vec<PersistedCampaignTrialSnapshot>,
}

impl PersistedCampaignSubjectSnapshot {
    pub const fn campaign_id(&self) -> CampaignId {
        self.campaign_id
    }

    pub const fn campaign_fingerprint(&self) -> BlobId {
        self.campaign_fingerprint
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn manifest_source_fingerprint(&self) -> BlobId {
        self.manifest_source_fingerprint
    }

    pub const fn manifest_fingerprint(&self) -> BlobId {
        self.manifest_fingerprint
    }

    pub const fn project_input_fingerprint(&self) -> BlobId {
        self.project_input_fingerprint
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    pub const fn maximum(&self) -> ResearchBudgetMaximum {
        self.maximum
    }

    pub const fn record_fingerprint(&self) -> BlobId {
        self.record_fingerprint
    }

    pub fn trials(&self) -> &[PersistedCampaignTrialSnapshot] {
        &self.trials
    }
}

/// One normalized trial node used by a frozen campaign topology.
#[derive(Debug, Eq, PartialEq)]
pub struct PersistedCampaignTrialSnapshot {
    trial_fingerprint: BlobId,
    trial_case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    maximum: ResearchBudgetMaximum,
    dependencies: Vec<BlobId>,
}

impl PersistedCampaignTrialSnapshot {
    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn trial_case_id(&self) -> TrialCaseId {
        self.trial_case_id
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub const fn maximum(&self) -> ResearchBudgetMaximum {
        self.maximum
    }

    pub fn dependencies(&self) -> &[BlobId] {
        &self.dependencies
    }
}

/// Exact normalized trial row and the complete ordered twelve-stage protocol.
#[derive(Debug, Eq, PartialEq)]
pub struct PersistedTrialSubjectSnapshot {
    trial_run_id: TrialRunId,
    run_origin: TrialRunOrigin,
    run_record_fingerprint: BlobId,
    campaign_id: CampaignId,
    project_id: ProjectId,
    trial_fingerprint: BlobId,
    trial_case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    prompt_content_fingerprint: BlobId,
    model_binding_fingerprint: BlobId,
    expected_writer_call_count: u16,
    declared_writer_token_maximum: u64,
    maximum: ResearchBudgetMaximum,
    trial_record_fingerprint: BlobId,
    stages: Vec<PersistedTrialStageSnapshot>,
}

impl PersistedTrialSubjectSnapshot {
    pub const fn trial_run_id(&self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn run_origin(&self) -> TrialRunOrigin {
        self.run_origin
    }

    pub const fn run_record_fingerprint(&self) -> BlobId {
        self.run_record_fingerprint
    }

    pub const fn campaign_id(&self) -> CampaignId {
        self.campaign_id
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn trial_case_id(&self) -> TrialCaseId {
        self.trial_case_id
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub const fn prompt_content_fingerprint(&self) -> BlobId {
        self.prompt_content_fingerprint
    }

    pub const fn model_binding_fingerprint(&self) -> BlobId {
        self.model_binding_fingerprint
    }

    pub const fn expected_writer_call_count(&self) -> u16 {
        self.expected_writer_call_count
    }

    pub const fn declared_writer_token_maximum(&self) -> u64 {
        self.declared_writer_token_maximum
    }

    pub const fn maximum(&self) -> ResearchBudgetMaximum {
        self.maximum
    }

    pub const fn trial_record_fingerprint(&self) -> BlobId {
        self.trial_record_fingerprint
    }

    pub fn stages(&self) -> &[PersistedTrialStageSnapshot] {
        &self.stages
    }
}

/// One exact normalized stage row and its dependency order.
#[derive(Debug, Eq, PartialEq)]
pub struct PersistedTrialStageSnapshot {
    stage_id: StageId,
    stage: FrozenTrialStage,
    stage_spec_fingerprint: BlobId,
    maximum: ResearchBudgetMaximum,
    record_fingerprint: BlobId,
    dependencies: Vec<StageId>,
}

impl PersistedTrialStageSnapshot {
    pub const fn stage_id(&self) -> StageId {
        self.stage_id
    }

    pub const fn stage(&self) -> FrozenTrialStage {
        self.stage
    }

    pub const fn stage_spec_fingerprint(&self) -> BlobId {
        self.stage_spec_fingerprint
    }

    pub const fn maximum(&self) -> ResearchBudgetMaximum {
        self.maximum
    }

    pub const fn record_fingerprint(&self) -> BlobId {
        self.record_fingerprint
    }

    pub fn dependencies(&self) -> &[StageId] {
        &self.dependencies
    }
}

/// Store-owned normalized evidence for the exact leased subject.
#[derive(Debug, Eq, PartialEq)]
pub enum PersistedResearchSubjectSnapshot {
    Campaign(PersistedCampaignSubjectSnapshot),
    Trial(PersistedTrialSubjectSnapshot),
}

impl PersistedResearchSubjectSnapshot {
    pub const fn project_id(&self) -> ProjectId {
        match self {
            Self::Campaign(snapshot) => snapshot.project_id(),
            Self::Trial(snapshot) => snapshot.project_id(),
        }
    }

    pub const fn record_fingerprint(&self) -> BlobId {
        match self {
            Self::Campaign(snapshot) => snapshot.record_fingerprint(),
            Self::Trial(snapshot) => snapshot.run_record_fingerprint(),
        }
    }
}

/// Process-local exclusive ownership of a persisted frozen campaign or trial.
///
/// The constructor is store-private, this value is not cloneable or
/// serializable, and dropping it releases the exact subject. This proves only
/// store/project exclusivity: it does not attest that caller-supplied record
/// bytes encode the claimed private frozen spec. The consuming trial/campaign
/// verifier must match project, subject, and canonical record fingerprints
/// before creating its own execution authority.
///
/// ```compile_fail
/// use loom_store::ExclusiveResearchSessionLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<ExclusiveResearchSessionLease>();
/// ```
///
/// ```compile_fail
/// use loom_store::ExclusiveResearchSessionLease;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<ExclusiveResearchSessionLease>();
/// ```
#[must_use]
pub struct ExclusiveResearchSessionLease {
    key: ResearchSessionKey,
    record_fingerprint: BlobId,
    project_id: ProjectId,
    snapshot: PersistedResearchSubjectSnapshot,
    trial_run_id: Option<TrialRunId>,
    session_id: ArtifactId,
    lease_fingerprint: BlobId,
    registry: ResearchSessionRegistry,
}

pub(crate) struct ExclusiveResearchSessionLeaseInput {
    pub key: ResearchSessionKey,
    pub record_fingerprint: BlobId,
    pub project_id: ProjectId,
    pub snapshot: PersistedResearchSubjectSnapshot,
    pub trial_run_id: Option<TrialRunId>,
    pub session_id: ArtifactId,
    pub lease_fingerprint: BlobId,
    pub registry: ResearchSessionRegistry,
}

impl ExclusiveResearchSessionLease {
    pub(crate) fn new(input: ExclusiveResearchSessionLeaseInput) -> Self {
        let ExclusiveResearchSessionLeaseInput {
            key,
            record_fingerprint,
            project_id,
            snapshot,
            trial_run_id,
            session_id,
            lease_fingerprint,
            registry,
        } = input;
        Self {
            key,
            record_fingerprint,
            project_id,
            snapshot,
            trial_run_id,
            session_id,
            lease_fingerprint,
            registry,
        }
    }

    pub const fn kind(&self) -> ResearchSessionKind {
        self.key.kind
    }

    pub const fn subject_fingerprint(&self) -> BlobId {
        self.key.subject_fingerprint
    }

    pub const fn trial_run_id(&self) -> Option<TrialRunId> {
        self.trial_run_id
    }

    pub const fn session_id(&self) -> ArtifactId {
        self.session_id
    }

    /// Fingerprint of the exact canonical persisted bytes. The consuming
    /// trial/campaign crate must reserialize its private frozen spec and match
    /// this value before treating the lease as execution authority.
    pub const fn record_fingerprint(&self) -> BlobId {
        self.record_fingerprint
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    /// Complete bounded normalized rows read under the project store lease.
    /// This remains evidence only; the consuming engine must compare every
    /// field with its private frozen specification.
    pub const fn snapshot(&self) -> &PersistedResearchSubjectSnapshot {
        &self.snapshot
    }

    pub const fn lease_fingerprint(&self) -> BlobId {
        self.lease_fingerprint
    }
}

impl fmt::Debug for ExclusiveResearchSessionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExclusiveResearchSessionLease")
            .field("kind", &self.key.kind)
            .field("subject_fingerprint", &self.key.subject_fingerprint)
            .field("record_fingerprint", &self.record_fingerprint)
            .field("trial_run_id", &self.trial_run_id)
            .field("project_id", &self.project_id)
            .field("session_id", &self.session_id)
            .field("lease_fingerprint", &self.lease_fingerprint)
            .finish_non_exhaustive()
    }
}

impl Drop for ExclusiveResearchSessionLease {
    fn drop(&mut self) {
        self.registry
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

pub(crate) fn load_research_subject_snapshot(
    connection: &Connection,
    locator: ResearchSubjectLocator,
) -> Result<Option<PersistedResearchSubjectSnapshot>> {
    match locator {
        ResearchSubjectLocator::Campaign(subject_fingerprint) => {
            load_campaign_snapshot(connection, subject_fingerprint)
                .map(|snapshot| snapshot.map(PersistedResearchSubjectSnapshot::Campaign))
        }
        ResearchSubjectLocator::TrialRun(trial_run_id) => {
            load_trial_snapshot(connection, trial_run_id)
                .map(|snapshot| snapshot.map(PersistedResearchSubjectSnapshot::Trial))
        }
    }
}

#[derive(Debug)]
struct RawCampaignHead {
    campaign_id: String,
    project_id: String,
    manifest_source_fingerprint: String,
    manifest_fingerprint: String,
    project_input_fingerprint: String,
    seed: String,
    maximum: [i64; 4],
    record_fingerprint: String,
}

fn load_campaign_snapshot(
    connection: &Connection,
    campaign_fingerprint: BlobId,
) -> Result<Option<PersistedCampaignSubjectSnapshot>> {
    let raw = connection
        .query_row(
            "SELECT campaign_id, project_id, manifest_source_blob_id,
                    manifest_fingerprint, project_input_fingerprint, seed_decimal,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms, record_fingerprint
             FROM research_campaigns WHERE campaign_fingerprint = ?1",
            [campaign_fingerprint.to_string()],
            |row| {
                Ok(RawCampaignHead {
                    campaign_id: row.get(0)?,
                    project_id: row.get(1)?,
                    manifest_source_fingerprint: row.get(2)?,
                    manifest_fingerprint: row.get(3)?,
                    project_input_fingerprint: row.get(4)?,
                    seed: row.get(5)?,
                    maximum: [row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?],
                    record_fingerprint: row.get(10)?,
                })
            },
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let campaign_id = parse_sql_id(&raw.campaign_id, "campaign_id")?;
    let trials = load_campaign_trials(connection, campaign_id)?;
    Ok(Some(PersistedCampaignSubjectSnapshot {
        campaign_id,
        campaign_fingerprint,
        project_id: parse_sql_id(&raw.project_id, "project_id")?,
        manifest_source_fingerprint: parse_sql_digest(
            &raw.manifest_source_fingerprint,
            "manifest_source_blob_id",
        )?,
        manifest_fingerprint: parse_sql_digest(&raw.manifest_fingerprint, "manifest_fingerprint")?,
        project_input_fingerprint: parse_sql_digest(
            &raw.project_input_fingerprint,
            "project_input_fingerprint",
        )?,
        seed: raw
            .seed
            .parse()
            .map_err(|error| corrupt_snapshot(format!("invalid campaign seed_decimal: {error}")))?,
        maximum: parse_sql_budget(raw.maximum, "campaign maximum")?,
        record_fingerprint: parse_sql_digest(
            &raw.record_fingerprint,
            "campaign record_fingerprint",
        )?,
        trials,
    }))
}

fn load_campaign_trials(
    connection: &Connection,
    campaign_id: CampaignId,
) -> Result<Vec<PersistedCampaignTrialSnapshot>> {
    let mut dependencies = load_campaign_trial_dependencies(connection, campaign_id)?;
    let mut statement = connection.prepare(
        "SELECT trial_fingerprint, trial_case_id, treatment_fingerprint,
                maximum_writer_tokens, maximum_controller_tokens,
                maximum_evaluations, maximum_wall_time_ms
         FROM research_trial_specs
         WHERE campaign_id = ?1 ORDER BY trial_fingerprint",
    )?;
    let mut rows = statement.query([campaign_id.to_string()])?;
    let mut trials = Vec::new();
    while let Some(row) = rows.next()? {
        if trials.len() == MAX_SNAPSHOT_CAMPAIGN_TRIALS {
            return Err(corrupt_snapshot(
                "campaign trial snapshot exceeds its bound",
            ));
        }
        let trial_fingerprint =
            parse_sql_digest(&row.get::<_, String>(0)?, "campaign trial_fingerprint")?;
        trials.push(PersistedCampaignTrialSnapshot {
            trial_fingerprint,
            trial_case_id: parse_sql_id(&row.get::<_, String>(1)?, "campaign trial_case_id")?,
            treatment_fingerprint: parse_sql_digest(
                &row.get::<_, String>(2)?,
                "campaign treatment_fingerprint",
            )?,
            maximum: parse_sql_budget(
                [row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?],
                "campaign trial maximum",
            )?,
            dependencies: dependencies.remove(&trial_fingerprint).unwrap_or_default(),
        });
    }
    if !dependencies.is_empty() {
        return Err(corrupt_snapshot(
            "campaign dependency rows reference a trial outside the campaign snapshot",
        ));
    }
    Ok(trials)
}

fn load_campaign_trial_dependencies(
    connection: &Connection,
    campaign_id: CampaignId,
) -> Result<BTreeMap<BlobId, Vec<BlobId>>> {
    let mut statement = connection.prepare(
        "SELECT dependency.trial_fingerprint, dependency.dependency_trial_fingerprint
         FROM research_campaign_trial_dependencies dependency
         JOIN research_trial_specs trial
           ON trial.trial_fingerprint = dependency.trial_fingerprint
         WHERE trial.campaign_id = ?1
         ORDER BY dependency.trial_fingerprint, dependency.dependency_ordinal",
    )?;
    let mut rows = statement.query([campaign_id.to_string()])?;
    let mut by_trial = BTreeMap::<BlobId, Vec<BlobId>>::new();
    while let Some(row) = rows.next()? {
        let trial = parse_sql_digest(&row.get::<_, String>(0)?, "dependency trial_fingerprint")?;
        let entries = by_trial.entry(trial).or_default();
        if entries.len() == MAX_SNAPSHOT_TRIAL_DEPENDENCIES {
            return Err(corrupt_snapshot(
                "campaign trial dependency snapshot exceeds its bound",
            ));
        }
        entries.push(parse_sql_digest(
            &row.get::<_, String>(1)?,
            "dependency_trial_fingerprint",
        )?);
    }
    Ok(by_trial)
}

#[derive(Debug)]
struct RawTrialHead {
    trial_fingerprint: String,
    origin_kind: String,
    origin_campaign_id: Option<String>,
    origin_campaign_fingerprint: Option<String>,
    origin_benchmark_run_id: Option<String>,
    origin_benchmark_seal_fingerprint: Option<String>,
    origin_benchmark_assignment_fingerprint: Option<String>,
    run_record_fingerprint: String,
    campaign_id: String,
    project_id: String,
    trial_case_id: String,
    treatment_fingerprint: String,
    prompt_content_fingerprint: String,
    model_binding_fingerprint: String,
    expected_writer_call_count: i64,
    declared_writer_token_maximum: i64,
    maximum: [i64; 4],
    record_fingerprint: String,
}

fn load_trial_snapshot(
    connection: &Connection,
    trial_run_id: TrialRunId,
) -> Result<Option<PersistedTrialSubjectSnapshot>> {
    query_trial_head(connection, trial_run_id)?
        .map(|raw| decode_trial_snapshot(connection, trial_run_id, &raw))
        .transpose()
}

fn query_trial_head(
    connection: &Connection,
    trial_run_id: TrialRunId,
) -> Result<Option<RawTrialHead>> {
    connection
        .query_row(
            "SELECT run.trial_fingerprint, run.origin_kind, run.origin_campaign_id,
                    origin_campaign.campaign_fingerprint,
                    run.origin_benchmark_run_id,
                    run.origin_benchmark_seal_fingerprint,
                    run.origin_benchmark_assignment_fingerprint,
                    run.record_fingerprint,
                    trial.campaign_id, campaign.project_id, trial.trial_case_id,
                    trial.treatment_fingerprint, trial.prompt_content_fingerprint,
                    trial.model_binding_fingerprint,
                    trial.expected_writer_call_count,
                    trial.declared_writer_token_maximum,
                    trial.maximum_writer_tokens, trial.maximum_controller_tokens,
                    trial.maximum_evaluations, trial.maximum_wall_time_ms,
                    trial.record_fingerprint
             FROM research_trial_specs trial
             JOIN research_campaigns campaign USING (campaign_id)
             JOIN research_trial_runs run USING (trial_fingerprint)
             LEFT JOIN research_campaigns origin_campaign
               ON origin_campaign.campaign_id = run.origin_campaign_id
             WHERE run.trial_run_id = ?1",
            [trial_run_id.to_string()],
            |row| {
                Ok(RawTrialHead {
                    trial_fingerprint: row.get(0)?,
                    origin_kind: row.get(1)?,
                    origin_campaign_id: row.get(2)?,
                    origin_campaign_fingerprint: row.get(3)?,
                    origin_benchmark_run_id: row.get(4)?,
                    origin_benchmark_seal_fingerprint: row.get(5)?,
                    origin_benchmark_assignment_fingerprint: row.get(6)?,
                    run_record_fingerprint: row.get(7)?,
                    campaign_id: row.get(8)?,
                    project_id: row.get(9)?,
                    trial_case_id: row.get(10)?,
                    treatment_fingerprint: row.get(11)?,
                    prompt_content_fingerprint: row.get(12)?,
                    model_binding_fingerprint: row.get(13)?,
                    expected_writer_call_count: row.get(14)?,
                    declared_writer_token_maximum: row.get(15)?,
                    maximum: [row.get(16)?, row.get(17)?, row.get(18)?, row.get(19)?],
                    record_fingerprint: row.get(20)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn decode_trial_snapshot(
    connection: &Connection,
    trial_run_id: TrialRunId,
    raw: &RawTrialHead,
) -> Result<PersistedTrialSubjectSnapshot> {
    let trial_fingerprint = parse_sql_digest(&raw.trial_fingerprint, "trial_fingerprint")?;
    let run_origin = match (
        raw.origin_kind.as_str(),
        raw.origin_campaign_id.as_deref(),
        raw.origin_campaign_fingerprint.as_deref(),
        raw.origin_benchmark_run_id.as_deref(),
        raw.origin_benchmark_seal_fingerprint.as_deref(),
        raw.origin_benchmark_assignment_fingerprint.as_deref(),
    ) {
        ("campaign", Some(campaign_id), Some(campaign_fingerprint), None, None, None) => {
            TrialRunOrigin::Campaign {
                campaign_id: parse_sql_id(campaign_id, "run origin_campaign_id")?,
                campaign_fingerprint: parse_sql_digest(
                    campaign_fingerprint,
                    "run origin campaign_fingerprint",
                )?,
            }
        }
        ("standalone", None, None, None, None, None) => TrialRunOrigin::Standalone,
        ("benchmark", None, None, Some(run_id), Some(seal), Some(assignment)) => {
            TrialRunOrigin::Benchmark {
                benchmark_run_id: parse_sql_id(run_id, "origin benchmark_run_id")?,
                seal_fingerprint: parse_sql_digest(seal, "origin benchmark seal")?,
                assignment_fingerprint: parse_sql_digest(
                    assignment,
                    "origin benchmark assignment",
                )?,
            }
        }
        _ => return Err(corrupt_snapshot("invalid trial-run origin binding")),
    };
    Ok(PersistedTrialSubjectSnapshot {
        trial_run_id,
        run_origin,
        run_record_fingerprint: parse_sql_digest(
            &raw.run_record_fingerprint,
            "trial run record_fingerprint",
        )?,
        campaign_id: parse_sql_id(&raw.campaign_id, "trial campaign_id")?,
        project_id: parse_sql_id(&raw.project_id, "trial project_id")?,
        trial_fingerprint,
        trial_case_id: parse_sql_id(&raw.trial_case_id, "trial_case_id")?,
        treatment_fingerprint: parse_sql_digest(
            &raw.treatment_fingerprint,
            "trial treatment_fingerprint",
        )?,
        prompt_content_fingerprint: parse_sql_digest(
            &raw.prompt_content_fingerprint,
            "trial prompt_content_fingerprint",
        )?,
        model_binding_fingerprint: parse_sql_digest(
            &raw.model_binding_fingerprint,
            "trial model_binding_fingerprint",
        )?,
        expected_writer_call_count: parse_sql_nonzero_u16(
            raw.expected_writer_call_count,
            "trial expected_writer_call_count",
        )?,
        declared_writer_token_maximum: parse_sql_u64(
            raw.declared_writer_token_maximum,
            "declared_writer_token_maximum",
        )?,
        maximum: parse_sql_budget(raw.maximum, "trial maximum")?,
        trial_record_fingerprint: parse_sql_digest(
            &raw.record_fingerprint,
            "trial record_fingerprint",
        )?,
        stages: load_trial_stages(connection, trial_fingerprint)?,
    })
}

fn load_trial_stages(
    connection: &Connection,
    trial_fingerprint: BlobId,
) -> Result<Vec<PersistedTrialStageSnapshot>> {
    let mut dependencies = load_stage_dependencies(connection, trial_fingerprint)?;
    let mut statement = connection.prepare(
        "SELECT stage_id, stage_kind, stage_spec_fingerprint,
                maximum_writer_tokens, maximum_controller_tokens,
                maximum_evaluations, maximum_wall_time_ms, record_fingerprint
         FROM research_campaign_stage_specs
         WHERE trial_fingerprint = ?1 ORDER BY stage_ordinal",
    )?;
    let mut rows = statement.query([trial_fingerprint.to_string()])?;
    let mut stages = Vec::new();
    while let Some(row) = rows.next()? {
        if stages.len() == MAX_SNAPSHOT_TRIAL_STAGES {
            return Err(corrupt_snapshot("trial stage snapshot exceeds its bound"));
        }
        let stage_id = parse_sql_id(&row.get::<_, String>(0)?, "stage_id")?;
        stages.push(PersistedTrialStageSnapshot {
            stage_id,
            stage: parse_stage_kind(&row.get::<_, String>(1)?)?,
            stage_spec_fingerprint: parse_sql_digest(
                &row.get::<_, String>(2)?,
                "stage_spec_fingerprint",
            )?,
            maximum: parse_sql_budget(
                [row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?],
                "stage maximum",
            )?,
            record_fingerprint: parse_sql_digest(
                &row.get::<_, String>(7)?,
                "stage record_fingerprint",
            )?,
            dependencies: dependencies.remove(&stage_id).unwrap_or_default(),
        });
    }
    if !dependencies.is_empty() {
        return Err(corrupt_snapshot(
            "stage dependency rows reference a stage outside the trial snapshot",
        ));
    }
    Ok(stages)
}

fn load_stage_dependencies(
    connection: &Connection,
    trial_fingerprint: BlobId,
) -> Result<BTreeMap<StageId, Vec<StageId>>> {
    let mut statement = connection.prepare(
        "SELECT dependency.stage_id, dependency.dependency_stage_id
         FROM research_campaign_stage_dependencies dependency
         JOIN research_campaign_stage_specs stage ON stage.stage_id = dependency.stage_id
         WHERE stage.trial_fingerprint = ?1
         ORDER BY dependency.stage_id, dependency.dependency_ordinal",
    )?;
    let mut rows = statement.query([trial_fingerprint.to_string()])?;
    let mut by_stage = BTreeMap::<StageId, Vec<StageId>>::new();
    while let Some(row) = rows.next()? {
        let stage_id = parse_sql_id(&row.get::<_, String>(0)?, "dependency stage_id")?;
        let entries = by_stage.entry(stage_id).or_default();
        if entries.len() == MAX_SNAPSHOT_STAGE_DEPENDENCIES {
            return Err(corrupt_snapshot(
                "stage dependency snapshot exceeds its bound",
            ));
        }
        entries.push(parse_sql_id(
            &row.get::<_, String>(1)?,
            "dependency_stage_id",
        )?);
    }
    Ok(by_stage)
}

fn parse_sql_budget(values: [i64; 4], field: &str) -> Result<ResearchBudgetMaximum> {
    Ok(ResearchBudgetMaximum {
        writer_tokens: parse_sql_u64(values[0], field)?,
        controller_tokens: parse_sql_u64(values[1], field)?,
        evaluations: u32::try_from(values[2])
            .map_err(|_| corrupt_snapshot(format!("{field} evaluations are out of range")))?,
        wall_time_ms: parse_sql_u64(values[3], field)?,
    })
}

fn parse_sql_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| corrupt_snapshot(format!("{field} is negative")))
}

fn parse_sql_nonzero_u16(value: i64, field: &str) -> Result<u16> {
    let value = u16::try_from(value)
        .map_err(|_| corrupt_snapshot(format!("{field} is outside the u16 domain")))?;
    if value == 0 {
        return Err(corrupt_snapshot(format!("{field} is zero")));
    }
    Ok(value)
}

fn parse_sql_digest(value: &str, field: &str) -> Result<BlobId> {
    BlobId::from_str(value).map_err(|error| corrupt_snapshot(format!("invalid {field}: {error}")))
}

fn parse_sql_id<T>(value: &str, field: &str) -> Result<T>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    value
        .parse()
        .map_err(|error| corrupt_snapshot(format!("invalid {field}: {error}")))
}

fn parse_stage_kind(value: &str) -> Result<FrozenTrialStage> {
    match value {
        "freeze_inputs" => Ok(FrozenTrialStage::FreezeInputs),
        "backtranslate_mask" => Ok(FrozenTrialStage::BacktranslateMask),
        "plan" => Ok(FrozenTrialStage::Plan),
        "retrieve" => Ok(FrozenTrialStage::Retrieve),
        "compile_prompt" => Ok(FrozenTrialStage::CompilePrompt),
        "generate" => Ok(FrozenTrialStage::Generate),
        "admit" => Ok(FrozenTrialStage::Admit),
        "assemble" => Ok(FrozenTrialStage::Assemble),
        "gate" => Ok(FrozenTrialStage::Gate),
        "evaluate" => Ok(FrozenTrialStage::Evaluate),
        "describe" => Ok(FrozenTrialStage::Describe),
        "archive" => Ok(FrozenTrialStage::Archive),
        _ => Err(corrupt_snapshot(format!("invalid stage_kind {value:?}"))),
    }
}

fn corrupt_snapshot(message: impl Into<String>) -> StoreError {
    StoreError::CorruptDatabase(format!(
        "invalid persisted research subject snapshot: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use loom_types::BlobId;
    use rusqlite::params;
    use tempfile::tempdir;

    use crate::{ProjectStore, StoreError};

    #[test]
    fn session_authority_requires_persistence_is_exclusive_and_changes_after_reopen() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "research sessions").expect("project");
        let subject = BlobId::digest(b"frozen campaign subject");

        assert!(matches!(
            store.acquire_campaign_session(subject),
            Err(StoreError::ResearchSessionSubjectNotPersisted { .. })
        ));
        seed_campaign(&mut store, subject);

        let first = store
            .acquire_campaign_session(subject)
            .expect("first exclusive session");
        let first_fingerprint = first.lease_fingerprint();
        let first_writer = store
            .open_research_journal_writer(first)
            .expect("writer consumes and retains exact lease");
        assert_eq!(first_writer.lease_fingerprint(), first_fingerprint);
        assert!(matches!(
            store.acquire_campaign_session(subject),
            Err(StoreError::ResearchSessionAlreadyActive { .. })
        ));
        drop(first_writer);

        let second = store
            .acquire_campaign_session(subject)
            .expect("lease released on drop");
        assert_ne!(first_fingerprint, second.lease_fingerprint());
        let second_fingerprint = second.lease_fingerprint();
        drop(second);
        drop(store);

        let reopened = ProjectStore::open(directory.path()).expect("reopen project");
        let third = reopened
            .acquire_campaign_session(subject)
            .expect("new process-local authority domain");
        assert_ne!(second_fingerprint, third.lease_fingerprint());
    }

    #[test]
    fn journal_writer_retains_project_and_subject_exclusivity_after_store_drop() {
        let directory = tempdir().expect("temporary project");
        let project_path = directory.path().to_path_buf();
        let (mut store, _) =
            ProjectStore::initialize(&project_path, "writer authority").expect("project");
        let subject = BlobId::digest(b"writer-owned campaign");
        seed_campaign(&mut store, subject);
        let lease = store
            .acquire_campaign_session(subject)
            .expect("exclusive session");
        let writer = store
            .open_research_journal_writer(lease)
            .expect("writer consumes lease");
        drop(store);

        assert!(matches!(
            ProjectStore::open(&project_path),
            Err(StoreError::ProjectAlreadyOpen(_))
        ));
        drop(writer);
        let reopened = ProjectStore::open(&project_path).expect("writer drop releases project");
        let lease = reopened
            .acquire_campaign_session(subject)
            .expect("new session exists only after stale writer is gone");
        drop(lease);
    }

    fn seed_campaign(store: &mut ProjectStore, campaign_fingerprint: BlobId) {
        let record = b"canonical campaign record";
        let record_fingerprint = store.put_blob(record).expect("record blob");
        let manifest = b"format = 'loom.campaign.v1'";
        let manifest_blob = store.put_blob(manifest).expect("manifest blob");
        let transaction = store.connection.transaction().expect("transaction");
        for (blob_id, byte_len, media_type) in [
            (
                record_fingerprint,
                i64::try_from(record.len()).expect("record length"),
                "application/json",
            ),
            (
                manifest_blob,
                i64::try_from(manifest.len()).expect("manifest length"),
                "application/toml",
            ),
        ] {
            transaction
                .execute(
                    "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                     VALUES (?1, ?2, ?3, 1)",
                    params![blob_id.to_string(), byte_len, media_type],
                )
                .expect("register blob");
        }
        transaction
            .execute(
                "INSERT INTO research_execution_records(
                    record_fingerprint, record_kind, record_blob_id, created_at_ms
                 ) VALUES (?1, 'campaign', ?1, 1)",
                [record_fingerprint.to_string()],
            )
            .expect("register campaign record");
        transaction
            .execute(
                "INSERT INTO research_campaigns(
                    campaign_id, campaign_fingerprint, project_id,
                    manifest_source_blob_id, manifest_fingerprint,
                    project_input_fingerprint, seed_decimal,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '7', 100, 100, 2, 1000, ?7, 1)",
                params![
                    "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    campaign_fingerprint.to_string(),
                    store.manifest.project_id.to_string(),
                    manifest_blob.to_string(),
                    BlobId::digest(manifest).to_string(),
                    BlobId::digest(b"project input").to_string(),
                    record_fingerprint.to_string(),
                ],
            )
            .expect("register campaign");
        transaction.commit().expect("commit campaign");
    }
}
