use std::collections::BTreeSet;

use loom_eval::CONFIRMATORY_N_VALUES;
use loom_research_types::TrialCaseId;
use loom_types::{ArtifactId, BlobId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

const NESTED_POOL_DOMAIN: &[u8] = b"loom/campaign-nested-pool/v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NestedPool {
    n: usize,
    ordered_occurrences: Vec<ArtifactId>,
}

impl NestedPool {
    pub const fn n(&self) -> usize {
        self.n
    }

    pub fn ordered_occurrences(&self) -> &[ArtifactId] {
        &self.ordered_occurrences
    }
}

/// The six exact, nested prefixes used by the confirmatory N curve.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NestedPoolPlan {
    case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    ordered_occurrences: Vec<ArtifactId>,
    pools: Vec<NestedPool>,
    exact_boundaries_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl NestedPoolPlan {
    pub fn new(
        case_id: TrialCaseId,
        treatment_fingerprint: BlobId,
        ordered_occurrences: Vec<ArtifactId>,
    ) -> Result<Self, NestedPoolError> {
        let required = *CONFIRMATORY_N_VALUES
            .last()
            .expect("confirmatory N constants are nonempty");
        if ordered_occurrences.len() != required {
            return Err(NestedPoolError::WrongOccurrenceCount {
                expected: required,
                actual: ordered_occurrences.len(),
            });
        }
        if ordered_occurrences
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != ordered_occurrences.len()
        {
            return Err(NestedPoolError::DuplicateOccurrence);
        }
        let pools = CONFIRMATORY_N_VALUES
            .iter()
            .map(|n| NestedPool {
                n: *n,
                ordered_occurrences: ordered_occurrences[..*n].to_vec(),
            })
            .collect::<Vec<_>>();
        let exact_boundaries_fingerprint = fingerprint_boundaries(&pools);
        let fingerprint = fingerprint_plan(
            case_id,
            treatment_fingerprint,
            &ordered_occurrences,
            exact_boundaries_fingerprint,
        );
        Ok(Self {
            case_id,
            treatment_fingerprint,
            ordered_occurrences,
            pools,
            exact_boundaries_fingerprint,
            fingerprint,
        })
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub fn ordered_occurrences(&self) -> &[ArtifactId] {
        &self.ordered_occurrences
    }

    pub fn pools(&self) -> &[NestedPool] {
        &self.pools
    }

    pub fn pool(&self, n: usize) -> Option<&NestedPool> {
        self.pools.iter().find(|pool| pool.n == n)
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub const fn exact_boundaries_fingerprint(&self) -> BlobId {
        self.exact_boundaries_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum NestedPoolError {
    #[error("nested pool requires {expected} occurrences, received {actual}")]
    WrongOccurrenceCount { expected: usize, actual: usize },
    #[error("nested pool repeats an occurrence")]
    DuplicateOccurrence,
}

fn fingerprint_plan(
    case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    occurrences: &[ArtifactId],
    exact_boundaries_fingerprint: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(NESTED_POOL_DOMAIN);
    digest.update(case_id.as_ulid().to_bytes());
    digest.update(treatment_fingerprint.as_bytes());
    digest.update(exact_boundaries_fingerprint.as_bytes());
    digest.update((occurrences.len() as u64).to_be_bytes());
    for occurrence in occurrences {
        digest.update(occurrence.as_ulid().to_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_boundaries(pools: &[NestedPool]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/campaign-nested-pool-boundaries/v1\0");
    digest.update((pools.len() as u64).to_be_bytes());
    for pool in pools {
        digest.update((pool.n as u64).to_be_bytes());
        digest.update((pool.ordered_occurrences.len() as u64).to_be_bytes());
        for occurrence in &pool.ordered_occurrences {
            digest.update(occurrence.as_ulid().to_bytes());
        }
    }
    BlobId::from_bytes(digest.finalize().into())
}
