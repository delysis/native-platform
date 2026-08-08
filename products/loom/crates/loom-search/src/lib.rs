#![forbid(unsafe_code)]

use loom_types::ArtifactId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_BRANCH_BUDGET: u32 = 100_000;
pub const MAX_TOKEN_BUDGET: u64 = 10_000_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchBudget {
    pub max_branches: u32,
    pub max_tokens: u64,
    pub max_wall_time_ms: u64,
}

impl SearchBudget {
    pub fn validate(self) -> Result<Self, SearchError> {
        if self.max_branches == 0 || self.max_branches > MAX_BRANCH_BUDGET {
            return Err(SearchError::InvalidBranchBudget(self.max_branches));
        }
        if self.max_tokens == 0 || self.max_tokens > MAX_TOKEN_BUDGET {
            return Err(SearchError::InvalidTokenBudget(self.max_tokens));
        }
        if self.max_wall_time_ms == 0 {
            return Err(SearchError::InvalidWallTimeBudget);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchState {
    Cancelled,
    Completed,
    Paused,
    Ready,
    Running,
}

impl SearchState {
    pub fn start(&mut self) -> Result<(), SearchError> {
        transition(self, Self::Ready, Self::Running)
    }

    pub fn pause(&mut self) -> Result<(), SearchError> {
        transition(self, Self::Running, Self::Paused)
    }

    pub fn resume(&mut self) -> Result<(), SearchError> {
        transition(self, Self::Paused, Self::Running)
    }

    pub fn complete(&mut self) -> Result<(), SearchError> {
        transition(self, Self::Running, Self::Completed)
    }

    pub fn cancel(&mut self) -> Result<(), SearchError> {
        match self {
            Self::Ready | Self::Running | Self::Paused => {
                *self = Self::Cancelled;
                Ok(())
            }
            state => Err(SearchError::InvalidTransition {
                from: *state,
                to: Self::Cancelled,
            }),
        }
    }
}

fn transition(
    state: &mut SearchState,
    expected: SearchState,
    next: SearchState,
) -> Result<(), SearchError> {
    if *state != expected {
        return Err(SearchError::InvalidTransition {
            from: *state,
            to: next,
        });
    }
    *state = next;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BudgetLedger {
    pub branches_started: u32,
    pub tokens_generated: u64,
    pub elapsed_ms: u64,
}

impl BudgetLedger {
    pub const fn permits(self, budget: SearchBudget, additional_tokens: u64) -> bool {
        self.branches_started < budget.max_branches
            && match self.tokens_generated.checked_add(additional_tokens) {
                Some(tokens) => tokens <= budget.max_tokens,
                None => false,
            }
            && self.elapsed_ms < budget.max_wall_time_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidatePoint {
    pub artifact_id: ArtifactId,
    pub quality: f32,
    pub novelty: f32,
}

pub fn pareto_frontier(candidates: &[CandidatePoint]) -> Vec<CandidatePoint> {
    candidates
        .iter()
        .copied()
        .filter(|candidate| {
            !candidates.iter().any(|other| {
                (other.quality >= candidate.quality && other.novelty >= candidate.novelty)
                    && (other.quality > candidate.quality || other.novelty > candidate.novelty)
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SearchError {
    #[error("branch budget {0} is outside the supported range")]
    InvalidBranchBudget(u32),
    #[error("token budget {0} is outside the supported range")]
    InvalidTokenBudget(u64),
    #[error("wall-time budget must be positive")]
    InvalidWallTimeBudget,
    #[error("cannot transition search from {from:?} to {to:?}")]
    InvalidTransition { from: SearchState, to: SearchState },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_refuses_invalid_resume() {
        let mut state = SearchState::Ready;
        assert!(state.resume().is_err());
        state.start().expect("start");
        state.pause().expect("pause");
        state.resume().expect("resume");
        state.complete().expect("complete");
    }

    #[test]
    fn frontier_keeps_quality_and_novelty_extremes() {
        let quality = CandidatePoint {
            artifact_id: ArtifactId::new(),
            quality: 0.9,
            novelty: 0.2,
        };
        let novelty = CandidatePoint {
            artifact_id: ArtifactId::new(),
            quality: 0.5,
            novelty: 0.9,
        };
        let dominated = CandidatePoint {
            artifact_id: ArtifactId::new(),
            quality: 0.4,
            novelty: 0.1,
        };
        let frontier = pareto_frontier(&[quality, novelty, dominated]);
        assert_eq!(frontier, vec![quality, novelty]);
    }
}
