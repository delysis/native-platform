use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::ManifestKey;
use loom_types::BlobId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_FACTORIAL_AXES: usize = 64;
pub const MAX_FACTORIAL_ARMS: usize = 256;

const FACTORIAL_DOMAIN: &[u8] = b"loom/blocked-single-axis-factorial/v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FactorSetting {
    axis: ManifestKey,
    value_fingerprint: BlobId,
}

impl FactorSetting {
    pub const fn new(axis: ManifestKey, value_fingerprint: BlobId) -> Self {
        Self {
            axis,
            value_fingerprint,
        }
    }

    pub const fn axis(&self) -> &ManifestKey {
        &self.axis
    }

    pub const fn value_fingerprint(&self) -> BlobId {
        self.value_fingerprint
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FactorialArm {
    trial_fingerprint: BlobId,
    treatment_fingerprint: BlobId,
    settings: Vec<FactorSetting>,
}

impl FactorialArm {
    pub fn new(
        trial_fingerprint: BlobId,
        treatment_fingerprint: BlobId,
        settings: Vec<FactorSetting>,
    ) -> Result<Self, FactorialError> {
        if settings.is_empty() || settings.len() > MAX_FACTORIAL_AXES {
            return Err(FactorialError::InvalidAxisCount(settings.len()));
        }
        let mut by_axis = BTreeMap::new();
        for setting in settings {
            if by_axis
                .insert(setting.axis.clone(), setting.value_fingerprint)
                .is_some()
            {
                return Err(FactorialError::DuplicateAxis);
            }
        }
        Ok(Self {
            trial_fingerprint,
            treatment_fingerprint,
            settings: by_axis
                .into_iter()
                .map(|(axis, value_fingerprint)| FactorSetting {
                    axis,
                    value_fingerprint,
                })
                .collect(),
        })
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub fn settings(&self) -> &[FactorSetting] {
        &self.settings
    }
}

/// A blocked experiment in which every non-baseline arm changes exactly the
/// declared axis and leaves every other treatment coordinate unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BlockedFactorialPlan {
    block_fingerprint: BlobId,
    varying_axis: ManifestKey,
    baseline: FactorialArm,
    variants: Vec<FactorialArm>,
    fingerprint: BlobId,
}

impl BlockedFactorialPlan {
    pub fn new(
        block_fingerprint: BlobId,
        varying_axis: ManifestKey,
        baseline: FactorialArm,
        variants: Vec<FactorialArm>,
    ) -> Result<Self, FactorialError> {
        if variants.is_empty() || variants.len() >= MAX_FACTORIAL_ARMS {
            return Err(FactorialError::InvalidArmCount(variants.len() + 1));
        }
        let baseline_settings = as_map(&baseline);
        if !baseline_settings.contains_key(&varying_axis) {
            return Err(FactorialError::VaryingAxisAbsent);
        }
        let mut trial_ids = BTreeSet::from([baseline.trial_fingerprint]);
        let mut treatment_ids = BTreeSet::from([baseline.treatment_fingerprint]);
        let mut varying_values = BTreeSet::from([baseline_settings[&varying_axis]]);
        let mut checked = Vec::with_capacity(variants.len());
        for variant in variants {
            if !trial_ids.insert(variant.trial_fingerprint) {
                return Err(FactorialError::DuplicateTrial);
            }
            if !treatment_ids.insert(variant.treatment_fingerprint) {
                return Err(FactorialError::DuplicateTreatment);
            }
            let settings = as_map(&variant);
            if settings.keys().ne(baseline_settings.keys()) {
                return Err(FactorialError::AxisSetMismatch);
            }
            let differences = settings
                .iter()
                .filter(|(axis, value)| baseline_settings.get(*axis) != Some(*value))
                .map(|(axis, _)| *axis)
                .collect::<Vec<_>>();
            if differences.as_slice() != [&varying_axis] {
                return Err(FactorialError::ConfoundedArm {
                    differing_axes: differences.len(),
                });
            }
            if !varying_values.insert(settings[&varying_axis]) {
                return Err(FactorialError::DuplicateFactorLevel);
            }
            checked.push(variant);
        }
        checked.sort_unstable_by(|left, right| {
            as_map(left)[&varying_axis]
                .cmp(&as_map(right)[&varying_axis])
                .then_with(|| left.trial_fingerprint.cmp(&right.trial_fingerprint))
        });
        let mut plan = Self {
            block_fingerprint,
            varying_axis,
            baseline,
            variants: checked,
            fingerprint: BlobId::digest(b"uninitialized factorial"),
        };
        plan.fingerprint = fingerprint_plan(&plan);
        Ok(plan)
    }

    pub const fn block_fingerprint(&self) -> BlobId {
        self.block_fingerprint
    }

    pub const fn varying_axis(&self) -> &ManifestKey {
        &self.varying_axis
    }

    pub const fn baseline(&self) -> &FactorialArm {
        &self.baseline
    }

    pub fn variants(&self) -> &[FactorialArm] {
        &self.variants
    }

    pub fn trial_fingerprints(&self) -> impl Iterator<Item = BlobId> + '_ {
        std::iter::once(self.baseline.trial_fingerprint)
            .chain(self.variants.iter().map(|arm| arm.trial_fingerprint))
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FactorialError {
    #[error("factorial axis count {0} is outside 1..={MAX_FACTORIAL_AXES}")]
    InvalidAxisCount(usize),
    #[error("factorial arm count {0} is outside 2..={MAX_FACTORIAL_ARMS}")]
    InvalidArmCount(usize),
    #[error("a factorial arm repeats an axis")]
    DuplicateAxis,
    #[error("the declared varying axis is absent from the baseline")]
    VaryingAxisAbsent,
    #[error("a factorial arm has a different axis set")]
    AxisSetMismatch,
    #[error("factorial arm is confounded; expected one changed axis, found {differing_axes}")]
    ConfoundedArm { differing_axes: usize },
    #[error("factorial arms repeat a trial fingerprint")]
    DuplicateTrial,
    #[error("factorial arms repeat a treatment fingerprint")]
    DuplicateTreatment,
    #[error("factorial arms repeat a factor level")]
    DuplicateFactorLevel,
}

fn as_map(arm: &FactorialArm) -> BTreeMap<&ManifestKey, BlobId> {
    arm.settings
        .iter()
        .map(|setting| (&setting.axis, setting.value_fingerprint))
        .collect()
}

fn fingerprint_plan(plan: &BlockedFactorialPlan) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(FACTORIAL_DOMAIN);
    digest.update(plan.block_fingerprint.as_bytes());
    update_text(&mut digest, plan.varying_axis.as_str());
    update_arm(&mut digest, &plan.baseline);
    digest.update((plan.variants.len() as u64).to_be_bytes());
    for arm in &plan.variants {
        update_arm(&mut digest, arm);
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn update_arm(digest: &mut Sha256, arm: &FactorialArm) {
    digest.update(arm.trial_fingerprint.as_bytes());
    digest.update(arm.treatment_fingerprint.as_bytes());
    digest.update((arm.settings.len() as u64).to_be_bytes());
    for setting in &arm.settings {
        update_text(digest, setting.axis.as_str());
        digest.update(setting.value_fingerprint.as_bytes());
    }
}

fn update_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}
