use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub const MAX_BRANCH_BUDGET: u32 = 100_000;
pub const MAX_TOKEN_BUDGET: u64 = 10_000_000_000;
pub const MAX_WALL_TIME_MS: u64 = 31 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SearchBudget {
    #[serde(rename = "max_branches")]
    branches: u32,
    #[serde(rename = "max_tokens")]
    tokens: u64,
    #[serde(rename = "max_wall_time_ms")]
    wall_time_ms: u64,
}

impl SearchBudget {
    pub const fn new(
        max_branches: u32,
        max_tokens: u64,
        max_wall_time_ms: u64,
    ) -> Result<Self, BudgetError> {
        if max_branches == 0 || max_branches > MAX_BRANCH_BUDGET {
            return Err(BudgetError::InvalidBranchLimit(max_branches));
        }
        if max_tokens == 0 || max_tokens > MAX_TOKEN_BUDGET {
            return Err(BudgetError::InvalidTokenLimit(max_tokens));
        }
        if max_wall_time_ms == 0 || max_wall_time_ms > MAX_WALL_TIME_MS {
            return Err(BudgetError::InvalidWallTimeLimit(max_wall_time_ms));
        }
        Ok(Self {
            branches: max_branches,
            tokens: max_tokens,
            wall_time_ms: max_wall_time_ms,
        })
    }

    pub const fn max_branches(self) -> u32 {
        self.branches
    }

    pub const fn max_tokens(self) -> u64 {
        self.tokens
    }

    pub const fn max_wall_time_ms(self) -> u64 {
        self.wall_time_ms
    }
}

impl<'de> Deserialize<'de> for SearchBudget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireBudget {
            #[serde(rename = "max_branches")]
            branches: u32,
            #[serde(rename = "max_tokens")]
            tokens: u64,
            #[serde(rename = "max_wall_time_ms")]
            wall_time_ms: u64,
        }

        let wire = WireBudget::deserialize(deserializer)?;
        Self::new(wire.branches, wire.tokens, wire.wall_time_ms).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BudgetLedger {
    branches_started: u32,
    tokens_generated: u64,
    elapsed_ms: u64,
}

impl BudgetLedger {
    pub const fn from_usage(
        branches_started: u32,
        tokens_generated: u64,
        elapsed_ms: u64,
    ) -> Result<Self, BudgetError> {
        if branches_started > MAX_BRANCH_BUDGET {
            return Err(BudgetError::InvalidLedgerUsage {
                resource: BudgetResource::Branches,
                value: branches_started as u64,
                global_limit: MAX_BRANCH_BUDGET as u64,
            });
        }
        if tokens_generated > MAX_TOKEN_BUDGET {
            return Err(BudgetError::InvalidLedgerUsage {
                resource: BudgetResource::Tokens,
                value: tokens_generated,
                global_limit: MAX_TOKEN_BUDGET,
            });
        }
        if elapsed_ms > MAX_WALL_TIME_MS {
            return Err(BudgetError::InvalidLedgerUsage {
                resource: BudgetResource::WallTime,
                value: elapsed_ms,
                global_limit: MAX_WALL_TIME_MS,
            });
        }
        Ok(Self {
            branches_started,
            tokens_generated,
            elapsed_ms,
        })
    }

    pub const fn branches_started(self) -> u32 {
        self.branches_started
    }

    pub const fn tokens_generated(self) -> u64 {
        self.tokens_generated
    }

    pub const fn elapsed_ms(self) -> u64 {
        self.elapsed_ms
    }

    pub fn remaining(self, budget: SearchBudget) -> Result<BudgetRemaining, BudgetError> {
        check_limit(
            BudgetResource::Branches,
            u64::from(self.branches_started),
            u64::from(budget.branches),
        )?;
        check_limit(BudgetResource::Tokens, self.tokens_generated, budget.tokens)?;
        check_limit(
            BudgetResource::WallTime,
            self.elapsed_ms,
            budget.wall_time_ms,
        )?;
        Ok(BudgetRemaining {
            branches: budget.branches - self.branches_started,
            tokens: budget.tokens - self.tokens_generated,
            wall_time_ms: budget.wall_time_ms - self.elapsed_ms,
        })
    }

    /// Returns a new ledger only when the entire charge fits. The original is
    /// unchanged on both overflow and budget exhaustion.
    pub fn try_charge(
        self,
        budget: SearchBudget,
        charge: BudgetCharge,
    ) -> Result<Self, BudgetError> {
        if charge == BudgetCharge::default() {
            return Err(BudgetError::EmptyCharge);
        }

        let branches_started = self
            .branches_started
            .checked_add(charge.branches_started)
            .ok_or(BudgetError::CounterOverflow(BudgetResource::Branches))?;
        let tokens_generated = self
            .tokens_generated
            .checked_add(charge.tokens_generated)
            .ok_or(BudgetError::CounterOverflow(BudgetResource::Tokens))?;
        let elapsed_ms = self
            .elapsed_ms
            .checked_add(charge.elapsed_ms)
            .ok_or(BudgetError::CounterOverflow(BudgetResource::WallTime))?;

        check_limit(
            BudgetResource::Branches,
            u64::from(branches_started),
            u64::from(budget.branches),
        )?;
        check_limit(BudgetResource::Tokens, tokens_generated, budget.tokens)?;
        check_limit(BudgetResource::WallTime, elapsed_ms, budget.wall_time_ms)?;

        Ok(Self {
            branches_started,
            tokens_generated,
            elapsed_ms,
        })
    }
}

impl<'de> Deserialize<'de> for BudgetLedger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireLedger {
            branches_started: u32,
            tokens_generated: u64,
            elapsed_ms: u64,
        }

        let wire = WireLedger::deserialize(deserializer)?;
        Self::from_usage(
            wire.branches_started,
            wire.tokens_generated,
            wire.elapsed_ms,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BudgetRemaining {
    pub branches: u32,
    pub tokens: u64,
    pub wall_time_ms: u64,
}

fn check_limit(resource: BudgetResource, attempted: u64, limit: u64) -> Result<(), BudgetError> {
    if attempted > limit {
        return Err(BudgetError::Exceeded {
            resource,
            limit,
            attempted,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BudgetCharge {
    pub branches_started: u32,
    pub tokens_generated: u64,
    pub elapsed_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetResource {
    Branches,
    Tokens,
    WallTime,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BudgetError {
    #[error("branch limit {0} is outside 1..={MAX_BRANCH_BUDGET}")]
    InvalidBranchLimit(u32),
    #[error("token limit {0} is outside 1..={MAX_TOKEN_BUDGET}")]
    InvalidTokenLimit(u64),
    #[error("wall-time limit {0} is outside 1..={MAX_WALL_TIME_MS} ms")]
    InvalidWallTimeLimit(u64),
    #[error("a budget charge must account for at least one resource")]
    EmptyCharge,
    #[error("restored {resource:?} usage {value} exceeds global limit {global_limit}")]
    InvalidLedgerUsage {
        resource: BudgetResource,
        value: u64,
        global_limit: u64,
    },
    #[error("{0:?} counter overflowed")]
    CounterOverflow(BudgetResource),
    #[error("{resource:?} budget exceeded: attempted {attempted}, limit {limit}")]
    Exceeded {
        resource: BudgetResource,
        limit: u64,
        attempted: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> SearchBudget {
        SearchBudget::new(2, 100, 1_000).expect("valid budget")
    }

    #[test]
    fn charge_is_atomic_when_a_dimension_exceeds_budget() {
        let ledger = BudgetLedger::default();
        let error = ledger
            .try_charge(
                budget(),
                BudgetCharge {
                    branches_started: 1,
                    tokens_generated: 101,
                    elapsed_ms: 1,
                },
            )
            .expect_err("token budget must fail");
        assert_eq!(
            error,
            BudgetError::Exceeded {
                resource: BudgetResource::Tokens,
                limit: 100,
                attempted: 101,
            }
        );
        assert_eq!(ledger, BudgetLedger::default());
    }

    #[test]
    fn rejects_overflow_instead_of_wrapping() {
        let ledger = BudgetLedger {
            branches_started: 0,
            tokens_generated: u64::MAX,
            elapsed_ms: 0,
        };
        assert_eq!(
            ledger.try_charge(
                budget(),
                BudgetCharge {
                    branches_started: 0,
                    tokens_generated: 1,
                    elapsed_ms: 0,
                },
            ),
            Err(BudgetError::CounterOverflow(BudgetResource::Tokens))
        );
    }

    #[test]
    fn deserialize_rejects_an_unbounded_budget() {
        let json = format!(
            r#"{{"max_branches":1,"max_tokens":1,"max_wall_time_ms":{}}}"#,
            MAX_WALL_TIME_MS + 1
        );
        assert!(serde_json::from_str::<SearchBudget>(&json).is_err());
    }

    #[test]
    fn ledger_round_trip_is_bounded_and_resume_safe() {
        let ledger = BudgetLedger::from_usage(1, 20, 30).expect("ledger");
        let encoded = serde_json::to_string(&ledger).expect("serialize");
        let decoded: BudgetLedger = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, ledger);
        assert_eq!(
            decoded.remaining(budget()).expect("remaining"),
            BudgetRemaining {
                branches: 1,
                tokens: 80,
                wall_time_ms: 970,
            }
        );
    }

    #[test]
    fn restored_ledger_rejects_global_and_project_budget_overruns() {
        let json = format!(
            r#"{{"branches_started":{},"tokens_generated":0,"elapsed_ms":0}}"#,
            MAX_BRANCH_BUDGET + 1
        );
        assert!(serde_json::from_str::<BudgetLedger>(&json).is_err());

        let ledger = BudgetLedger::from_usage(2, 0, 0).expect("ledger");
        let tighter = SearchBudget::new(1, 1, 1).expect("budget");
        assert!(matches!(
            ledger.remaining(tighter),
            Err(BudgetError::Exceeded {
                resource: BudgetResource::Branches,
                ..
            })
        ));
    }
}
