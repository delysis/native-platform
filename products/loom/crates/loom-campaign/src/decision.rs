use std::collections::BTreeSet;

use loom_research_types::{BoundedVec, TrialCaseId};
use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_DECISION_TRIAL_REFERENCES: usize = 4_096;

const DECISION_DOMAIN: &[u8] = b"loom/campaign-search-decision/v2\0";

/// Replayable effect of a live, internally verified search operation.
///
/// Values decoded from storage remain claims. Only the live journal methods
/// that consume private evaluation leases can create new receipts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchDecisionEffect {
    BlockedFactorialScheduled {
        artifact_fingerprint: BlobId,
        seed: u64,
        seeded_trial_order: BoundedVec<BlobId, MAX_DECISION_TRIAL_REFERENCES>,
    },
    NestedPoolRecorded {
        artifact_fingerprint: BlobId,
        case_id: TrialCaseId,
        treatment_fingerprint: BlobId,
        exact_boundaries_fingerprint: BlobId,
        pool_evidence_fingerprint: BlobId,
    },
    SuccessiveHalvingApplied {
        artifact_fingerprint: BlobId,
        rung: u16,
        case_id: TrialCaseId,
        prior_decision_fingerprint: Option<BlobId>,
        evaluation_coverage_fingerprint: BlobId,
        equal_budget_fingerprint: BlobId,
        survivors: BoundedVec<BlobId, MAX_DECISION_TRIAL_REFERENCES>,
        eliminated: BoundedVec<BlobId, MAX_DECISION_TRIAL_REFERENCES>,
    },
    PressureAdvanced {
        artifact_fingerprint: BlobId,
        family_fingerprint: BlobId,
        observed_level: u16,
        next_level: u16,
        affected_trials: BoundedVec<BlobId, MAX_DECISION_TRIAL_REFERENCES>,
        evaluation_coverage_fingerprint: BlobId,
        evaluation_receipt_fingerprint: BlobId,
        execution_charge_fingerprint: BlobId,
        cumulative_compute: u64,
        prior_decision_fingerprint: Option<BlobId>,
    },
    PressureStopped {
        artifact_fingerprint: BlobId,
        family_fingerprint: BlobId,
        observed_level: u16,
        stopped_trials: BoundedVec<BlobId, MAX_DECISION_TRIAL_REFERENCES>,
        evaluation_coverage_fingerprint: BlobId,
        evaluation_receipt_fingerprint: BlobId,
        execution_charge_fingerprint: BlobId,
        cumulative_compute: u64,
        prior_decision_fingerprint: Option<BlobId>,
    },
    MapElitesInitialized {
        artifact_fingerprint: BlobId,
        generation: u64,
    },
    MapElitesAdvanced {
        artifact_fingerprint: BlobId,
        generation: u64,
        parent_fingerprint: BlobId,
        admitted_occurrence_id: ArtifactId,
        admitted_occurrence_commitment: BlobId,
    },
}

impl SearchDecisionEffect {
    pub const fn artifact_fingerprint(&self) -> BlobId {
        match self {
            Self::BlockedFactorialScheduled {
                artifact_fingerprint,
                ..
            }
            | Self::NestedPoolRecorded {
                artifact_fingerprint,
                ..
            }
            | Self::SuccessiveHalvingApplied {
                artifact_fingerprint,
                ..
            }
            | Self::PressureAdvanced {
                artifact_fingerprint,
                ..
            }
            | Self::PressureStopped {
                artifact_fingerprint,
                ..
            }
            | Self::MapElitesInitialized {
                artifact_fingerprint,
                ..
            }
            | Self::MapElitesAdvanced {
                artifact_fingerprint,
                ..
            } => *artifact_fingerprint,
        }
    }

    pub(crate) fn referenced_trials(&self) -> Vec<BlobId> {
        match self {
            Self::BlockedFactorialScheduled {
                seeded_trial_order, ..
            } => seeded_trial_order.to_vec(),
            Self::SuccessiveHalvingApplied {
                survivors,
                eliminated,
                ..
            } => survivors.iter().chain(eliminated.iter()).copied().collect(),
            Self::PressureAdvanced {
                affected_trials, ..
            } => affected_trials.to_vec(),
            Self::PressureStopped { stopped_trials, .. } => stopped_trials.to_vec(),
            Self::NestedPoolRecorded { .. }
            | Self::MapElitesInitialized { .. }
            | Self::MapElitesAdvanced { .. } => Vec::new(),
        }
    }

    fn update_digest(&self, digest: &mut Sha256) {
        match self {
            Self::BlockedFactorialScheduled {
                artifact_fingerprint,
                seed,
                seeded_trial_order,
            } => {
                digest.update([0]);
                digest.update(artifact_fingerprint.as_bytes());
                digest.update(seed.to_be_bytes());
                update_blobs(digest, seeded_trial_order);
            }
            Self::NestedPoolRecorded {
                artifact_fingerprint,
                case_id,
                treatment_fingerprint,
                exact_boundaries_fingerprint,
                pool_evidence_fingerprint,
            } => {
                digest.update([1]);
                digest.update(artifact_fingerprint.as_bytes());
                digest.update(case_id.as_ulid().to_bytes());
                digest.update(treatment_fingerprint.as_bytes());
                digest.update(exact_boundaries_fingerprint.as_bytes());
                digest.update(pool_evidence_fingerprint.as_bytes());
            }
            Self::SuccessiveHalvingApplied {
                artifact_fingerprint,
                rung,
                case_id,
                prior_decision_fingerprint,
                evaluation_coverage_fingerprint,
                equal_budget_fingerprint,
                survivors,
                eliminated,
            } => {
                digest.update([2]);
                digest.update(artifact_fingerprint.as_bytes());
                digest.update(rung.to_be_bytes());
                digest.update(case_id.as_ulid().to_bytes());
                update_optional_blob(digest, *prior_decision_fingerprint);
                digest.update(evaluation_coverage_fingerprint.as_bytes());
                digest.update(equal_budget_fingerprint.as_bytes());
                update_blobs(digest, survivors);
                update_blobs(digest, eliminated);
            }
            Self::PressureAdvanced { .. } | Self::PressureStopped { .. } => {
                update_pressure_digest(self, digest);
            }
            Self::MapElitesInitialized {
                artifact_fingerprint,
                generation,
            } => {
                digest.update([5]);
                digest.update(artifact_fingerprint.as_bytes());
                digest.update(generation.to_be_bytes());
            }
            Self::MapElitesAdvanced {
                artifact_fingerprint,
                generation,
                parent_fingerprint,
                admitted_occurrence_id,
                admitted_occurrence_commitment,
            } => {
                digest.update([6]);
                digest.update(artifact_fingerprint.as_bytes());
                digest.update(generation.to_be_bytes());
                digest.update(parent_fingerprint.as_bytes());
                digest.update(admitted_occurrence_id.as_ulid().to_bytes());
                digest.update(admitted_occurrence_commitment.as_bytes());
            }
        }
    }
}

fn update_pressure_digest(effect: &SearchDecisionEffect, digest: &mut Sha256) {
    match effect {
        SearchDecisionEffect::PressureAdvanced {
            artifact_fingerprint,
            family_fingerprint,
            observed_level,
            next_level,
            affected_trials,
            evaluation_coverage_fingerprint,
            evaluation_receipt_fingerprint,
            execution_charge_fingerprint,
            cumulative_compute,
            prior_decision_fingerprint,
        } => {
            digest.update([3]);
            digest.update(artifact_fingerprint.as_bytes());
            digest.update(family_fingerprint.as_bytes());
            digest.update(observed_level.to_be_bytes());
            digest.update(next_level.to_be_bytes());
            update_blobs(digest, affected_trials);
            digest.update(evaluation_coverage_fingerprint.as_bytes());
            digest.update(evaluation_receipt_fingerprint.as_bytes());
            digest.update(execution_charge_fingerprint.as_bytes());
            digest.update(cumulative_compute.to_be_bytes());
            update_optional_blob(digest, *prior_decision_fingerprint);
        }
        SearchDecisionEffect::PressureStopped {
            artifact_fingerprint,
            family_fingerprint,
            observed_level,
            stopped_trials,
            evaluation_coverage_fingerprint,
            evaluation_receipt_fingerprint,
            execution_charge_fingerprint,
            cumulative_compute,
            prior_decision_fingerprint,
        } => {
            digest.update([4]);
            digest.update(artifact_fingerprint.as_bytes());
            digest.update(family_fingerprint.as_bytes());
            digest.update(observed_level.to_be_bytes());
            update_blobs(digest, stopped_trials);
            digest.update(evaluation_coverage_fingerprint.as_bytes());
            digest.update(evaluation_receipt_fingerprint.as_bytes());
            digest.update(execution_charge_fingerprint.as_bytes());
            digest.update(cumulative_compute.to_be_bytes());
            update_optional_blob(digest, *prior_decision_fingerprint);
        }
        _ => unreachable!("pressure digest helper receives only pressure effects"),
    }
}

/// Fingerprint-bound search claim emitted only by live campaign operations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchDecisionReceipt {
    decision_id: ArtifactId,
    effect: SearchDecisionEffect,
    referenced_trials: BoundedVec<BlobId, MAX_DECISION_TRIAL_REFERENCES>,
    fingerprint: BlobId,
}

impl SearchDecisionReceipt {
    pub(crate) fn from_verified_effect(
        decision_id: ArtifactId,
        effect: SearchDecisionEffect,
    ) -> Result<Self, SearchDecisionError> {
        let references = effect.referenced_trials();
        let unique = references.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != references.len() {
            return Err(SearchDecisionError::DuplicateTrialReference);
        }
        let referenced_trials = BoundedVec::new(unique.into_iter().collect())
            .map_err(|_| SearchDecisionError::InvalidTrialReferenceCount(references.len()))?;
        let fingerprint = fingerprint_receipt(decision_id, &effect, &referenced_trials);
        Ok(Self {
            decision_id,
            effect,
            referenced_trials,
            fingerprint,
        })
    }

    pub const fn decision_id(&self) -> ArtifactId {
        self.decision_id
    }

    pub const fn effect(&self) -> &SearchDecisionEffect {
        &self.effect
    }

    pub fn referenced_trials(&self) -> &[BlobId] {
        &self.referenced_trials
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub(crate) fn verify(&self) -> Result<(), SearchDecisionError> {
        if self.referenced_trials.len() > MAX_DECISION_TRIAL_REFERENCES {
            return Err(SearchDecisionError::InvalidTrialReferenceCount(
                self.referenced_trials.len(),
            ));
        }
        if self
            .referenced_trials
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(SearchDecisionError::NonCanonicalTrialReferences);
        }
        let expected = self
            .effect
            .referenced_trials()
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if expected.as_slice() != &*self.referenced_trials {
            return Err(SearchDecisionError::EffectReferenceMismatch);
        }
        if fingerprint_receipt(self.decision_id, &self.effect, &self.referenced_trials)
            != self.fingerprint
        {
            return Err(SearchDecisionError::FingerprintMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SearchDecisionError {
    #[error("search decision trial reference count {0} exceeds {MAX_DECISION_TRIAL_REFERENCES}")]
    InvalidTrialReferenceCount(usize),
    #[error("search decision trial references are not strictly ordered and unique")]
    NonCanonicalTrialReferences,
    #[error("search decision repeats a trial reference")]
    DuplicateTrialReference,
    #[error("search decision effect and reference index disagree")]
    EffectReferenceMismatch,
    #[error("search decision fingerprint mismatch")]
    FingerprintMismatch,
}

fn fingerprint_receipt(
    decision_id: ArtifactId,
    effect: &SearchDecisionEffect,
    referenced_trials: &[BlobId],
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(DECISION_DOMAIN);
    digest.update(decision_id.as_ulid().to_bytes());
    effect.update_digest(&mut digest);
    update_blobs(&mut digest, referenced_trials);
    BlobId::from_bytes(digest.finalize().into())
}

fn update_blobs(digest: &mut Sha256, values: &[BlobId]) {
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        digest.update(value.as_bytes());
    }
}

fn update_optional_blob(digest: &mut Sha256, value: Option<BlobId>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.as_bytes());
        }
        None => digest.update([0]),
    }
}
