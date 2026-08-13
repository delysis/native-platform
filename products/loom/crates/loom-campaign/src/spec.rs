use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{CampaignId, CompiledManifest, ManifestDocument, TrialCaseId};
use loom_trial::FrozenTrialSpec;
use loom_types::{BlobId, ProjectId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{CampaignBudgetAmount, CampaignBudgetError, CampaignBudgetLimits};

pub const MAX_FROZEN_CAMPAIGN_TRIALS: usize = 65_536;
pub const MAX_CAMPAIGN_TRIAL_DEPENDENCIES: usize = 256;

const TRIAL_NODE_DOMAIN: &[u8] = b"loom/frozen-campaign-trial/v1\0";
const CAMPAIGN_DOMAIN: &[u8] = b"loom/frozen-campaign/v1\0";
const FROZEN_CAMPAIGN_RECORD_FORMAT: &str = "loom.frozen-campaign-spec.v1";

#[derive(Serialize)]
struct FrozenCampaignCanonicalRecord<'a> {
    format: &'static str,
    spec: &'a FrozenCampaignSpec,
}

#[derive(Clone, Copy, Debug)]
pub struct FrozenCampaignTrialInput<'a> {
    pub trial: &'a FrozenTrialSpec,
    pub dependencies: &'a [BlobId],
}

#[derive(Clone, Copy, Debug)]
pub struct FrozenCampaignInputs<'a> {
    pub campaign_id: CampaignId,
    pub campaign_manifest: &'a CompiledManifest,
    pub project_input_fingerprint: BlobId,
    pub max_wall_time_ms: u64,
    pub trials: &'a [FrozenCampaignTrialInput<'a>],
}

/// One immutable trial node in a frozen exploratory campaign.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenCampaignTrial {
    trial_fingerprint: BlobId,
    case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    budget_maximum: CampaignBudgetAmount,
    dependencies: Vec<BlobId>,
    fingerprint: BlobId,
}

impl FrozenCampaignTrial {
    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub const fn budget_maximum(&self) -> CampaignBudgetAmount {
        self.budget_maximum
    }

    pub fn dependencies(&self) -> &[BlobId] {
        &self.dependencies
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

/// Content-bound campaign topology and aggregate resource ceilings.
///
/// This artifact is serializable for inspection but deliberately cannot be
/// deserialized into a trusted frozen campaign.
///
/// ```compile_fail
/// use loom_campaign::FrozenCampaignSpec;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<FrozenCampaignSpec>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenCampaignSpec {
    campaign_id: CampaignId,
    project_id: ProjectId,
    project_input_fingerprint: BlobId,
    manifest_source_fingerprint: BlobId,
    manifest_fingerprint: BlobId,
    seed: u64,
    budget_limits: CampaignBudgetLimits,
    trials: Vec<FrozenCampaignTrial>,
    fingerprint: BlobId,
}

impl FrozenCampaignSpec {
    pub fn compile(inputs: FrozenCampaignInputs<'_>) -> Result<Self, CampaignSpecError> {
        inputs
            .campaign_manifest
            .verify_integrity()
            .map_err(|_| CampaignSpecError::ManifestIntegrity)?;
        let ManifestDocument::Campaign(manifest) = inputs.campaign_manifest.document() else {
            return Err(CampaignSpecError::WrongManifestFormat);
        };
        if inputs.trials.is_empty() || inputs.trials.len() > MAX_FROZEN_CAMPAIGN_TRIALS {
            return Err(CampaignSpecError::InvalidTrialCount(inputs.trials.len()));
        }
        let budget_limits = CampaignBudgetLimits::new(
            manifest.budget().max_writer_tokens,
            manifest.budget().max_controller_tokens,
            manifest.budget().max_evaluations,
            inputs.max_wall_time_ms,
        )?;

        let first = inputs.trials[0].trial;
        let project_id = first.project_id();
        let manifest_source_fingerprint = inputs.campaign_manifest.source_hash().as_blob_id();
        let manifest_fingerprint = inputs.campaign_manifest.artifact_hash().as_blob_id();
        let mut by_fingerprint = BTreeMap::new();
        let mut case_treatments = BTreeSet::new();
        for input in inputs.trials {
            let trial = input.trial;
            if trial.campaign_id() != inputs.campaign_id {
                return Err(CampaignSpecError::CampaignIdMismatch);
            }
            if trial.project_id() != project_id {
                return Err(CampaignSpecError::ProjectIdMismatch);
            }
            if trial.project_input_fingerprint() != inputs.project_input_fingerprint {
                return Err(CampaignSpecError::ProjectSourceMismatch);
            }
            if trial.campaign_manifest_source_fingerprint() != manifest_source_fingerprint
                || trial.campaign_manifest_fingerprint() != manifest_fingerprint
            {
                return Err(CampaignSpecError::TrialManifestMismatch);
            }
            if input.dependencies.len() > MAX_CAMPAIGN_TRIAL_DEPENDENCIES {
                return Err(CampaignSpecError::TooManyDependencies {
                    trial: trial.fingerprint(),
                    actual: input.dependencies.len(),
                });
            }
            let dependencies = input.dependencies.iter().copied().collect::<BTreeSet<_>>();
            if dependencies.len() != input.dependencies.len() {
                return Err(CampaignSpecError::DuplicateDependency(trial.fingerprint()));
            }
            if dependencies.contains(&trial.fingerprint()) {
                return Err(CampaignSpecError::SelfDependency(trial.fingerprint()));
            }
            if !case_treatments.insert((trial.case_id(), trial.treatment_fingerprint())) {
                return Err(CampaignSpecError::DuplicateCaseTreatment {
                    case_id: trial.case_id(),
                    treatment: trial.treatment_fingerprint(),
                });
            }
            let budget_maximum = CampaignBudgetAmount::from_trial_limits(trial.budget())?;
            if !budget_maximum.fits_limits(budget_limits) {
                return Err(CampaignSpecError::TrialBudgetExceedsCampaign(
                    trial.fingerprint(),
                ));
            }
            let mut node = FrozenCampaignTrial {
                trial_fingerprint: trial.fingerprint(),
                case_id: trial.case_id(),
                treatment_fingerprint: trial.treatment_fingerprint(),
                budget_maximum,
                dependencies: dependencies.into_iter().collect(),
                fingerprint: BlobId::digest(b"uninitialized campaign trial"),
            };
            node.fingerprint = fingerprint_trial_node(&node);
            if by_fingerprint
                .insert(node.trial_fingerprint, node)
                .is_some()
            {
                return Err(CampaignSpecError::DuplicateTrial(trial.fingerprint()));
            }
        }
        validate_dependencies(&by_fingerprint)?;
        let trials = by_fingerprint.into_values().collect::<Vec<_>>();
        let mut spec = Self {
            campaign_id: inputs.campaign_id,
            project_id,
            project_input_fingerprint: inputs.project_input_fingerprint,
            manifest_source_fingerprint,
            manifest_fingerprint,
            seed: manifest.seed(),
            budget_limits,
            trials,
            fingerprint: BlobId::digest(b"uninitialized campaign"),
        };
        spec.fingerprint = fingerprint_campaign(&spec);
        Ok(spec)
    }

    pub const fn campaign_id(&self) -> CampaignId {
        self.campaign_id
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn project_input_fingerprint(&self) -> BlobId {
        self.project_input_fingerprint
    }

    pub const fn manifest_source_fingerprint(&self) -> BlobId {
        self.manifest_source_fingerprint
    }

    pub const fn manifest_fingerprint(&self) -> BlobId {
        self.manifest_fingerprint
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    pub const fn budget_limits(&self) -> CampaignBudgetLimits {
        self.budget_limits
    }

    pub fn trials(&self) -> &[FrozenCampaignTrial] {
        &self.trials
    }

    pub fn trial(&self, fingerprint: BlobId) -> Option<&FrozenCampaignTrial> {
        self.trials
            .binary_search_by_key(&fingerprint, FrozenCampaignTrial::trial_fingerprint)
            .ok()
            .map(|index| &self.trials[index])
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    /// Exact deterministic JSON persisted as this frozen campaign's canonical
    /// record. Its digest is bound independently by the live store lease.
    pub fn canonical_record_bytes(&self) -> Result<Vec<u8>, CampaignSpecError> {
        serde_json::to_vec(&FrozenCampaignCanonicalRecord {
            format: FROZEN_CAMPAIGN_RECORD_FORMAT,
            spec: self,
        })
        .map_err(|_| CampaignSpecError::CanonicalRecordSerialization)
    }

    pub fn canonical_record_fingerprint(&self) -> Result<BlobId, CampaignSpecError> {
        self.canonical_record_bytes()
            .map(|bytes| BlobId::digest(&bytes))
    }

    pub(crate) fn verify(&self) -> Result<(), CampaignSpecError> {
        self.budget_limits.verify()?;
        if fingerprint_campaign(self) != self.fingerprint {
            return Err(CampaignSpecError::FingerprintMismatch);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(
        campaign_id: CampaignId,
        project_id: ProjectId,
        project_input_fingerprint: BlobId,
        campaign_manifest: &CompiledManifest,
        max_wall_time_ms: u64,
        trials: Vec<DiagnosticTrialInput>,
    ) -> Result<Self, CampaignSpecError> {
        campaign_manifest
            .verify_integrity()
            .map_err(|_| CampaignSpecError::ManifestIntegrity)?;
        let ManifestDocument::Campaign(manifest) = campaign_manifest.document() else {
            return Err(CampaignSpecError::WrongManifestFormat);
        };
        let budget_limits = CampaignBudgetLimits::new(
            manifest.budget().max_writer_tokens,
            manifest.budget().max_controller_tokens,
            manifest.budget().max_evaluations,
            max_wall_time_ms,
        )?;
        let mut by_fingerprint = BTreeMap::new();
        for trial in trials {
            let mut node = FrozenCampaignTrial {
                trial_fingerprint: trial.trial_fingerprint,
                case_id: trial.case_id,
                treatment_fingerprint: trial.treatment_fingerprint,
                budget_maximum: CampaignBudgetAmount::from_trial_limits(trial.budget)?,
                dependencies: trial.dependencies,
                fingerprint: BlobId::digest(b"uninitialized diagnostic campaign trial"),
            };
            node.dependencies.sort_unstable();
            node.fingerprint = fingerprint_trial_node(&node);
            if by_fingerprint
                .insert(node.trial_fingerprint, node)
                .is_some()
            {
                return Err(CampaignSpecError::DuplicateTrial(trial.trial_fingerprint));
            }
        }
        validate_dependencies(&by_fingerprint)?;
        let mut spec = Self {
            campaign_id,
            project_id,
            project_input_fingerprint,
            manifest_source_fingerprint: campaign_manifest.source_hash().as_blob_id(),
            manifest_fingerprint: campaign_manifest.artifact_hash().as_blob_id(),
            seed: manifest.seed(),
            budget_limits,
            trials: by_fingerprint.into_values().collect(),
            fingerprint: BlobId::digest(b"uninitialized diagnostic campaign"),
        };
        spec.fingerprint = fingerprint_campaign(&spec);
        Ok(spec)
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct DiagnosticTrialInput {
    pub trial_fingerprint: BlobId,
    pub case_id: TrialCaseId,
    pub treatment_fingerprint: BlobId,
    pub budget: loom_trial::TrialBudgetLimits,
    pub dependencies: Vec<BlobId>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CampaignSpecError {
    #[error(transparent)]
    Budget(#[from] CampaignBudgetError),
    #[error("campaign manifest failed its integrity check")]
    ManifestIntegrity,
    #[error("expected a loom.campaign.v1 manifest")]
    WrongManifestFormat,
    #[error("frozen trial count {0} is outside 1..={MAX_FROZEN_CAMPAIGN_TRIALS}")]
    InvalidTrialCount(usize),
    #[error("a frozen trial belongs to a different campaign occurrence")]
    CampaignIdMismatch,
    #[error("frozen trials belong to different projects")]
    ProjectIdMismatch,
    #[error("a frozen trial references a different exact project source")]
    ProjectSourceMismatch,
    #[error("a frozen trial references a different campaign manifest")]
    TrialManifestMismatch,
    #[error(
        "trial {trial} has {actual} dependencies; maximum is {MAX_CAMPAIGN_TRIAL_DEPENDENCIES}"
    )]
    TooManyDependencies { trial: BlobId, actual: usize },
    #[error("trial {0} repeats a dependency")]
    DuplicateDependency(BlobId),
    #[error("trial {0} depends on itself")]
    SelfDependency(BlobId),
    #[error("trial {trial} depends on unknown trial {dependency}")]
    UnknownDependency { trial: BlobId, dependency: BlobId },
    #[error("campaign trial dependencies contain a cycle")]
    DependencyCycle,
    #[error("trial fingerprint {0} occurs more than once")]
    DuplicateTrial(BlobId),
    #[error("case {case_id} repeats treatment {treatment}")]
    DuplicateCaseTreatment {
        case_id: TrialCaseId,
        treatment: BlobId,
    },
    #[error("trial {0} has a maximum larger than its campaign budget")]
    TrialBudgetExceedsCampaign(BlobId),
    #[error("frozen campaign fingerprint mismatch")]
    FingerprintMismatch,
    #[error("frozen campaign canonical record serialization failed")]
    CanonicalRecordSerialization,
}

fn validate_dependencies(
    trials: &BTreeMap<BlobId, FrozenCampaignTrial>,
) -> Result<(), CampaignSpecError> {
    let mut indegree = trials
        .iter()
        .map(|(fingerprint, trial)| (*fingerprint, trial.dependencies.len()))
        .collect::<BTreeMap<_, _>>();
    let mut dependents = BTreeMap::<BlobId, Vec<BlobId>>::new();
    for trial in trials.values() {
        for dependency in &trial.dependencies {
            if !trials.contains_key(dependency) {
                return Err(CampaignSpecError::UnknownDependency {
                    trial: trial.trial_fingerprint,
                    dependency: *dependency,
                });
            }
            dependents
                .entry(*dependency)
                .or_default()
                .push(trial.trial_fingerprint);
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(trial, degree)| (*degree == 0).then_some(*trial))
        .collect::<BTreeSet<_>>();
    let mut visited = 0_usize;
    while let Some(trial) = ready.pop_first() {
        visited += 1;
        if let Some(children) = dependents.get(&trial) {
            for child in children {
                let degree = indegree.get_mut(child).expect("known dependent");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(*child);
                }
            }
        }
    }
    if visited != trials.len() {
        return Err(CampaignSpecError::DependencyCycle);
    }
    Ok(())
}

fn fingerprint_trial_node(node: &FrozenCampaignTrial) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(TRIAL_NODE_DOMAIN);
    digest.update(node.trial_fingerprint.as_bytes());
    digest.update(node.case_id.as_ulid().to_bytes());
    digest.update(node.treatment_fingerprint.as_bytes());
    node.budget_maximum.update_digest(&mut digest);
    digest.update((node.dependencies.len() as u64).to_be_bytes());
    for dependency in &node.dependencies {
        digest.update(dependency.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_campaign(spec: &FrozenCampaignSpec) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(CAMPAIGN_DOMAIN);
    digest.update(spec.campaign_id.as_ulid().to_bytes());
    digest.update(spec.project_id.as_ulid().to_bytes());
    digest.update(spec.project_input_fingerprint.as_bytes());
    digest.update(spec.manifest_source_fingerprint.as_bytes());
    digest.update(spec.manifest_fingerprint.as_bytes());
    digest.update(spec.seed.to_be_bytes());
    digest.update(spec.budget_limits.fingerprint().as_bytes());
    digest.update((spec.trials.len() as u64).to_be_bytes());
    for trial in &spec.trials {
        digest.update(trial.fingerprint.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}
