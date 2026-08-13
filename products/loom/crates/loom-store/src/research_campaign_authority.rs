use std::fmt;
use std::str::FromStr;

use loom_research_types::{CampaignId, ModelCallId, TrialCaseId};
use loom_types::{ArtifactId, BlobId, ProjectId};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::{AdmittedCandidateProjection, AdmittedGeneratedSpan, ProjectStore, Result, StoreError};

pub const MAX_VERIFIED_CAMPAIGN_POOL_CANDIDATES: usize = 32;

/// One exact live span/projection pair proposed for an ordered campaign pool.
///
/// IDs are deliberately insufficient: both opaque admission leases must come
/// from the currently open store session.
#[derive(Clone, Copy)]
pub struct CampaignPoolCandidateLease<'a> {
    span: &'a AdmittedGeneratedSpan,
    projection: &'a AdmittedCandidateProjection,
}

impl<'a> CampaignPoolCandidateLease<'a> {
    pub const fn new(
        span: &'a AdmittedGeneratedSpan,
        projection: &'a AdmittedCandidateProjection,
    ) -> Self {
        Self { span, projection }
    }
}

impl fmt::Debug for CampaignPoolCandidateLease<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CampaignPoolCandidateLease")
            .field("occurrence_id", &self.span.occurrence_id())
            .field("projection_id", &self.projection.projection_id())
            .finish_non_exhaustive()
    }
}

/// Checked current-store facts for one exact ordered candidate pool.
///
/// This is deliberately diagnostic data, not a campaign-mutation authority.
/// The public low-level journal writer can append caller-supplied canonical
/// records, so SQL coherence cannot replace a move-only trial-runtime lease.
/// A future production adapter must compose these facts with exact
/// Generate/Admit/Assemble authority from the same trial run.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedCampaignNestedPoolEvidence {
    project_id: ProjectId,
    campaign_id: CampaignId,
    trial_fingerprint: BlobId,
    case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    batch_fingerprint: BlobId,
    ordered_occurrences: Vec<ArtifactId>,
    store_authority_domain_fingerprint: BlobId,
    evidence_fingerprint: BlobId,
}

impl CheckedCampaignNestedPoolEvidence {
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn campaign_id(&self) -> CampaignId {
        self.campaign_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub const fn batch_fingerprint(&self) -> BlobId {
        self.batch_fingerprint
    }

    pub fn ordered_occurrences(&self) -> &[ArtifactId] {
        &self.ordered_occurrences
    }

    pub const fn evidence_fingerprint(&self) -> BlobId {
        self.evidence_fingerprint
    }

    pub const fn store_authority_domain_fingerprint(&self) -> BlobId {
        self.store_authority_domain_fingerprint
    }
}

#[derive(Clone, Copy)]
struct FrozenTrialPoolBinding {
    project_id: ProjectId,
    campaign_id: CampaignId,
    case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    expected_writer_call_count: u16,
}

#[derive(Clone, Copy)]
struct VerifiedPoolCandidateRow {
    projection_fingerprint: BlobId,
    batch_fingerprint: BlobId,
    call_id: ModelCallId,
}

const VERIFIED_POOL_CANDIDATE_SQL: &str = "SELECT projection.graph_fingerprint,
            batch.batch_verification_fingerprint, call.call_id
     FROM research_generated_span_occurrences span
     JOIN research_model_calls call ON call.call_id = span.call_id
     JOIN research_verified_inference_batch_calls batch_call
       ON batch_call.call_id = call.call_id
      AND batch_call.outcome = 'completed'
     JOIN research_verified_inference_batches batch
       ON batch.batch_verification_fingerprint =
          batch_call.batch_verification_fingerprint
     JOIN research_verified_inference_batch_seals batch_seal
       ON batch_seal.batch_verification_fingerprint =
          batch.batch_verification_fingerprint
     JOIN research_candidate_assembly_parts part
       ON part.occurrence_id = span.occurrence_id
      AND part.position = 0
     JOIN research_candidate_assemblies assembly
       ON assembly.assembly_id = part.assembly_id
      AND assembly.part_count = 1
     JOIN research_admission_records assembly_admission
       ON assembly_admission.subject_kind = 'candidate_assembly'
      AND assembly_admission.subject_id = assembly.assembly_id
     JOIN research_candidate_projections projection
       ON projection.assembly_id = assembly.assembly_id
     JOIN research_admission_records projection_admission
       ON projection_admission.subject_kind = 'candidate_projection'
      AND projection_admission.subject_id = projection.projection_id
     JOIN research_trial_specs trial
       ON trial.trial_fingerprint = ?3
      AND trial.campaign_id = call.campaign_id
      AND trial.trial_case_id = call.trial_case_id
      AND trial.treatment_fingerprint = batch.treatment_recipe_fingerprint
      AND trial.prompt_content_fingerprint = batch.prompt_content_fingerprint
      AND trial.model_binding_fingerprint = batch.model_binding_fingerprint
      AND trial.expected_writer_call_count = batch.expected_case_count
     JOIN research_campaign_stage_specs stage
       ON stage.trial_fingerprint = trial.trial_fingerprint
      AND stage.stage_kind = 'generate'
      AND stage.stage_id = call.stage_id
      AND stage.stage_id = batch.prompt_stage_id
     JOIN research_campaign_stage_attempts stage_attempt
       ON stage_attempt.stage_attempt_id = call.stage_attempt_id
      AND stage_attempt.stage_id = stage.stage_id
     JOIN research_trial_events stage_terminal
       ON stage_terminal.trial_fingerprint = trial.trial_fingerprint
      AND stage_terminal.stage_attempt_id = stage_attempt.stage_attempt_id
      AND stage_terminal.event_kind = 'attempt_finished'
      AND stage_terminal.attempt_outcome = 'succeeded'
      AND stage_terminal.terminal_output_fingerprint =
          batch.batch_verification_fingerprint
     WHERE span.occurrence_id = ?1
       AND projection.projection_id = ?2
       AND span.evidence_class = 'live_base_writer_claim'
       AND span.verification_audit_fingerprint IS NOT NULL
       AND call.evidence_class = 'live_base_writer_claim'
       AND call.verification_audit_fingerprint = span.verification_audit_fingerprint
       AND call.campaign_id = ?4
       AND call.trial_case_id = ?5
       AND batch.prompt_campaign_id = ?4
       AND batch.prompt_trial_case_id = ?5
       AND batch.treatment_recipe_fingerprint = ?6
       AND batch.project_id = ?7
       AND projection_admission.admission_record_id = ?8
       AND batch_call.position = ?9
       AND batch.expected_case_count = ?10
       AND batch_seal.completed_call_count = ?10
       AND batch_seal.cancelled_call_count = 0";

impl ProjectStore {
    /// Verifies an exact ordered N-pool against current admission leases and
    /// immutable inference, trial, assembly, and projection rows.
    ///
    /// Checking is intentionally idempotent: repeating it in one open store
    /// session yields equivalent diagnostic evidence. Admission rows are
    /// unique per subject and append-only, so the joined row is the sole
    /// effective admission rather than one member of a superseding history.
    pub fn check_campaign_nested_pool(
        &self,
        trial_fingerprint: BlobId,
        ordered_candidates: &[CampaignPoolCandidateLease<'_>],
    ) -> Result<CheckedCampaignNestedPoolEvidence> {
        if ordered_candidates.is_empty()
            || ordered_candidates.len() > MAX_VERIFIED_CAMPAIGN_POOL_CANDIDATES
        {
            return Err(invalid_pool(
                "candidate count is outside the bounded pool domain",
            ));
        }
        if ordered_candidates.iter().any(|candidate| {
            !candidate.span.belongs_to_session(self.session_nonce)
                || !candidate.projection.belongs_to_session(self.session_nonce)
        }) {
            return Err(invalid_pool(
                "candidate admission lease belongs to another store session",
            ));
        }
        let binding = self.load_frozen_trial_pool_binding(trial_fingerprint)?;
        if binding.project_id != self.manifest.project_id {
            return Err(invalid_pool("frozen trial belongs to another project"));
        }
        if usize::from(binding.expected_writer_call_count) != ordered_candidates.len() {
            return Err(invalid_pool(
                "candidate pool cardinality differs from the frozen trial",
            ));
        }

        let mut ordered_occurrences = Vec::with_capacity(ordered_candidates.len());
        let mut ordered_projection_fingerprints = Vec::with_capacity(ordered_candidates.len());
        let mut batch_fingerprint = None;
        let mut call_ids = Vec::with_capacity(ordered_candidates.len());
        for (expected_position, candidate) in ordered_candidates.iter().enumerate() {
            let occurrence_id = ArtifactId::from_ulid(candidate.span.occurrence_id().as_ulid());
            if ordered_occurrences.contains(&occurrence_id) {
                return Err(invalid_pool("candidate pool repeats an occurrence"));
            }
            let row = self.verify_pool_candidate_rows(
                trial_fingerprint,
                binding,
                candidate,
                expected_position,
            )?;
            if batch_fingerprint.is_some_and(|expected| expected != row.batch_fingerprint) {
                return Err(invalid_pool(
                    "candidate pool mixes sealed inference batches",
                ));
            }
            if call_ids.contains(&row.call_id) {
                return Err(invalid_pool("candidate pool repeats a model call"));
            }
            batch_fingerprint.get_or_insert(row.batch_fingerprint);
            call_ids.push(row.call_id);
            ordered_occurrences.push(occurrence_id);
            ordered_projection_fingerprints.push(row.projection_fingerprint);
        }
        let batch_fingerprint = batch_fingerprint
            .ok_or_else(|| invalid_pool("candidate pool has no sealed inference batch"))?;
        let evidence_fingerprint = pool_evidence_fingerprint(
            self.session_nonce.as_bytes(),
            trial_fingerprint,
            binding,
            batch_fingerprint,
            &ordered_occurrences,
            &ordered_projection_fingerprints,
        );
        let store_authority_domain_fingerprint = self.research_authority_domain_fingerprint();
        Ok(CheckedCampaignNestedPoolEvidence {
            project_id: binding.project_id,
            campaign_id: binding.campaign_id,
            trial_fingerprint,
            case_id: binding.case_id,
            treatment_fingerprint: binding.treatment_fingerprint,
            batch_fingerprint,
            ordered_occurrences,
            store_authority_domain_fingerprint,
            evidence_fingerprint,
        })
    }

    fn load_frozen_trial_pool_binding(
        &self,
        trial_fingerprint: BlobId,
    ) -> Result<FrozenTrialPoolBinding> {
        let raw = self
            .connection
            .query_row(
                "SELECT campaign.project_id, trial.campaign_id,
                        trial.trial_case_id, trial.treatment_fingerprint,
                        trial.expected_writer_call_count
                 FROM research_trial_specs trial
                 JOIN research_campaigns campaign
                   ON campaign.campaign_id = trial.campaign_id
                 WHERE trial.trial_fingerprint = ?1",
                params![trial_fingerprint.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| invalid_pool("frozen trial row does not exist"))?;
        let expected_writer_call_count = u16::try_from(raw.4)
            .ok()
            .filter(|value| *value != 0)
            .ok_or_else(|| invalid_pool("stored expected writer-call count is malformed"))?;
        Ok(FrozenTrialPoolBinding {
            project_id: ProjectId::from_str(&raw.0)
                .map_err(|_| invalid_pool("stored trial project ID is malformed"))?,
            campaign_id: CampaignId::from_str(&raw.1)
                .map_err(|_| invalid_pool("stored trial campaign ID is malformed"))?,
            case_id: TrialCaseId::from_str(&raw.2)
                .map_err(|_| invalid_pool("stored trial case ID is malformed"))?,
            treatment_fingerprint: BlobId::from_str(&raw.3)
                .map_err(|_| invalid_pool("stored trial treatment fingerprint is malformed"))?,
            expected_writer_call_count,
        })
    }

    fn verify_pool_candidate_rows(
        &self,
        trial_fingerprint: BlobId,
        binding: FrozenTrialPoolBinding,
        candidate: &CampaignPoolCandidateLease<'_>,
        expected_position: usize,
    ) -> Result<VerifiedPoolCandidateRow> {
        let row = self
            .connection
            .query_row(
                VERIFIED_POOL_CANDIDATE_SQL,
                params![
                    candidate.span.occurrence_id().to_string(),
                    candidate.projection.projection_id().to_string(),
                    trial_fingerprint.to_string(),
                    binding.campaign_id.to_string(),
                    binding.case_id.to_string(),
                    binding.treatment_fingerprint.to_string(),
                    binding.project_id.to_string(),
                    candidate
                        .projection
                        .admission_record_id()
                        .as_blob_id()
                        .to_string(),
                    i64::try_from(expected_position)
                        .map_err(|_| invalid_pool("candidate position exceeds SQLite domain"))?,
                    i64::from(binding.expected_writer_call_count),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                invalid_pool(
                    "candidate is not an exact admitted single-span projection for the frozen trial",
                )
            })?;
        Ok(VerifiedPoolCandidateRow {
            projection_fingerprint: BlobId::from_str(&row.0)
                .map_err(|_| invalid_pool("stored projection fingerprint is malformed"))?,
            batch_fingerprint: BlobId::from_str(&row.1)
                .map_err(|_| invalid_pool("stored batch fingerprint is malformed"))?,
            call_id: ModelCallId::from_str(&row.2)
                .map_err(|_| invalid_pool("stored model-call ID is malformed"))?,
        })
    }
}

fn pool_evidence_fingerprint(
    session_nonce: &[u8; 32],
    trial_fingerprint: BlobId,
    binding: FrozenTrialPoolBinding,
    batch_fingerprint: BlobId,
    ordered_occurrences: &[ArtifactId],
    ordered_projection_fingerprints: &[BlobId],
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/store-verified-campaign-nested-pool/v1\0");
    digest.update(session_nonce);
    digest.update(binding.project_id.as_ulid().to_bytes());
    digest.update(binding.campaign_id.as_ulid().to_bytes());
    digest.update(trial_fingerprint.as_bytes());
    digest.update(binding.case_id.as_ulid().to_bytes());
    digest.update(binding.treatment_fingerprint.as_bytes());
    digest.update(batch_fingerprint.as_bytes());
    digest.update(
        u64::try_from(ordered_occurrences.len())
            .expect("campaign pool count is bounded to 32")
            .to_be_bytes(),
    );
    for (occurrence, projection) in ordered_occurrences
        .iter()
        .zip(ordered_projection_fingerprints)
    {
        digest.update(occurrence.as_ulid().to_bytes());
        digest.update(projection.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn invalid_pool(message: &str) -> StoreError {
    StoreError::InvalidResearchDiagnostic(message.into())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::research_admission::tests::{
        CampaignPoolAdmissionTestFixture, admitted_additional_campaign_pool_batch_for_test,
        admitted_campaign_pool_for_test,
    };
    use crate::{
        FrozenCampaignPersistence, FrozenStagePersistence, FrozenTrialPersistence,
        ResearchBudgetMaximum, ResearchJournalBudget, StandaloneTrialRunPersistence,
        TrialJournalEventPersistence, TrialJournalMutation, TrialStageOutcome,
    };
    use loom_research_types::{
        FrozenTrialStage, StageId, TrialRunId, TrialRunOrigin, TrialRunRecord,
    };

    struct PoolFixture {
        directory: tempfile::TempDir,
        store: ProjectStore,
        admission: CampaignPoolAdmissionTestFixture,
        trial_fingerprint: BlobId,
    }

    fn pool_fixture(candidate_count: usize) -> PoolFixture {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "campaign pool").expect("store");
        let admission = admitted_campaign_pool_for_test(&mut store, candidate_count);
        let trial_fingerprint = BlobId::digest(b"campaign pool frozen trial");
        persist_trial_binding(&mut store, &admission, trial_fingerprint);
        PoolFixture {
            directory,
            store,
            admission,
            trial_fingerprint,
        }
    }

    fn persist_trial_binding(
        store: &mut ProjectStore,
        admission: &CampaignPoolAdmissionTestFixture,
        trial_fingerprint: BlobId,
    ) {
        let maximum = ResearchBudgetMaximum {
            writer_tokens: 4_096,
            controller_tokens: 0,
            evaluations: 64,
            wall_time_ms: 60_000,
        };
        store
            .persist_frozen_campaign(FrozenCampaignPersistence {
                campaign_id: admission.campaign_id,
                campaign_fingerprint: BlobId::digest(b"campaign pool campaign"),
                project_id: store.manifest.project_id,
                manifest_source_bytes: b"campaign pool manifest",
                manifest_fingerprint: BlobId::digest(b"campaign pool manifest artifact"),
                project_input_fingerprint: BlobId::digest(b"campaign pool project input"),
                seed: 17,
                maximum,
                canonical_record_bytes: b"campaign pool canonical campaign",
            })
            .expect("persist campaign");
        let mut stage_ids = FrozenTrialStage::ALL.map(|_| StageId::new());
        stage_ids[5] = admission.stage_id;
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
        let stage_records = FrozenTrialStage::ALL
            .iter()
            .enumerate()
            .map(|(index, stage)| format!("campaign pool stage {index} {stage:?}").into_bytes())
            .collect::<Vec<_>>();
        let stages = FrozenTrialStage::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, stage)| FrozenStagePersistence {
                stage_id: stage_ids[index],
                stage,
                stage_spec_fingerprint: BlobId::digest(&stage_records[index]),
                maximum: ResearchBudgetMaximum {
                    writer_tokens: u64::from(stage == FrozenTrialStage::Generate) * 4_096,
                    controller_tokens: 0,
                    evaluations: u32::from(stage == FrozenTrialStage::Evaluate) * 64,
                    wall_time_ms: 1_000,
                },
                dependencies: &dependencies[index],
                canonical_record_bytes: &stage_records[index],
            })
            .collect::<Vec<_>>();
        store
            .persist_frozen_trial(FrozenTrialPersistence {
                campaign_id: admission.campaign_id,
                trial_fingerprint,
                trial_case_id: admission.case_id,
                treatment_fingerprint: admission.treatment_fingerprint,
                prompt_content_fingerprint: admission.prompt_content_fingerprint,
                model_binding_fingerprint: admission.model_binding_fingerprint,
                expected_writer_call_count: u16::try_from(admission.candidates.len())
                    .expect("bounded test candidate count"),
                declared_writer_token_maximum: 4_096,
                maximum,
                canonical_record_bytes: b"campaign pool canonical trial",
                stages: &stages,
            })
            .expect("persist exact frozen trial through the production API");
        persist_succeeded_generate_attempt(store, admission, trial_fingerprint);
    }

    fn persist_succeeded_generate_attempt(
        store: &mut ProjectStore,
        admission: &CampaignPoolAdmissionTestFixture,
        trial_fingerprint: BlobId,
    ) {
        let trial_run_id = TrialRunId::new();
        let run_record =
            TrialRunRecord::new(trial_run_id, trial_fingerprint, TrialRunOrigin::Standalone);
        let run_bytes = run_record.canonical_bytes().expect("canonical run");
        store
            .persist_standalone_trial_run(StandaloneTrialRunPersistence {
                trial_run_id,
                trial_fingerprint,
                canonical_record_bytes: &run_bytes,
            })
            .expect("persist standalone run");
        let lease = store
            .acquire_trial_run_session(trial_run_id)
            .expect("trial session lease");
        let mut writer = store
            .open_research_journal_writer(lease)
            .expect("trial journal writer");
        let event_bytes = [
            b"campaign pool trial prepared".as_slice(),
            b"campaign pool generate reserved".as_slice(),
            b"campaign pool generate started".as_slice(),
            b"campaign pool generate succeeded".as_slice(),
        ];
        let event_fingerprints = event_bytes.map(BlobId::digest);
        let reservation = ResearchJournalBudget {
            writer_tokens: 4_096,
            controller_tokens: 0,
            evaluations: 0,
            wall_time_ms: 1_000,
        };
        writer
            .append_trial_event(TrialJournalEventPersistence {
                trial_run_id,
                trial_fingerprint,
                event_index: 0,
                previous_event_fingerprint: None,
                event_fingerprint: event_fingerprints[0],
                canonical_event_bytes: event_bytes[0],
                mutation: TrialJournalMutation::Prepared,
            })
            .expect("prepared event");
        writer
            .append_trial_event(TrialJournalEventPersistence {
                trial_run_id,
                trial_fingerprint,
                event_index: 1,
                previous_event_fingerprint: Some(event_fingerprints[0]),
                event_fingerprint: event_fingerprints[1],
                canonical_event_bytes: event_bytes[1],
                mutation: TrialJournalMutation::AttemptReserved {
                    attempt_id: admission.stage_attempt_id,
                    stage_id: admission.stage_id,
                    attempt_ordinal: 1,
                    reservation,
                    canonical_attempt_bytes: b"campaign pool exact generate attempt",
                    canonical_reservation_bytes: b"campaign pool exact generate reservation",
                },
            })
            .expect("generate reservation event");
        writer
            .append_trial_event(TrialJournalEventPersistence {
                trial_run_id,
                trial_fingerprint,
                event_index: 2,
                previous_event_fingerprint: Some(event_fingerprints[1]),
                event_fingerprint: event_fingerprints[2],
                canonical_event_bytes: event_bytes[2],
                mutation: TrialJournalMutation::AttemptStarted {
                    attempt_id: admission.stage_attempt_id,
                },
            })
            .expect("generate started event");
        writer
            .append_trial_event(TrialJournalEventPersistence {
                trial_run_id,
                trial_fingerprint,
                event_index: 3,
                previous_event_fingerprint: Some(event_fingerprints[2]),
                event_fingerprint: event_fingerprints[3],
                canonical_event_bytes: event_bytes[3],
                mutation: TrialJournalMutation::AttemptFinished {
                    attempt_id: admission.stage_attempt_id,
                    outcome: TrialStageOutcome::Succeeded,
                    terminal_output_fingerprint: Some(admission.batch_fingerprint),
                    charge: ResearchJournalBudget {
                        writer_tokens: u64::try_from(admission.candidates.len())
                            .expect("bounded test candidate count"),
                        controller_tokens: 0,
                        evaluations: 0,
                        wall_time_ms: 1,
                    },
                    canonical_charge_bytes: b"campaign pool exact generate charge",
                },
            })
            .expect("generate succeeded event");
    }

    fn candidate_refs(
        admission: &CampaignPoolAdmissionTestFixture,
    ) -> Vec<CampaignPoolCandidateLease<'_>> {
        admission
            .candidates
            .iter()
            .map(|candidate| {
                CampaignPoolCandidateLease::new(&candidate.span, &candidate.projection)
            })
            .collect()
    }

    #[test]
    fn current_store_pool_requires_exact_batch_order_and_repeat_verification_is_idempotent() {
        let fixture = pool_fixture(2);
        let candidates = candidate_refs(&fixture.admission);
        let first = fixture
            .store
            .check_campaign_nested_pool(fixture.trial_fingerprint, &candidates)
            .expect("exact admitted pool");
        let repeated = fixture
            .store
            .check_campaign_nested_pool(fixture.trial_fingerprint, &candidates)
            .expect("idempotent re-verification");
        let copied_diagnostic = first.clone();
        assert_eq!(copied_diagnostic, first);
        assert_eq!(
            first.evidence_fingerprint(),
            repeated.evidence_fingerprint()
        );
        assert_eq!(first.ordered_occurrences(), repeated.ordered_occurrences());
        assert_eq!(
            first.batch_fingerprint(),
            fixture.admission.batch_fingerprint
        );
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(fixture.trial_fingerprint, &candidates[..1])
                .is_err(),
            "a strict pool cannot omit a sealed batch position"
        );

        let reversed = candidates.iter().copied().rev().collect::<Vec<_>>();
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(fixture.trial_fingerprint, &reversed)
                .is_err(),
            "an untyped caller permutation must not redefine sealed batch order"
        );
    }

    #[test]
    fn complete_confirmatory_pool_preserves_all_thirty_two_admitted_occurrences() {
        let fixture = pool_fixture(MAX_VERIFIED_CAMPAIGN_POOL_CANDIDATES);
        let candidates = candidate_refs(&fixture.admission);
        let checked = fixture
            .store
            .check_campaign_nested_pool(fixture.trial_fingerprint, &candidates)
            .expect("complete confirmatory pool");
        assert_eq!(
            checked.ordered_occurrences().len(),
            MAX_VERIFIED_CAMPAIGN_POOL_CANDIDATES
        );
        for (expected, actual) in fixture
            .admission
            .candidates
            .iter()
            .zip(checked.ordered_occurrences())
        {
            assert_eq!(
                ArtifactId::from_ulid(expected.span.occurrence_id().as_ulid()),
                *actual
            );
        }
    }

    #[test]
    fn pool_rejects_cross_candidate_pairing_and_prior_store_session_leases() {
        let mut fixture = pool_fixture(2);
        let exact = candidate_refs(&fixture.admission);
        let prior_pool = fixture
            .store
            .check_campaign_nested_pool(fixture.trial_fingerprint, &exact)
            .expect("current-session pool");
        let prior_domain = prior_pool.store_authority_domain_fingerprint();
        let crossed = [CampaignPoolCandidateLease::new(
            &fixture.admission.candidates[0].span,
            &fixture.admission.candidates[1].projection,
        )];
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(fixture.trial_fingerprint, &crossed)
                .is_err()
        );

        let project_path = fixture.directory.path().to_path_buf();
        drop(fixture.store);
        fixture.store = ProjectStore::open(project_path).expect("reopen exact project");
        assert_ne!(
            prior_domain,
            fixture.store.research_authority_domain_fingerprint(),
            "a pool proof must not bind a journal from a later store session"
        );
        let stale = candidate_refs(&fixture.admission);
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(fixture.trial_fingerprint, &stale)
                .is_err(),
            "reopening the same rows must invalidate old admission leases"
        );
    }

    #[test]
    fn pool_rejects_candidates_mixed_from_two_sealed_batches() {
        let mut fixture = pool_fixture(2);
        let additional = admitted_additional_campaign_pool_batch_for_test(
            &mut fixture.store,
            &fixture.admission,
            2,
        );
        assert_ne!(
            fixture.admission.batch_fingerprint,
            additional.batch_fingerprint
        );
        let mixed = [
            CampaignPoolCandidateLease::new(
                &fixture.admission.candidates[0].span,
                &fixture.admission.candidates[0].projection,
            ),
            CampaignPoolCandidateLease::new(
                &additional.candidates[1].span,
                &additional.candidates[1].projection,
            ),
        ];
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(fixture.trial_fingerprint, &mixed)
                .is_err(),
            "one strict pool cannot mix two sealed inference batches"
        );
    }

    #[test]
    fn deleting_the_exact_projection_admission_makes_pool_verification_fail_closed() {
        let fixture = pool_fixture(1);
        let candidates = candidate_refs(&fixture.admission);
        let _verified = fixture
            .store
            .check_campaign_nested_pool(fixture.trial_fingerprint, &candidates)
            .expect("pre-attack pool");
        fixture
            .store
            .connection
            .execute_batch(
                "DROP TRIGGER research_admission_records_immutable_delete;
                 DELETE FROM research_admission_records
                 WHERE subject_kind = 'candidate_projection';",
            )
            .expect("simulate hostile missing admission row");
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(fixture.trial_fingerprint, &candidates)
                .is_err()
        );
    }

    #[test]
    fn missing_occurrence_relation_and_unknown_trial_fail_closed() {
        let fixture = pool_fixture(1);
        let candidates = candidate_refs(&fixture.admission);
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(BlobId::digest(b"nonexistent trial"), &candidates)
                .is_err()
        );
        fixture
            .store
            .connection
            .execute_batch(
                "DROP TRIGGER research_assembly_parts_immutable_delete;
                 DELETE FROM research_candidate_assembly_parts;",
            )
            .expect("simulate hostile missing occurrence relation");
        assert!(
            fixture
                .store
                .check_campaign_nested_pool(fixture.trial_fingerprint, &candidates)
                .is_err()
        );
    }

    #[test]
    fn wrong_prompt_model_stage_or_unsealed_batch_cannot_enter_a_pool() {
        let compiled_prompt_attack = pool_fixture(1);
        let candidates = candidate_refs(&compiled_prompt_attack.admission);
        compiled_prompt_attack
            .store
            .connection
            .execute_batch("DROP TRIGGER research_trial_specs_immutable_update;")
            .expect("drop test immutability guard");
        compiled_prompt_attack
            .store
            .connection
            .execute(
                "UPDATE research_trial_specs SET prompt_content_fingerprint = ?1",
                [BlobId::digest(b"wrong prompt content").to_string()],
            )
            .expect("simulate wrong prompt binding");
        assert!(
            compiled_prompt_attack
                .store
                .check_campaign_nested_pool(compiled_prompt_attack.trial_fingerprint, &candidates,)
                .is_err()
        );

        let model_attack = pool_fixture(1);
        let candidates = candidate_refs(&model_attack.admission);
        model_attack
            .store
            .connection
            .execute_batch("DROP TRIGGER research_trial_specs_immutable_update;")
            .expect("drop test immutability guard");
        model_attack
            .store
            .connection
            .execute(
                "UPDATE research_trial_specs SET model_binding_fingerprint = ?1",
                [BlobId::digest(b"wrong model binding").to_string()],
            )
            .expect("simulate wrong model binding");
        assert!(
            model_attack
                .store
                .check_campaign_nested_pool(model_attack.trial_fingerprint, &candidates)
                .is_err()
        );

        let stage_attack = pool_fixture(1);
        let candidates = candidate_refs(&stage_attack.admission);
        stage_attack
            .store
            .connection
            .execute_batch("DROP TRIGGER research_campaign_stage_specs_immutable_update;")
            .expect("drop test immutability guard");
        stage_attack
            .store
            .connection
            .execute(
                "UPDATE research_campaign_stage_specs SET stage_kind = 'admit'
                 WHERE stage_kind = 'generate'",
                [],
            )
            .expect("simulate wrong generation stage");
        assert!(
            stage_attack
                .store
                .check_campaign_nested_pool(stage_attack.trial_fingerprint, &candidates)
                .is_err()
        );
    }

    #[test]
    fn pool_rejects_wrong_terminal_output_or_unsealed_batch() {
        let terminal_attack = pool_fixture(1);
        let candidates = candidate_refs(&terminal_attack.admission);
        terminal_attack
            .store
            .connection
            .execute_batch("DROP TRIGGER research_trial_events_immutable_update;")
            .expect("drop test trial-event immutability guard");
        terminal_attack
            .store
            .connection
            .execute(
                "UPDATE research_trial_events
                 SET terminal_output_fingerprint = ?1
                 WHERE event_kind = 'attempt_finished'",
                [BlobId::digest(b"unrelated successful output").to_string()],
            )
            .expect("simulate succeeded attempt for another output");
        assert!(
            terminal_attack
                .store
                .check_campaign_nested_pool(terminal_attack.trial_fingerprint, &candidates)
                .is_err(),
            "a succeeded attempt for another output cannot check this pool"
        );

        let seal_attack = pool_fixture(1);
        let candidates = candidate_refs(&seal_attack.admission);
        seal_attack
            .store
            .connection
            .execute_batch(
                "DROP TRIGGER research_verified_batch_seals_immutable_delete;
                 DELETE FROM research_verified_inference_batch_seals;",
            )
            .expect("simulate unsealed batch");
        assert!(
            seal_attack
                .store
                .check_campaign_nested_pool(seal_attack.trial_fingerprint, &candidates)
                .is_err()
        );
    }
}
