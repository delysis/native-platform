use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{
    CampaignId, FrozenTrialStage, StageId, TrialCaseId, TrialRunId, TrialRunOrigin, TrialRunRecord,
};
use loom_types::{BlobId, ProjectId, now_unix_ms};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::provenance::insert_blob_row;
use crate::{ProjectStore, Result, StoreError};

pub const MAX_RESEARCH_EXECUTION_RECORD_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_PERSISTED_CAMPAIGN_TRIALS: usize = 65_536;
pub const MAX_PERSISTED_CAMPAIGN_TRIAL_DEPENDENCIES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResearchExecutionRecordKind {
    Campaign,
    TrialSpec,
    TrialRun,
    StageSpec,
    CampaignTrialAttempt,
    StageAttempt,
    CampaignEvent,
    TrialEvent,
    BudgetReservation,
    BudgetCharge,
    SearchDecision,
    StoryGraph,
    StoryState,
    PromptMask,
    BacktranslationProposal,
    BacktranslationAudition,
    BacktranslationAcceptance,
    EvaluationTask,
    EvaluationReceipt,
    PairwiseAssignment,
    ScoreVector,
    CandidateDescriptor,
    PreferenceLabel,
    ArchiveSnapshot,
    BenchmarkSuite,
    BenchmarkSeal,
    BenchmarkRun,
    BenchmarkJournal,
    HumanLabelPacket,
    BenchmarkResult,
}

impl ResearchExecutionRecordKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Campaign => "campaign",
            Self::TrialSpec => "trial_spec",
            Self::TrialRun => "trial_run",
            Self::StageSpec => "stage_spec",
            Self::CampaignTrialAttempt => "campaign_trial_attempt",
            Self::StageAttempt => "stage_attempt",
            Self::CampaignEvent => "campaign_event",
            Self::TrialEvent => "trial_event",
            Self::BudgetReservation => "budget_reservation",
            Self::BudgetCharge => "budget_charge",
            Self::SearchDecision => "search_decision",
            Self::StoryGraph => "story_graph",
            Self::StoryState => "story_state",
            Self::PromptMask => "prompt_mask",
            Self::BacktranslationProposal => "backtranslation_proposal",
            Self::BacktranslationAudition => "backtranslation_audition",
            Self::BacktranslationAcceptance => "backtranslation_acceptance",
            Self::EvaluationTask => "evaluation_task",
            Self::EvaluationReceipt => "evaluation_receipt",
            Self::PairwiseAssignment => "pairwise_assignment",
            Self::ScoreVector => "score_vector",
            Self::CandidateDescriptor => "candidate_descriptor",
            Self::PreferenceLabel => "preference_label",
            Self::ArchiveSnapshot => "archive_snapshot",
            Self::BenchmarkSuite => "benchmark_suite",
            Self::BenchmarkSeal => "benchmark_seal",
            Self::BenchmarkRun => "benchmark_run",
            Self::BenchmarkJournal => "benchmark_journal",
            Self::HumanLabelPacket => "human_label_packet",
            Self::BenchmarkResult => "benchmark_result",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedResearchExecutionRecord {
    fingerprint: BlobId,
    kind: ResearchExecutionRecordKind,
    byte_len: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct FrozenCampaignPersistence<'a> {
    pub campaign_id: CampaignId,
    pub campaign_fingerprint: BlobId,
    pub project_id: ProjectId,
    pub manifest_source_bytes: &'a [u8],
    pub manifest_fingerprint: BlobId,
    pub project_input_fingerprint: BlobId,
    pub seed: u64,
    pub maximum: ResearchBudgetMaximum,
    pub canonical_record_bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResearchBudgetMaximum {
    pub writer_tokens: u64,
    pub controller_tokens: u64,
    pub evaluations: u32,
    pub wall_time_ms: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct FrozenStagePersistence<'a> {
    pub stage_id: StageId,
    pub stage: FrozenTrialStage,
    pub stage_spec_fingerprint: BlobId,
    pub maximum: ResearchBudgetMaximum,
    pub dependencies: &'a [StageId],
    pub canonical_record_bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
pub struct FrozenTrialPersistence<'a> {
    pub campaign_id: CampaignId,
    pub trial_fingerprint: BlobId,
    pub trial_case_id: TrialCaseId,
    pub treatment_fingerprint: BlobId,
    pub prompt_content_fingerprint: BlobId,
    pub model_binding_fingerprint: BlobId,
    pub expected_writer_call_count: u16,
    pub declared_writer_token_maximum: u64,
    pub maximum: ResearchBudgetMaximum,
    pub canonical_record_bytes: &'a [u8],
    pub stages: &'a [FrozenStagePersistence<'a>],
}

#[derive(Clone, Copy, Debug)]
pub struct FrozenCampaignTrialTopologyPersistence<'a> {
    pub trial_fingerprint: BlobId,
    pub dependencies: &'a [BlobId],
}

#[derive(Clone, Copy, Debug)]
pub struct StandaloneTrialRunPersistence<'a> {
    pub trial_run_id: TrialRunId,
    pub trial_fingerprint: BlobId,
    pub canonical_record_bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedTrialRun {
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl PersistedTrialRun {
    pub const fn trial_run_id(self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn trial_fingerprint(self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrozenCampaignTopologyPersistence<'a> {
    pub campaign_id: CampaignId,
    pub campaign_fingerprint: BlobId,
    pub trials: &'a [FrozenCampaignTrialTopologyPersistence<'a>],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedFrozenCampaignTopology {
    campaign_id: CampaignId,
    campaign_fingerprint: BlobId,
    trial_count: u32,
    dependency_count: u64,
}

impl PersistedFrozenCampaignTopology {
    pub const fn campaign_id(self) -> CampaignId {
        self.campaign_id
    }

    pub const fn campaign_fingerprint(self) -> BlobId {
        self.campaign_fingerprint
    }

    pub const fn trial_count(self) -> u32 {
        self.trial_count
    }

    pub const fn dependency_count(self) -> u64 {
        self.dependency_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedFrozenCampaign {
    campaign_id: CampaignId,
    campaign_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl PersistedFrozenCampaign {
    pub const fn campaign_id(self) -> CampaignId {
        self.campaign_id
    }

    pub const fn campaign_fingerprint(self) -> BlobId {
        self.campaign_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedFrozenTrial {
    trial_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

#[derive(Debug, Eq, PartialEq)]
struct StoredCampaignRow {
    campaign_fingerprint: String,
    project_id: String,
    manifest_source_blob_id: String,
    manifest_fingerprint: String,
    project_input_fingerprint: String,
    seed_decimal: String,
    maximum_writer_tokens: i64,
    maximum_controller_tokens: i64,
    maximum_evaluations: i64,
    maximum_wall_time_ms: i64,
    record_fingerprint: String,
}

impl PersistedFrozenTrial {
    pub const fn trial_fingerprint(self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

impl PersistedResearchExecutionRecord {
    pub const fn fingerprint(self) -> BlobId {
        self.fingerprint
    }

    pub const fn kind(self) -> ResearchExecutionRecordKind {
        self.kind
    }

    pub const fn byte_len(self) -> u64 {
        self.byte_len
    }
}

impl ProjectStore {
    /// Appends one exact canonical research record to the content-addressed
    /// evidence registry. This is diagnostic persistence only; the returned
    /// value cannot mint trial, evaluator, archive, benchmark, or manuscript
    /// authority.
    pub fn persist_research_execution_record(
        &mut self,
        kind: ResearchExecutionRecordKind,
        canonical_bytes: &[u8],
    ) -> Result<PersistedResearchExecutionRecord> {
        ensure_research_record_size(canonical_bytes.len())?;
        let fingerprint = self.put_blob(canonical_bytes)?;
        let byte_len = u64::try_from(canonical_bytes.len()).map_err(|_| {
            StoreError::ResearchExecutionRecordTooLarge {
                actual_bytes: canonical_bytes.len(),
                max_bytes: MAX_RESEARCH_EXECUTION_RECORD_BYTES,
            }
        })?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT record_kind, record_blob_id
                 FROM research_execution_records
                 WHERE record_fingerprint = ?1",
                [fingerprint.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((existing_kind, existing_blob)) = existing {
            if existing_kind != kind.as_str() || existing_blob != fingerprint.to_string() {
                return Err(StoreError::ResearchExecutionRecordConflict { fingerprint });
            }
            transaction.commit()?;
            return Ok(PersistedResearchExecutionRecord {
                fingerprint,
                kind,
                byte_len,
            });
        }

        insert_blob_row(
            &transaction,
            fingerprint,
            canonical_bytes.len(),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO research_execution_records(
                record_fingerprint, record_kind, record_blob_id, created_at_ms
             ) VALUES (?1, ?2, ?1, ?3)",
            params![fingerprint.to_string(), kind.as_str(), created_at_ms],
        )?;
        transaction.commit()?;

        Ok(PersistedResearchExecutionRecord {
            fingerprint,
            kind,
            byte_len,
        })
    }

    pub fn persist_frozen_campaign(
        &mut self,
        input: FrozenCampaignPersistence<'_>,
    ) -> Result<PersistedFrozenCampaign> {
        if input.project_id != self.manifest.project_id {
            return Err(StoreError::ResearchSubjectProjectMismatch);
        }
        ensure_research_record_size(input.manifest_source_bytes.len())?;
        if input.maximum.writer_tokens == 0
            || input.maximum.evaluations == 0
            || input.maximum.wall_time_ms == 0
        {
            return Err(StoreError::InvalidFrozenResearchSubject(
                "campaign writer, evaluation, and wall-time limits must be nonzero".into(),
            ));
        }
        let maximum = sql_budget_maximum(input.maximum)?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::Campaign,
            input.canonical_record_bytes,
        )?;
        let manifest_source_blob_id = self.put_blob(input.manifest_source_bytes)?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_row(
            &transaction,
            manifest_source_blob_id,
            input.manifest_source_bytes.len(),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_campaigns(
                campaign_id, campaign_fingerprint, project_id,
                manifest_source_blob_id, manifest_fingerprint,
                project_input_fingerprint, seed_decimal,
                maximum_writer_tokens, maximum_controller_tokens,
                maximum_evaluations, maximum_wall_time_ms,
                record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                input.campaign_id.to_string(),
                input.campaign_fingerprint.to_string(),
                input.project_id.to_string(),
                manifest_source_blob_id.to_string(),
                input.manifest_fingerprint.to_string(),
                input.project_input_fingerprint.to_string(),
                input.seed.to_string(),
                maximum[0],
                maximum[1],
                maximum[2],
                maximum[3],
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let stored = read_campaign_row(&transaction, input.campaign_id)?;
        let expected = StoredCampaignRow {
            campaign_fingerprint: input.campaign_fingerprint.to_string(),
            project_id: input.project_id.to_string(),
            manifest_source_blob_id: manifest_source_blob_id.to_string(),
            manifest_fingerprint: input.manifest_fingerprint.to_string(),
            project_input_fingerprint: input.project_input_fingerprint.to_string(),
            seed_decimal: input.seed.to_string(),
            maximum_writer_tokens: maximum[0],
            maximum_controller_tokens: maximum[1],
            maximum_evaluations: maximum[2],
            maximum_wall_time_ms: maximum[3],
            record_fingerprint: record.fingerprint().to_string(),
        };
        if stored.as_ref() != Some(&expected) {
            return Err(StoreError::ResearchExecutionSubjectConflict {
                subject: input.campaign_fingerprint,
            });
        }
        transaction.commit()?;
        Ok(PersistedFrozenCampaign {
            campaign_id: input.campaign_id,
            campaign_fingerprint: input.campaign_fingerprint,
            record_fingerprint: record.fingerprint(),
        })
    }

    pub fn persist_frozen_trial(
        &mut self,
        input: FrozenTrialPersistence<'_>,
    ) -> Result<PersistedFrozenTrial> {
        if input.expected_writer_call_count == 0 {
            return Err(StoreError::InvalidFrozenResearchSubject(
                "expected writer-call count must be nonzero".into(),
            ));
        }
        validate_frozen_stages(
            input.stages,
            input.declared_writer_token_maximum,
            input.maximum,
        )?;
        let maximum = sql_budget_maximum(input.maximum)?;
        let declared_writer_token_maximum = i64::try_from(input.declared_writer_token_maximum)
            .map_err(|_| {
                StoreError::InvalidFrozenResearchSubject(
                    "declared writer-token maximum exceeds SQLite's integer domain".into(),
                )
            })?;
        let campaign_exists: i64 = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM research_campaigns WHERE campaign_id = ?1
             )",
            [input.campaign_id.to_string()],
            |row| row.get(0),
        )?;
        if campaign_exists != 1 {
            return Err(StoreError::ResearchCampaignNotPersisted(input.campaign_id));
        }

        let trial_record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::TrialSpec,
            input.canonical_record_bytes,
        )?;
        let mut stage_records = Vec::with_capacity(input.stages.len());
        for stage in input.stages {
            stage_records.push(self.persist_research_execution_record(
                ResearchExecutionRecordKind::StageSpec,
                stage.canonical_record_bytes,
            )?);
        }

        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_exact_trial_row(
            &transaction,
            &input,
            maximum,
            declared_writer_token_maximum,
            trial_record,
            created_at_ms,
        )?;
        insert_exact_stage_rows(&transaction, &input, &stage_records, created_at_ms)?;
        transaction.commit()?;

        Ok(PersistedFrozenTrial {
            trial_fingerprint: input.trial_fingerprint,
            record_fingerprint: trial_record.fingerprint(),
        })
    }

    /// Creates one independently dispatched execution occurrence for an
    /// already-persisted frozen trial. Campaign-owned runs are created only by
    /// the campaign reservation transaction and cannot enter through here.
    pub fn persist_standalone_trial_run(
        &mut self,
        input: StandaloneTrialRunPersistence<'_>,
    ) -> Result<PersistedTrialRun> {
        let record: TrialRunRecord =
            serde_json::from_slice(input.canonical_record_bytes).map_err(|error| {
                StoreError::InvalidFrozenResearchSubject(format!(
                    "invalid canonical trial-run record: {error}"
                ))
            })?;
        let canonical = record.canonical_bytes().map_err(|error| {
            StoreError::InvalidFrozenResearchSubject(format!(
                "cannot canonicalize trial-run record: {error}"
            ))
        })?;
        if canonical != input.canonical_record_bytes
            || record.trial_run_id() != input.trial_run_id
            || record.trial_fingerprint() != input.trial_fingerprint
            || record.origin() != TrialRunOrigin::Standalone
        {
            return Err(StoreError::InvalidFrozenResearchSubject(
                "standalone trial-run record differs from its exact request".into(),
            ));
        }
        let execution_record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::TrialRun,
            input.canonical_record_bytes,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_trial_run_row(
            &transaction,
            input.trial_run_id,
            input.trial_fingerprint,
            "standalone",
            None,
            execution_record.fingerprint(),
            created_at_ms,
        )?;
        transaction.commit()?;
        Ok(PersistedTrialRun {
            trial_run_id: input.trial_run_id,
            trial_fingerprint: input.trial_fingerprint,
            record_fingerprint: execution_record.fingerprint(),
        })
    }

    /// Seals the complete normalized trial DAG after every frozen trial row is
    /// present. This remains diagnostic persistence: campaign execution also
    /// compares the resulting store snapshot with its private frozen spec.
    pub fn persist_frozen_campaign_topology(
        &mut self,
        input: FrozenCampaignTopologyPersistence<'_>,
    ) -> Result<PersistedFrozenCampaignTopology> {
        let topology = validate_campaign_topology(input.trials)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored_campaign_fingerprint = transaction
            .query_row(
                "SELECT campaign_fingerprint FROM research_campaigns WHERE campaign_id = ?1",
                [input.campaign_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let expected_campaign_fingerprint = input.campaign_fingerprint.to_string();
        if stored_campaign_fingerprint.as_ref() != Some(&expected_campaign_fingerprint) {
            return Err(StoreError::ResearchExecutionSubjectConflict {
                subject: input.campaign_fingerprint,
            });
        }

        let stored_trials = read_campaign_trial_fingerprints(&transaction, input.campaign_id)?;
        let expected_trials = input
            .trials
            .iter()
            .map(|trial| trial.trial_fingerprint.to_string())
            .collect::<Vec<_>>();
        ensure_exact_frozen_subject_row(
            stored_trials == expected_trials,
            input.campaign_fingerprint,
        )?;

        for trial in input.trials {
            for (ordinal, dependency) in trial.dependencies.iter().enumerate() {
                transaction.execute(
                    "INSERT OR IGNORE INTO research_campaign_trial_dependencies(
                        trial_fingerprint, dependency_trial_fingerprint, dependency_ordinal
                     ) VALUES (?1, ?2, ?3)",
                    params![
                        trial.trial_fingerprint.to_string(),
                        dependency.to_string(),
                        i64::try_from(ordinal).map_err(|_| {
                            StoreError::InvalidFrozenResearchSubject(
                                "campaign dependency ordinal overflow".into(),
                            )
                        })?,
                    ],
                )?;
            }
        }
        let stored_dependencies =
            read_campaign_trial_dependencies(&transaction, input.campaign_id)?;
        ensure_exact_frozen_subject_row(
            stored_dependencies == topology.dependencies,
            input.campaign_fingerprint,
        )?;
        transaction.commit()?;

        Ok(PersistedFrozenCampaignTopology {
            campaign_id: input.campaign_id,
            campaign_fingerprint: input.campaign_fingerprint,
            trial_count: u32::try_from(input.trials.len()).map_err(|_| {
                StoreError::InvalidFrozenResearchSubject("campaign trial count overflow".into())
            })?,
            dependency_count: u64::try_from(topology.dependencies.len()).map_err(|_| {
                StoreError::InvalidFrozenResearchSubject(
                    "campaign dependency count overflow".into(),
                )
            })?,
        })
    }
}

pub(crate) fn insert_trial_run_row(
    transaction: &Transaction<'_>,
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    origin_kind: &str,
    origin_campaign_id: Option<CampaignId>,
    record_fingerprint: BlobId,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO research_trial_runs(
            trial_run_id, trial_fingerprint, origin_kind, origin_campaign_id,
            record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            trial_run_id.to_string(),
            trial_fingerprint.to_string(),
            origin_kind,
            origin_campaign_id.map(|value| value.to_string()),
            record_fingerprint.to_string(),
            created_at_ms,
        ],
    )?;
    let stored = transaction
        .query_row(
            "SELECT trial_fingerprint, origin_kind, origin_campaign_id,
                    record_fingerprint
             FROM research_trial_runs WHERE trial_run_id = ?1",
            [trial_run_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let exact = (
        trial_fingerprint.to_string(),
        origin_kind.to_owned(),
        origin_campaign_id.map(|value| value.to_string()),
        record_fingerprint.to_string(),
    );
    if stored.as_ref() != Some(&exact) {
        return Err(StoreError::ResearchExecutionSubjectConflict {
            subject: BlobId::digest(trial_run_id.to_string().as_bytes()),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct ValidatedCampaignTopology {
    dependencies: Vec<(String, String, i64)>,
}

fn validate_campaign_topology(
    trials: &[FrozenCampaignTrialTopologyPersistence<'_>],
) -> Result<ValidatedCampaignTopology> {
    if trials.is_empty() || trials.len() > MAX_PERSISTED_CAMPAIGN_TRIALS {
        return Err(StoreError::InvalidFrozenResearchSubject(format!(
            "campaign topology must contain 1..={MAX_PERSISTED_CAMPAIGN_TRIALS} trials"
        )));
    }
    let trial_ids = trials
        .iter()
        .map(|trial| trial.trial_fingerprint)
        .collect::<BTreeSet<_>>();
    if trial_ids.len() != trials.len()
        || trials
            .windows(2)
            .any(|pair| pair[0].trial_fingerprint >= pair[1].trial_fingerprint)
    {
        return Err(StoreError::InvalidFrozenResearchSubject(
            "campaign trials must be unique and strictly fingerprint-ordered".into(),
        ));
    }
    let mut indegree = trial_ids
        .iter()
        .copied()
        .map(|trial| (trial, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<BlobId, Vec<BlobId>>::new();
    let mut dependencies = Vec::new();
    for trial in trials {
        if trial.dependencies.len() > MAX_PERSISTED_CAMPAIGN_TRIAL_DEPENDENCIES
            || trial.dependencies.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(StoreError::InvalidFrozenResearchSubject(
                "campaign dependencies must be bounded, unique, and fingerprint-ordered".into(),
            ));
        }
        for (ordinal, dependency) in trial.dependencies.iter().copied().enumerate() {
            if dependency == trial.trial_fingerprint || !trial_ids.contains(&dependency) {
                return Err(StoreError::InvalidFrozenResearchSubject(
                    "campaign dependency is self-referential or absent".into(),
                ));
            }
            *indegree
                .get_mut(&trial.trial_fingerprint)
                .expect("trial IDs initialize indegree") += 1;
            outgoing
                .entry(dependency)
                .or_default()
                .push(trial.trial_fingerprint);
            dependencies.push((
                trial.trial_fingerprint.to_string(),
                dependency.to_string(),
                i64::try_from(ordinal).map_err(|_| {
                    StoreError::InvalidFrozenResearchSubject(
                        "campaign dependency ordinal overflow".into(),
                    )
                })?,
            ));
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(trial, degree)| (*degree == 0).then_some(*trial))
        .collect::<BTreeSet<_>>();
    let mut visited = 0_usize;
    while let Some(trial) = ready.pop_first() {
        visited += 1;
        for dependent in outgoing.get(&trial).into_iter().flatten() {
            let degree = indegree
                .get_mut(dependent)
                .expect("dependency target initializes indegree");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(*dependent);
            }
        }
    }
    if visited != trials.len() {
        return Err(StoreError::InvalidFrozenResearchSubject(
            "campaign trial dependencies contain a cycle".into(),
        ));
    }
    Ok(ValidatedCampaignTopology { dependencies })
}

fn read_campaign_trial_fingerprints(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
) -> Result<Vec<String>> {
    let mut statement = transaction.prepare(
        "SELECT trial_fingerprint FROM research_trial_specs
         WHERE campaign_id = ?1 ORDER BY trial_fingerprint",
    )?;
    Ok(statement
        .query_map([campaign_id.to_string()], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_campaign_trial_dependencies(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
) -> Result<Vec<(String, String, i64)>> {
    let mut statement = transaction.prepare(
        "SELECT dependency.trial_fingerprint,
                dependency.dependency_trial_fingerprint,
                dependency.dependency_ordinal
         FROM research_campaign_trial_dependencies dependency
         JOIN research_trial_specs trial
           ON trial.trial_fingerprint = dependency.trial_fingerprint
         WHERE trial.campaign_id = ?1
         ORDER BY dependency.trial_fingerprint, dependency.dependency_ordinal",
    )?;
    Ok(statement
        .query_map([campaign_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

fn insert_exact_trial_row(
    transaction: &Transaction<'_>,
    input: &FrozenTrialPersistence<'_>,
    maximum: [i64; 4],
    declared_writer_token_maximum: i64,
    record: PersistedResearchExecutionRecord,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO research_trial_specs(
            trial_fingerprint, campaign_id, trial_case_id,
            treatment_fingerprint, prompt_content_fingerprint,
            model_binding_fingerprint, expected_writer_call_count,
            declared_writer_token_maximum,
            maximum_writer_tokens, maximum_controller_tokens,
            maximum_evaluations, maximum_wall_time_ms,
            record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            input.trial_fingerprint.to_string(),
            input.campaign_id.to_string(),
            input.trial_case_id.to_string(),
            input.treatment_fingerprint.to_string(),
            input.prompt_content_fingerprint.to_string(),
            input.model_binding_fingerprint.to_string(),
            i64::from(input.expected_writer_call_count),
            declared_writer_token_maximum,
            maximum[0],
            maximum[1],
            maximum[2],
            maximum[3],
            record.fingerprint().to_string(),
            created_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_trial_specs
         WHERE trial_fingerprint = ?1 AND campaign_id = ?2
           AND trial_case_id = ?3 AND treatment_fingerprint = ?4
           AND prompt_content_fingerprint = ?5
           AND model_binding_fingerprint = ?6
           AND expected_writer_call_count = ?7
           AND declared_writer_token_maximum = ?8
           AND maximum_writer_tokens = ?9
           AND maximum_controller_tokens = ?10
           AND maximum_evaluations = ?11 AND maximum_wall_time_ms = ?12
           AND record_fingerprint = ?13",
        params![
            input.trial_fingerprint.to_string(),
            input.campaign_id.to_string(),
            input.trial_case_id.to_string(),
            input.treatment_fingerprint.to_string(),
            input.prompt_content_fingerprint.to_string(),
            input.model_binding_fingerprint.to_string(),
            i64::from(input.expected_writer_call_count),
            declared_writer_token_maximum,
            maximum[0],
            maximum[1],
            maximum[2],
            maximum[3],
            record.fingerprint().to_string(),
        ],
        |row| row.get(0),
    )?;
    ensure_exact_frozen_subject_row(exact == 1, input.trial_fingerprint)
}

fn insert_exact_stage_rows(
    transaction: &Transaction<'_>,
    input: &FrozenTrialPersistence<'_>,
    records: &[PersistedResearchExecutionRecord],
    created_at_ms: i64,
) -> Result<()> {
    for (ordinal, (stage, record)) in input.stages.iter().zip(records).enumerate() {
        insert_exact_stage_row(
            transaction,
            input.trial_fingerprint,
            ordinal,
            stage,
            *record,
            created_at_ms,
        )?;
    }
    let stage_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_campaign_stage_specs
         WHERE trial_fingerprint = ?1",
        [input.trial_fingerprint.to_string()],
        |row| row.get(0),
    )?;
    let expected_count = i64::try_from(input.stages.len())
        .map_err(|_| StoreError::InvalidFrozenResearchSubject("stage count overflow".into()))?;
    ensure_exact_frozen_subject_row(stage_count == expected_count, input.trial_fingerprint)?;
    verify_exact_stage_dependencies(transaction, input)
}

fn insert_exact_stage_row(
    transaction: &Transaction<'_>,
    trial_fingerprint: BlobId,
    ordinal: usize,
    stage: &FrozenStagePersistence<'_>,
    record: PersistedResearchExecutionRecord,
    created_at_ms: i64,
) -> Result<()> {
    let ordinal = i64::try_from(ordinal)
        .map_err(|_| StoreError::InvalidFrozenResearchSubject("stage ordinal overflow".into()))?;
    let maximum = sql_budget_maximum(stage.maximum)?;
    transaction.execute(
        "INSERT OR IGNORE INTO research_campaign_stage_specs(
            stage_id, trial_fingerprint, stage_ordinal, stage_kind,
            stage_spec_fingerprint, maximum_writer_tokens,
            maximum_controller_tokens, maximum_evaluations,
            maximum_wall_time_ms, record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            stage.stage_id.to_string(),
            trial_fingerprint.to_string(),
            ordinal,
            stage_kind(stage.stage),
            stage.stage_spec_fingerprint.to_string(),
            maximum[0],
            maximum[1],
            maximum[2],
            maximum[3],
            record.fingerprint().to_string(),
            created_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_campaign_stage_specs
         WHERE stage_id = ?1 AND trial_fingerprint = ?2
           AND stage_ordinal = ?3 AND stage_kind = ?4
           AND stage_spec_fingerprint = ?5 AND maximum_writer_tokens = ?6
           AND maximum_controller_tokens = ?7 AND maximum_evaluations = ?8
           AND maximum_wall_time_ms = ?9 AND record_fingerprint = ?10",
        params![
            stage.stage_id.to_string(),
            trial_fingerprint.to_string(),
            ordinal,
            stage_kind(stage.stage),
            stage.stage_spec_fingerprint.to_string(),
            maximum[0],
            maximum[1],
            maximum[2],
            maximum[3],
            record.fingerprint().to_string(),
        ],
        |row| row.get(0),
    )?;
    ensure_exact_frozen_subject_row(exact == 1, trial_fingerprint)?;
    for (dependency_ordinal, dependency) in stage.dependencies.iter().enumerate() {
        transaction.execute(
            "INSERT OR IGNORE INTO research_campaign_stage_dependencies(
                stage_id, dependency_stage_id, dependency_ordinal
             ) VALUES (?1, ?2, ?3)",
            params![
                stage.stage_id.to_string(),
                dependency.to_string(),
                i64::try_from(dependency_ordinal).map_err(|_| {
                    StoreError::InvalidFrozenResearchSubject("dependency ordinal overflow".into())
                })?,
            ],
        )?;
    }
    Ok(())
}

type StoredStageDependency = (String, i64, String, i64);

fn verify_exact_stage_dependencies(
    transaction: &Transaction<'_>,
    input: &FrozenTrialPersistence<'_>,
) -> Result<()> {
    let mut statement = transaction.prepare(
        "SELECT stage.stage_id, stage.stage_ordinal,
                dependency.dependency_stage_id, dependency.dependency_ordinal
         FROM research_campaign_stage_specs stage
         JOIN research_campaign_stage_dependencies dependency USING (stage_id)
         WHERE stage.trial_fingerprint = ?1
         ORDER BY stage.stage_ordinal, dependency.dependency_ordinal",
    )?;
    let stored = statement
        .query_map([input.trial_fingerprint.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<std::result::Result<Vec<StoredStageDependency>, _>>()?;
    let mut expected = Vec::new();
    for (stage_ordinal, stage) in input.stages.iter().enumerate() {
        for (dependency_ordinal, dependency) in stage.dependencies.iter().enumerate() {
            expected.push((
                stage.stage_id.to_string(),
                i64::try_from(stage_ordinal).map_err(|_| {
                    StoreError::InvalidFrozenResearchSubject("stage ordinal overflow".into())
                })?,
                dependency.to_string(),
                i64::try_from(dependency_ordinal).map_err(|_| {
                    StoreError::InvalidFrozenResearchSubject("dependency ordinal overflow".into())
                })?,
            ));
        }
    }
    ensure_exact_frozen_subject_row(stored == expected, input.trial_fingerprint)
}

fn ensure_exact_frozen_subject_row(exact: bool, subject: BlobId) -> Result<()> {
    if !exact {
        return Err(StoreError::ResearchExecutionSubjectConflict { subject });
    }
    Ok(())
}

fn read_campaign_row(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
) -> Result<Option<StoredCampaignRow>> {
    transaction
        .query_row(
            "SELECT campaign_fingerprint, project_id, manifest_source_blob_id,
                    manifest_fingerprint, project_input_fingerprint, seed_decimal,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms,
                    record_fingerprint
             FROM research_campaigns WHERE campaign_id = ?1",
            [campaign_id.to_string()],
            |row| {
                Ok(StoredCampaignRow {
                    campaign_fingerprint: row.get(0)?,
                    project_id: row.get(1)?,
                    manifest_source_blob_id: row.get(2)?,
                    manifest_fingerprint: row.get(3)?,
                    project_input_fingerprint: row.get(4)?,
                    seed_decimal: row.get(5)?,
                    maximum_writer_tokens: row.get(6)?,
                    maximum_controller_tokens: row.get(7)?,
                    maximum_evaluations: row.get(8)?,
                    maximum_wall_time_ms: row.get(9)?,
                    record_fingerprint: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn validate_frozen_stages(
    stages: &[FrozenStagePersistence<'_>],
    declared_writer_token_maximum: u64,
    trial_maximum: ResearchBudgetMaximum,
) -> Result<()> {
    if stages.len() != FrozenTrialStage::ALL.len() {
        return Err(StoreError::InvalidFrozenResearchSubject(format!(
            "expected {} stages, found {}",
            FrozenTrialStage::ALL.len(),
            stages.len()
        )));
    }
    if declared_writer_token_maximum == 0
        || declared_writer_token_maximum > trial_maximum.writer_tokens
        || trial_maximum.writer_tokens == 0
        || trial_maximum.evaluations == 0
        || trial_maximum.wall_time_ms == 0
    {
        return Err(StoreError::InvalidFrozenResearchSubject(
            "trial and declared writer budget limits are inconsistent".into(),
        ));
    }
    let mut stage_ids = BTreeSet::new();
    let mut records = BTreeSet::new();
    let mut total = ResearchBudgetMaximum {
        writer_tokens: 0,
        controller_tokens: 0,
        evaluations: 0,
        wall_time_ms: 0,
    };
    for (ordinal, (stage, expected_kind)) in stages.iter().zip(FrozenTrialStage::ALL).enumerate() {
        if stage.stage != expected_kind || !stage_ids.insert(stage.stage_id) {
            return Err(StoreError::InvalidFrozenResearchSubject(format!(
                "invalid stage identity or kind at ordinal {ordinal}"
            )));
        }
        if !valid_stage_budget_shape(stage.stage, stage.maximum, declared_writer_token_maximum) {
            return Err(StoreError::InvalidFrozenResearchSubject(format!(
                "stage {ordinal} has an invalid resource shape"
            )));
        }
        let record_fingerprint = BlobId::digest(stage.canonical_record_bytes);
        if stage.canonical_record_bytes.is_empty() || !records.insert(record_fingerprint) {
            return Err(StoreError::InvalidFrozenResearchSubject(format!(
                "stage {ordinal} has empty or repeated canonical evidence"
            )));
        }
        let dependency_ordinals = canonical_dependency_ordinals(ordinal).ok_or_else(|| {
            StoreError::InvalidFrozenResearchSubject("stage ordinal exceeds protocol".into())
        })?;
        let expected_dependencies = dependency_ordinals
            .iter()
            .map(|dependency| stages[*dependency].stage_id)
            .collect::<Vec<_>>();
        if stage.dependencies != expected_dependencies {
            return Err(StoreError::InvalidFrozenResearchSubject(format!(
                "stage {ordinal} does not have the canonical dependency list"
            )));
        }
        total.writer_tokens = total
            .writer_tokens
            .checked_add(stage.maximum.writer_tokens)
            .ok_or_else(|| {
                StoreError::InvalidFrozenResearchSubject("writer budget overflow".into())
            })?;
        total.controller_tokens = total
            .controller_tokens
            .checked_add(stage.maximum.controller_tokens)
            .ok_or_else(|| {
                StoreError::InvalidFrozenResearchSubject("controller budget overflow".into())
            })?;
        total.evaluations = total
            .evaluations
            .checked_add(stage.maximum.evaluations)
            .ok_or_else(|| {
                StoreError::InvalidFrozenResearchSubject("evaluation budget overflow".into())
            })?;
        total.wall_time_ms = total
            .wall_time_ms
            .checked_add(stage.maximum.wall_time_ms)
            .ok_or_else(|| {
                StoreError::InvalidFrozenResearchSubject("wall-time budget overflow".into())
            })?;
    }
    if total.writer_tokens > trial_maximum.writer_tokens
        || total.controller_tokens > trial_maximum.controller_tokens
        || total.evaluations > trial_maximum.evaluations
        || total.wall_time_ms > trial_maximum.wall_time_ms
    {
        return Err(StoreError::InvalidFrozenResearchSubject(
            "sum of stage maxima exceeds the frozen trial maximum".into(),
        ));
    }
    Ok(())
}

const fn valid_stage_budget_shape(
    stage: FrozenTrialStage,
    maximum: ResearchBudgetMaximum,
    declared_writer_token_maximum: u64,
) -> bool {
    if maximum.wall_time_ms == 0 {
        return false;
    }
    match stage {
        FrozenTrialStage::BacktranslateMask | FrozenTrialStage::Plan => {
            maximum.writer_tokens == 0 && maximum.evaluations == 0
        }
        FrozenTrialStage::Generate => {
            maximum.writer_tokens == declared_writer_token_maximum
                && maximum.controller_tokens == 0
                && maximum.evaluations == 0
        }
        FrozenTrialStage::Evaluate => maximum.writer_tokens == 0 && maximum.evaluations > 0,
        FrozenTrialStage::FreezeInputs
        | FrozenTrialStage::Retrieve
        | FrozenTrialStage::CompilePrompt
        | FrozenTrialStage::Admit
        | FrozenTrialStage::Assemble
        | FrozenTrialStage::Gate
        | FrozenTrialStage::Describe
        | FrozenTrialStage::Archive => {
            maximum.writer_tokens == 0 && maximum.controller_tokens == 0 && maximum.evaluations == 0
        }
    }
}

const fn canonical_dependency_ordinals(stage_ordinal: usize) -> Option<&'static [usize]> {
    match stage_ordinal {
        0 => Some(&[]),
        1 => Some(&[0]),
        2 => Some(&[0, 1]),
        3 => Some(&[0, 2]),
        4 => Some(&[0, 1, 2, 3]),
        5 => Some(&[4]),
        6 => Some(&[5]),
        7 => Some(&[6]),
        8 => Some(&[7]),
        9 => Some(&[8]),
        10 => Some(&[9]),
        11 => Some(&[9, 10]),
        _ => None,
    }
}

const fn stage_kind(stage: FrozenTrialStage) -> &'static str {
    match stage {
        FrozenTrialStage::FreezeInputs => "freeze_inputs",
        FrozenTrialStage::BacktranslateMask => "backtranslate_mask",
        FrozenTrialStage::Plan => "plan",
        FrozenTrialStage::Retrieve => "retrieve",
        FrozenTrialStage::CompilePrompt => "compile_prompt",
        FrozenTrialStage::Generate => "generate",
        FrozenTrialStage::Admit => "admit",
        FrozenTrialStage::Assemble => "assemble",
        FrozenTrialStage::Gate => "gate",
        FrozenTrialStage::Evaluate => "evaluate",
        FrozenTrialStage::Describe => "describe",
        FrozenTrialStage::Archive => "archive",
    }
}

fn sql_budget_maximum(maximum: ResearchBudgetMaximum) -> Result<[i64; 4]> {
    Ok([
        i64::try_from(maximum.writer_tokens).map_err(|_| {
            StoreError::InvalidFrozenResearchSubject(
                "writer-token maximum exceeds SQLite's integer domain".into(),
            )
        })?,
        i64::try_from(maximum.controller_tokens).map_err(|_| {
            StoreError::InvalidFrozenResearchSubject(
                "controller-token maximum exceeds SQLite's integer domain".into(),
            )
        })?,
        i64::from(maximum.evaluations),
        i64::try_from(maximum.wall_time_ms).map_err(|_| {
            StoreError::InvalidFrozenResearchSubject(
                "wall-time maximum exceeds SQLite's integer domain".into(),
            )
        })?,
    ])
}

fn ensure_research_record_size(byte_len: usize) -> Result<()> {
    if byte_len == 0 {
        return Err(StoreError::EmptyResearchExecutionRecord);
    }
    if byte_len > MAX_RESEARCH_EXECUTION_RECORD_BYTES {
        return Err(StoreError::ResearchExecutionRecordTooLarge {
            actual_bytes: byte_len,
            max_bytes: MAX_RESEARCH_EXECUTION_RECORD_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use loom_research_types::{CampaignId, FrozenTrialStage, StageId, TrialCaseId};
    use loom_types::BlobId;
    use tempfile::tempdir;

    use super::*;
    use crate::{PersistedResearchSubjectSnapshot, ResearchSessionKind};

    #[test]
    fn canonical_records_are_content_addressed_idempotent_and_non_authorizing() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "records").expect("store");
        let bytes = br#"{"format":"loom.research.test.v1","value":7}"#;

        let first = store
            .persist_research_execution_record(ResearchExecutionRecordKind::Campaign, bytes)
            .expect("persist record");
        let second = store
            .persist_research_execution_record(ResearchExecutionRecordKind::Campaign, bytes)
            .expect("idempotent record");
        assert_eq!(first, second);
        assert_eq!(first.fingerprint(), BlobId::digest(bytes));
        assert_eq!(
            first.byte_len(),
            u64::try_from(bytes.len()).expect("length")
        );

        assert!(matches!(
            store.persist_research_execution_record(ResearchExecutionRecordKind::TrialSpec, bytes,),
            Err(StoreError::ResearchExecutionRecordConflict { .. })
        ));
        assert!(matches!(
            store.acquire_campaign_session(first.fingerprint()),
            Err(StoreError::ResearchSessionSubjectNotPersisted { .. })
        ));
    }

    #[test]
    fn empty_and_oversized_record_bytes_fail_before_storage() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "bounds").expect("store");
        assert!(matches!(
            store.persist_research_execution_record(ResearchExecutionRecordKind::Campaign, b""),
            Err(StoreError::EmptyResearchExecutionRecord)
        ));
        assert!(matches!(
            ensure_research_record_size(MAX_RESEARCH_EXECUTION_RECORD_BYTES + 1),
            Err(StoreError::ResearchExecutionRecordTooLarge { .. })
        ));
    }

    #[test]
    fn frozen_campaign_topology_rejects_cycles_and_noncanonical_order() {
        let mut ids = [BlobId::digest(b"trial a"), BlobId::digest(b"trial b")];
        ids.sort_unstable();
        let first_dependencies = [ids[1]];
        let second_dependencies = [ids[0]];
        let cyclic = [
            FrozenCampaignTrialTopologyPersistence {
                trial_fingerprint: ids[0],
                dependencies: &first_dependencies,
            },
            FrozenCampaignTrialTopologyPersistence {
                trial_fingerprint: ids[1],
                dependencies: &second_dependencies,
            },
        ];
        assert!(matches!(
            validate_campaign_topology(&cyclic),
            Err(StoreError::InvalidFrozenResearchSubject(_))
        ));

        let reversed = [cyclic[1], cyclic[0]];
        assert!(matches!(
            validate_campaign_topology(&reversed),
            Err(StoreError::InvalidFrozenResearchSubject(_))
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn frozen_campaign_and_trial_registration_is_exact_idempotent_and_session_eligible() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "frozen").expect("store");
        let (campaign_id, campaign_fingerprint) = persist_test_campaign(&mut store);

        let stage_ids = FrozenTrialStage::ALL.map(|_| StageId::new());
        let dependencies = vec![
            vec![],
            vec![stage_ids[0]],
            vec![stage_ids[0], stage_ids[1]],
            vec![stage_ids[0], stage_ids[2]],
            vec![stage_ids[0], stage_ids[1], stage_ids[2], stage_ids[3]],
            vec![stage_ids[4]],
            vec![stage_ids[5]],
            vec![stage_ids[6]],
            vec![stage_ids[7]],
            vec![stage_ids[8]],
            vec![stage_ids[9]],
            vec![stage_ids[9], stage_ids[10]],
        ];
        let records = FrozenTrialStage::ALL
            .iter()
            .enumerate()
            .map(|(index, stage)| format!("canonical stage {index} {stage:?}").into_bytes())
            .collect::<Vec<_>>();
        let stages = FrozenTrialStage::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, stage)| FrozenStagePersistence {
                stage_id: stage_ids[index],
                stage,
                stage_spec_fingerprint: BlobId::digest(&records[index]),
                maximum: ResearchBudgetMaximum {
                    writer_tokens: u64::from(stage == FrozenTrialStage::Generate) * 256,
                    controller_tokens: if stage == FrozenTrialStage::Evaluate {
                        64
                    } else {
                        u64::from(matches!(
                            stage,
                            FrozenTrialStage::BacktranslateMask | FrozenTrialStage::Plan
                        )) * 128
                    },
                    evaluations: u32::from(stage == FrozenTrialStage::Evaluate),
                    wall_time_ms: 1_000,
                },
                dependencies: &dependencies[index],
                canonical_record_bytes: &records[index],
            })
            .collect::<Vec<_>>();
        let trial_fingerprint = BlobId::digest(b"frozen trial fingerprint");
        let trial = FrozenTrialPersistence {
            campaign_id,
            trial_fingerprint,
            trial_case_id: TrialCaseId::new(),
            treatment_fingerprint: BlobId::digest(b"treatment"),
            prompt_content_fingerprint: BlobId::digest(b"prompt content"),
            model_binding_fingerprint: BlobId::digest(b"model binding"),
            expected_writer_call_count: 1,
            declared_writer_token_maximum: 256,
            maximum: ResearchBudgetMaximum {
                writer_tokens: 256,
                controller_tokens: 512,
                evaluations: 2,
                wall_time_ms: 20_000,
            },
            canonical_record_bytes: b"canonical frozen trial record",
            stages: &stages,
        };
        let first_trial = store.persist_frozen_trial(trial).expect("persist trial");
        assert_eq!(
            first_trial,
            store.persist_frozen_trial(trial).expect("idempotent trial")
        );
        let mut altered_stages = stages.clone();
        altered_stages[0].maximum.wall_time_ms -= 1;
        assert!(matches!(
            store.persist_frozen_trial(FrozenTrialPersistence {
                stages: &altered_stages,
                ..trial
            }),
            Err(StoreError::ResearchExecutionSubjectConflict { .. })
        ));

        let topology_trial = FrozenCampaignTrialTopologyPersistence {
            trial_fingerprint,
            dependencies: &[],
        };
        let topology_input = FrozenCampaignTopologyPersistence {
            campaign_id,
            campaign_fingerprint,
            trials: std::slice::from_ref(&topology_trial),
        };
        let topology = store
            .persist_frozen_campaign_topology(topology_input)
            .expect("persist complete campaign topology");
        assert_eq!(
            topology,
            store
                .persist_frozen_campaign_topology(topology_input)
                .expect("idempotent campaign topology")
        );
        assert_eq!(topology.trial_count(), 1);
        assert_eq!(topology.dependency_count(), 0);
        assert!(matches!(
            store.persist_frozen_campaign_topology(FrozenCampaignTopologyPersistence {
                campaign_id,
                campaign_fingerprint,
                trials: &[],
            }),
            Err(StoreError::InvalidFrozenResearchSubject(_))
        ));
        assert!(matches!(
            store.persist_frozen_campaign_topology(FrozenCampaignTopologyPersistence {
                campaign_id,
                campaign_fingerprint: BlobId::digest(b"relabeled campaign"),
                trials: std::slice::from_ref(&topology_trial),
            }),
            Err(StoreError::ResearchExecutionSubjectConflict { .. })
        ));
        let extra_trial = FrozenCampaignTrialTopologyPersistence {
            trial_fingerprint: BlobId::digest(b"unpersisted extra trial"),
            dependencies: &[],
        };
        let mut conflicting_trials = [topology_trial, extra_trial];
        conflicting_trials.sort_unstable_by_key(|trial| trial.trial_fingerprint);
        assert!(matches!(
            store.persist_frozen_campaign_topology(FrozenCampaignTopologyPersistence {
                campaign_id,
                campaign_fingerprint,
                trials: &conflicting_trials,
            }),
            Err(StoreError::ResearchExecutionSubjectConflict { .. })
        ));

        let campaign_session = store
            .acquire_campaign_session(campaign_fingerprint)
            .expect("campaign session");
        let trial_run_id = loom_research_types::TrialRunId::new();
        let trial_run = loom_research_types::TrialRunRecord::new(
            trial_run_id,
            trial_fingerprint,
            loom_research_types::TrialRunOrigin::Standalone,
        );
        let trial_run_bytes = trial_run.canonical_bytes().expect("canonical trial run");
        store
            .persist_standalone_trial_run(StandaloneTrialRunPersistence {
                trial_run_id,
                trial_fingerprint,
                canonical_record_bytes: &trial_run_bytes,
            })
            .expect("standalone trial run");
        let trial_session = store
            .acquire_trial_run_session(trial_run_id)
            .expect("trial session");
        assert_eq!(campaign_session.subject_fingerprint(), campaign_fingerprint);
        assert_eq!(trial_session.trial_run_id(), Some(trial_run_id));
        let PersistedResearchSubjectSnapshot::Campaign(campaign_snapshot) =
            campaign_session.snapshot()
        else {
            panic!("campaign lease carried a trial snapshot");
        };
        assert_eq!(campaign_snapshot.trials().len(), 1);
        assert_eq!(
            campaign_snapshot.trials()[0].trial_fingerprint(),
            trial_fingerprint
        );
        let PersistedResearchSubjectSnapshot::Trial(trial_snapshot) = trial_session.snapshot()
        else {
            panic!("trial lease carried a campaign snapshot");
        };
        assert_eq!(trial_snapshot.stages().len(), FrozenTrialStage::ALL.len());
        for (index, stage) in trial_snapshot.stages().iter().enumerate() {
            assert_eq!(stage.stage_id(), stage_ids[index]);
            assert_eq!(stage.stage(), FrozenTrialStage::ALL[index]);
            assert_eq!(stage.dependencies(), dependencies[index]);
        }
    }

    fn persist_test_campaign(store: &mut ProjectStore) -> (CampaignId, BlobId) {
        let campaign_id = CampaignId::new();
        let campaign_fingerprint = BlobId::digest(b"frozen campaign fingerprint");
        let campaign = FrozenCampaignPersistence {
            campaign_id,
            campaign_fingerprint,
            project_id: store.manifest().project_id,
            manifest_source_bytes: b"format = 'loom.campaign.v1'\nseed = 7\n",
            manifest_fingerprint: BlobId::digest(b"canonical campaign manifest"),
            project_input_fingerprint: BlobId::digest(b"project inputs"),
            seed: 7,
            maximum: ResearchBudgetMaximum {
                writer_tokens: 256,
                controller_tokens: 512,
                evaluations: 2,
                wall_time_ms: 20_000,
            },
            canonical_record_bytes: b"canonical frozen campaign record",
        };
        let first = store
            .persist_frozen_campaign(campaign)
            .expect("persist campaign");
        assert_eq!(
            first,
            store
                .persist_frozen_campaign(campaign)
                .expect("idempotent campaign")
        );
        (campaign_id, campaign_fingerprint)
    }
}
