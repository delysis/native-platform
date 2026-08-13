use std::cmp::Ordering;
use std::collections::BTreeSet;

use loom_search::UnitScore;
use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_HALVING_CANDIDATES: usize = 4_096;

const HALVING_DOMAIN: &[u8] = b"loom/successive-halving-decision/v1\0";

/// Pessimistic evidence state for one arm at one halving rung.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HalvingOutcome {
    Scored { score: UnitScore },
    Abstained,
    HardGateRejected,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HalvingCandidate {
    trial_fingerprint: BlobId,
    occurrence_id: ArtifactId,
    outcome: HalvingOutcome,
}

impl HalvingCandidate {
    pub const fn new(
        trial_fingerprint: BlobId,
        occurrence_id: ArtifactId,
        outcome: HalvingOutcome,
    ) -> Self {
        Self {
            trial_fingerprint,
            occurrence_id,
            outcome,
        }
    }

    pub const fn trial_fingerprint(self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn occurrence_id(self) -> ArtifactId {
        self.occurrence_id
    }

    pub const fn outcome(self) -> HalvingOutcome {
        self.outcome
    }
}

/// One deterministic rung decision. Explicit scores always outrank
/// abstentions, including an explicit zero; abstentions outrank hard-gate
/// rejection. Stable occurrence identity breaks otherwise exact ties.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SuccessiveHalvingDecision {
    seed: u64,
    round: u16,
    ranking: Vec<HalvingCandidate>,
    survivors: Vec<HalvingCandidate>,
    eliminated: Vec<HalvingCandidate>,
    tie_broken_at_cutoff: bool,
    fingerprint: BlobId,
}

impl SuccessiveHalvingDecision {
    pub fn decide(
        seed: u64,
        round: u16,
        candidates: Vec<HalvingCandidate>,
        survivor_count: usize,
    ) -> Result<Self, HalvingError> {
        if round == 0 {
            return Err(HalvingError::InvalidRound);
        }
        if candidates.len() < 2 || candidates.len() > MAX_HALVING_CANDIDATES {
            return Err(HalvingError::InvalidCandidateCount(candidates.len()));
        }
        if survivor_count == 0 || survivor_count >= candidates.len() {
            return Err(HalvingError::InvalidSurvivorCount {
                survivors: survivor_count,
                candidates: candidates.len(),
            });
        }
        let mut occurrences = BTreeSet::new();
        let mut trials = BTreeSet::new();
        for candidate in &candidates {
            if !occurrences.insert(candidate.occurrence_id) {
                return Err(HalvingError::DuplicateOccurrence(candidate.occurrence_id));
            }
            if !trials.insert(candidate.trial_fingerprint) {
                return Err(HalvingError::DuplicateTrial(candidate.trial_fingerprint));
            }
        }
        let mut ranking = candidates;
        ranking.sort_unstable_by(|left, right| compare_candidates(seed, left, right));
        let tie_broken_at_cutoff = same_evidence(
            ranking[survivor_count - 1].outcome,
            ranking[survivor_count].outcome,
        );
        let survivors = ranking[..survivor_count].to_vec();
        let eliminated = ranking[survivor_count..].to_vec();
        let fingerprint = fingerprint_decision(seed, round, &ranking, survivor_count);
        Ok(Self {
            seed,
            round,
            ranking,
            survivors,
            eliminated,
            tie_broken_at_cutoff,
            fingerprint,
        })
    }

    pub fn halve(
        seed: u64,
        round: u16,
        candidates: Vec<HalvingCandidate>,
    ) -> Result<Self, HalvingError> {
        let survivors = candidates.len().div_ceil(2);
        Self::decide(seed, round, candidates, survivors)
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    pub const fn round(&self) -> u16 {
        self.round
    }

    pub fn ranking(&self) -> &[HalvingCandidate] {
        &self.ranking
    }

    pub fn survivors(&self) -> &[HalvingCandidate] {
        &self.survivors
    }

    pub fn eliminated(&self) -> &[HalvingCandidate] {
        &self.eliminated
    }

    pub const fn tie_broken_at_cutoff(&self) -> bool {
        self.tie_broken_at_cutoff
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HalvingError {
    #[error("successive-halving rounds are one-based")]
    InvalidRound,
    #[error("halving candidate count {0} is outside 2..={MAX_HALVING_CANDIDATES}")]
    InvalidCandidateCount(usize),
    #[error("survivor count {survivors} must be in 1..{candidates}")]
    InvalidSurvivorCount { survivors: usize, candidates: usize },
    #[error("halving repeats occurrence {0}")]
    DuplicateOccurrence(ArtifactId),
    #[error("halving repeats trial {0}")]
    DuplicateTrial(BlobId),
}

fn compare_candidates(seed: u64, left: &HalvingCandidate, right: &HalvingCandidate) -> Ordering {
    compare_outcome(right.outcome, left.outcome)
        .then_with(|| seeded_tie_key(seed, left).cmp(&seeded_tie_key(seed, right)))
        .then_with(|| left.trial_fingerprint.cmp(&right.trial_fingerprint))
        .then_with(|| left.occurrence_id.cmp(&right.occurrence_id))
}

fn compare_outcome(left: HalvingOutcome, right: HalvingOutcome) -> Ordering {
    outcome_rank(left)
        .cmp(&outcome_rank(right))
        .then_with(|| match (left, right) {
            (HalvingOutcome::Scored { score: left }, HalvingOutcome::Scored { score: right }) => {
                left.cmp(&right)
            }
            _ => Ordering::Equal,
        })
}

const fn outcome_rank(outcome: HalvingOutcome) -> u8 {
    match outcome {
        HalvingOutcome::Scored { .. } => 2,
        HalvingOutcome::Abstained => 1,
        HalvingOutcome::HardGateRejected => 0,
    }
}

const fn same_evidence(left: HalvingOutcome, right: HalvingOutcome) -> bool {
    match (left, right) {
        (HalvingOutcome::Scored { score: left }, HalvingOutcome::Scored { score: right }) => {
            left.millionths() == right.millionths()
        }
        (HalvingOutcome::Abstained, HalvingOutcome::Abstained)
        | (HalvingOutcome::HardGateRejected, HalvingOutcome::HardGateRejected) => true,
        _ => false,
    }
}

fn fingerprint_decision(
    seed: u64,
    round: u16,
    ranking: &[HalvingCandidate],
    survivor_count: usize,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(HALVING_DOMAIN);
    digest.update(seed.to_be_bytes());
    digest.update(round.to_be_bytes());
    digest.update((survivor_count as u64).to_be_bytes());
    for candidate in ranking {
        digest.update(candidate.trial_fingerprint.as_bytes());
        digest.update(candidate.occurrence_id.as_ulid().to_bytes());
        match candidate.outcome {
            HalvingOutcome::Scored { score } => {
                digest.update([0]);
                digest.update(score.millionths().to_be_bytes());
            }
            HalvingOutcome::Abstained => digest.update([1]),
            HalvingOutcome::HardGateRejected => digest.update([2]),
        }
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn seeded_tie_key(seed: u64, candidate: &HalvingCandidate) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/halving-tie/v1\0");
    digest.update(seed.to_be_bytes());
    digest.update(candidate.trial_fingerprint.as_bytes());
    digest.update(candidate.occurrence_id.as_ulid().to_bytes());
    BlobId::from_bytes(digest.finalize().into())
}
