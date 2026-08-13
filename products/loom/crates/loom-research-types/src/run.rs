use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Serialize};

use crate::{CampaignId, TrialRunId};

pub const TRIAL_RUN_RECORD_FORMAT: &str = "loom.trial-run.v1";

/// Durable origin of one execution of a frozen trial specification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrialRunOrigin {
    Campaign {
        campaign_id: CampaignId,
        campaign_fingerprint: BlobId,
    },
    Standalone,
    Benchmark {
        benchmark_run_id: ArtifactId,
        seal_fingerprint: BlobId,
        assignment_fingerprint: BlobId,
    },
}

/// Canonical, content-addressed identity record for one trial execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrialRunRecord {
    format: String,
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    origin: TrialRunOrigin,
}

impl TrialRunRecord {
    pub fn new(
        trial_run_id: TrialRunId,
        trial_fingerprint: BlobId,
        origin: TrialRunOrigin,
    ) -> Self {
        Self {
            format: TRIAL_RUN_RECORD_FORMAT.to_owned(),
            trial_run_id,
            trial_fingerprint,
            origin,
        }
    }

    pub const fn trial_run_id(&self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn origin(&self) -> TrialRunOrigin {
        self.origin
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn record_fingerprint(&self) -> Result<BlobId, serde_json::Error> {
        self.canonical_bytes().map(|bytes| BlobId::digest(&bytes))
    }
}
