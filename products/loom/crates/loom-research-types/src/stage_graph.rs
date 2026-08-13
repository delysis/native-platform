use std::collections::{BTreeMap, BTreeSet};

use loom_types::BlobId;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{BoundError, BoundedVec, StageGraphId, StageId};

pub const MAX_TRIAL_STAGES: usize = 12;
pub const MAX_STAGE_DEPENDENCIES: usize = 4;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrozenTrialStage {
    FreezeInputs,
    BacktranslateMask,
    Plan,
    Retrieve,
    CompilePrompt,
    Generate,
    Admit,
    Assemble,
    Gate,
    Evaluate,
    Describe,
    Archive,
}

impl FrozenTrialStage {
    pub const ALL: [Self; MAX_TRIAL_STAGES] = [
        Self::FreezeInputs,
        Self::BacktranslateMask,
        Self::Plan,
        Self::Retrieve,
        Self::CompilePrompt,
        Self::Generate,
        Self::Admit,
        Self::Assemble,
        Self::Gate,
        Self::Evaluate,
        Self::Describe,
        Self::Archive,
    ];

    const fn required_dependencies(self) -> &'static [Self] {
        match self {
            Self::FreezeInputs => &[],
            Self::BacktranslateMask => &[Self::FreezeInputs],
            Self::Plan => &[Self::FreezeInputs, Self::BacktranslateMask],
            Self::Retrieve => &[Self::FreezeInputs, Self::Plan],
            Self::CompilePrompt => &[
                Self::FreezeInputs,
                Self::BacktranslateMask,
                Self::Plan,
                Self::Retrieve,
            ],
            Self::Generate => &[Self::CompilePrompt],
            Self::Admit => &[Self::Generate],
            Self::Assemble => &[Self::Admit],
            Self::Gate => &[Self::Assemble],
            Self::Evaluate => &[Self::Gate],
            Self::Describe => &[Self::Evaluate],
            Self::Archive => &[Self::Evaluate, Self::Describe],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenStageSpec {
    id: StageId,
    stage: FrozenTrialStage,
    spec_fingerprint: BlobId,
    dependencies: BoundedVec<StageId, MAX_STAGE_DEPENDENCIES>,
}

impl FrozenStageSpec {
    pub fn new(
        id: StageId,
        stage: FrozenTrialStage,
        spec_fingerprint: BlobId,
        dependencies: Vec<StageId>,
    ) -> Result<Self, StageGraphError> {
        let dependencies = BoundedVec::new(dependencies)?;
        if dependencies.iter().copied().collect::<BTreeSet<_>>().len() != dependencies.len() {
            return Err(StageGraphError::DuplicateDependency(id));
        }
        Ok(Self {
            id,
            stage,
            spec_fingerprint,
            dependencies,
        })
    }

    pub const fn id(&self) -> StageId {
        self.id
    }

    pub const fn stage(&self) -> FrozenTrialStage {
        self.stage
    }

    pub const fn spec_fingerprint(&self) -> BlobId {
        self.spec_fingerprint
    }

    pub fn dependencies(&self) -> &[StageId] {
        &self.dependencies
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenStageSpecWire {
    id: StageId,
    stage: FrozenTrialStage,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    spec_fingerprint: BlobId,
    dependencies: BoundedVec<StageId, MAX_STAGE_DEPENDENCIES>,
}

impl<'de> Deserialize<'de> for FrozenStageSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FrozenStageSpecWire::deserialize(deserializer)?;
        Self::new(
            wire.id,
            wire.stage,
            wire.spec_fingerprint,
            wire.dependencies.into_inner(),
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StageGraph {
    id: StageGraphId,
    stages: BoundedVec<FrozenStageSpec, MAX_TRIAL_STAGES>,
    output: StageId,
}

impl StageGraph {
    pub fn new(
        id: StageGraphId,
        stages: Vec<FrozenStageSpec>,
        output: StageId,
    ) -> Result<Self, StageGraphError> {
        if stages.is_empty() {
            return Err(StageGraphError::Empty);
        }
        let stages = BoundedVec::new(stages)?;
        validate_stage_graph(&stages, output)?;
        Ok(Self { id, stages, output })
    }

    pub const fn id(&self) -> StageGraphId {
        self.id
    }

    pub fn stages(&self) -> &[FrozenStageSpec] {
        &self.stages
    }

    pub const fn output(&self) -> StageId {
        self.output
    }

    pub fn stage(&self, id: StageId) -> Option<&FrozenStageSpec> {
        self.stages.iter().find(|stage| stage.id == id)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StageGraphWire {
    id: StageGraphId,
    stages: BoundedVec<FrozenStageSpec, MAX_TRIAL_STAGES>,
    output: StageId,
}

impl<'de> Deserialize<'de> for StageGraph {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StageGraphWire::deserialize(deserializer)?;
        Self::new(wire.id, wire.stages.into_inner(), wire.output).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StageGraphError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("stage graph is empty")]
    Empty,
    #[error("stage graph must declare all twelve frozen trial stages exactly once")]
    IncompleteStageSet,
    #[error("stage graph repeats stage id {0}")]
    DuplicateStageId(StageId),
    #[error("stage graph repeats frozen stage {0:?}")]
    DuplicateStageKind(FrozenTrialStage),
    #[error("stage {0} repeats a dependency")]
    DuplicateDependency(StageId),
    #[error("stage {stage} refers to absent or forward dependency {dependency}")]
    MissingOrForwardDependency { stage: StageId, dependency: StageId },
    #[error("stage {stage} has noncanonical dependency kinds")]
    InvalidDependencies { stage: StageId },
    #[error("stage graph order placed {actual:?} where {expected:?} was required")]
    InvalidStageOrder {
        actual: FrozenTrialStage,
        expected: FrozenTrialStage,
    },
    #[error("stage graph output {0} is absent or is not the archive stage")]
    InvalidOutput(StageId),
    #[error("stage graph has disconnected stages: {reachable} of {total} reach the archive")]
    Disconnected { reachable: usize, total: usize },
}

fn validate_stage_graph(
    stages: &[FrozenStageSpec],
    output: StageId,
) -> Result<(), StageGraphError> {
    if stages.len() != FrozenTrialStage::ALL.len() {
        return Err(StageGraphError::IncompleteStageSet);
    }
    let mut positions = BTreeMap::new();
    let mut by_kind = BTreeMap::new();
    for (position, stage) in stages.iter().enumerate() {
        if positions.insert(stage.id, position).is_some() {
            return Err(StageGraphError::DuplicateStageId(stage.id));
        }
        if by_kind.insert(stage.stage, stage.id).is_some() {
            return Err(StageGraphError::DuplicateStageKind(stage.stage));
        }
    }
    if by_kind.len() != FrozenTrialStage::ALL.len() {
        return Err(StageGraphError::IncompleteStageSet);
    }

    for (position, stage) in stages.iter().enumerate() {
        let expected = FrozenTrialStage::ALL[position];
        if stage.stage != expected {
            return Err(StageGraphError::InvalidStageOrder {
                actual: stage.stage,
                expected,
            });
        }
        let mut actual_dependency_kinds = Vec::with_capacity(stage.dependencies.len());
        for dependency in stage.dependencies.iter().copied() {
            let Some(dependency_position) = positions.get(&dependency).copied() else {
                return Err(StageGraphError::MissingOrForwardDependency {
                    stage: stage.id,
                    dependency,
                });
            };
            if dependency_position >= position {
                return Err(StageGraphError::MissingOrForwardDependency {
                    stage: stage.id,
                    dependency,
                });
            }
            actual_dependency_kinds.push(stages[dependency_position].stage);
        }
        if actual_dependency_kinds != stage.stage.required_dependencies() {
            return Err(StageGraphError::InvalidDependencies { stage: stage.id });
        }
    }
    let Some(archive) = stages.last() else {
        return Err(StageGraphError::Empty);
    };
    if archive.stage != FrozenTrialStage::Archive || archive.id != output {
        return Err(StageGraphError::InvalidOutput(output));
    }

    let by_id = stages
        .iter()
        .map(|stage| (stage.id, stage))
        .collect::<BTreeMap<_, _>>();
    let mut reachable = BTreeSet::new();
    let mut stack = vec![output];
    while let Some(id) = stack.pop() {
        if reachable.insert(id) {
            let stage = by_id.get(&id).ok_or(StageGraphError::InvalidOutput(id))?;
            stack.extend(stage.dependencies.iter().copied());
        }
    }
    if reachable.len() != stages.len() {
        return Err(StageGraphError::Disconnected {
            reachable: reachable.len(),
            total: stages.len(),
        });
    }
    Ok(())
}
