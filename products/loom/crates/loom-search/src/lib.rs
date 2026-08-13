#![forbid(unsafe_code)]

mod budget;
mod candidates;
mod pairwise;
mod rubric;
mod score;
mod selection;
mod validation;

pub use budget::*;
pub use candidates::*;
pub use pairwise::*;
pub use rubric::*;
pub use score::*;
pub use selection::*;
pub use validation::*;

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    pub fn start(&mut self) -> Result<(), SearchStateError> {
        transition(self, Self::Ready, Self::Running)
    }

    pub fn pause(&mut self) -> Result<(), SearchStateError> {
        transition(self, Self::Running, Self::Paused)
    }

    pub fn resume(&mut self) -> Result<(), SearchStateError> {
        transition(self, Self::Paused, Self::Running)
    }

    pub fn complete(&mut self) -> Result<(), SearchStateError> {
        transition(self, Self::Running, Self::Completed)
    }

    pub fn cancel(&mut self) -> Result<(), SearchStateError> {
        match self {
            Self::Ready | Self::Running | Self::Paused => {
                *self = Self::Cancelled;
                Ok(())
            }
            state => Err(SearchStateError::InvalidTransition {
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
) -> Result<(), SearchStateError> {
    if *state != expected {
        return Err(SearchStateError::InvalidTransition {
            from: *state,
            to: next,
        });
    }
    *state = next;
    Ok(())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SearchStateError {
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
}
