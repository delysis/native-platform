use std::collections::BTreeSet;
use std::fmt;

use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub const SCORE_SCALE: u32 = 1_000_000;
pub const MAX_EVIDENCE_REFS: usize = 64;

/// A deterministic fixed-point value in the inclusive range 0..=1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnitScore(u32);

impl UnitScore {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(SCORE_SCALE);

    pub const fn from_millionths(value: u32) -> Result<Self, ScoreError> {
        if value > SCORE_SCALE {
            return Err(ScoreError::OutOfRange(value));
        }
        Ok(Self(value))
    }

    pub const fn millionths(self) -> u32 {
        self.0
    }
}

impl Serialize for UnitScore {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.0)
    }
}

impl<'de> Deserialize<'de> for UnitScore {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::from_millionths(value).map_err(serde::de::Error::custom)
    }
}

/// A stable reference to evidence. The optional blob binds the exact bytes when
/// an artifact contains more than one projection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EvidenceRef {
    pub artifact_id: ArtifactId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<BlobId>,
}

impl EvidenceRef {
    pub const fn artifact(artifact_id: ArtifactId) -> Self {
        Self {
            artifact_id,
            blob_id: None,
        }
    }

    pub const fn artifact_blob(artifact_id: ArtifactId, blob_id: BlobId) -> Self {
        Self {
            artifact_id,
            blob_id: Some(blob_id),
        }
    }
}

pub(crate) fn validate_evidence(evidence: &[EvidenceRef]) -> Result<(), EvidenceError> {
    if evidence.is_empty() || evidence.len() > MAX_EVIDENCE_REFS {
        return Err(EvidenceError::InvalidCount(evidence.len()));
    }
    let mut unique = BTreeSet::new();
    for reference in evidence {
        if !unique.insert(*reference) {
            return Err(EvidenceError::Duplicate(*reference));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ScoreError {
    #[error("score {0} is outside 0..={SCORE_SCALE} millionths")]
    OutOfRange(u32),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EvidenceError {
    #[error("evidence count {0} is outside 1..={MAX_EVIDENCE_REFS}")]
    InvalidCount(usize),
    #[error("evidence reference {0:?} occurs more than once")]
    Duplicate(EvidenceRef),
}

impl fmt::Display for UnitScore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.0, SCORE_SCALE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range_score() {
        assert_eq!(
            UnitScore::from_millionths(SCORE_SCALE + 1),
            Err(ScoreError::OutOfRange(SCORE_SCALE + 1))
        );
    }

    #[test]
    fn rejects_duplicate_evidence_references() {
        let reference = EvidenceRef::artifact(ArtifactId::new());
        assert_eq!(
            validate_evidence(&[reference, reference]),
            Err(EvidenceError::Duplicate(reference))
        );
    }
}
