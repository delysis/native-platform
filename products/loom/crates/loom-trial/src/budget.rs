use loom_research_types::{MAX_CAMPAIGN_EVALUATIONS, MAX_CAMPAIGN_TOKEN_BUDGET};
use loom_types::BlobId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_TRIAL_WALL_TIME_MS: u64 = 31 * 24 * 60 * 60 * 1_000;

const BUDGET_FINGERPRINT_DOMAIN: &[u8] = b"loom/frozen-trial-budget/v1\0";

/// Immutable ceilings for one frozen trial.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TrialBudgetLimits {
    writer_tokens: u64,
    controller_tokens: u64,
    evaluations: u32,
    wall_time_ms: u64,
    fingerprint: BlobId,
}

impl TrialBudgetLimits {
    pub fn new(
        writer_tokens: u64,
        controller_tokens: u64,
        evaluations: u32,
        wall_time_ms: u64,
    ) -> Result<Self, BudgetError> {
        if writer_tokens == 0 || writer_tokens > MAX_CAMPAIGN_TOKEN_BUDGET {
            return Err(BudgetError::InvalidLimit {
                resource: BudgetResource::WriterTokens,
                value: writer_tokens,
            });
        }
        if controller_tokens > MAX_CAMPAIGN_TOKEN_BUDGET {
            return Err(BudgetError::InvalidLimit {
                resource: BudgetResource::ControllerTokens,
                value: controller_tokens,
            });
        }
        if evaluations == 0 || evaluations > MAX_CAMPAIGN_EVALUATIONS {
            return Err(BudgetError::InvalidLimit {
                resource: BudgetResource::Evaluations,
                value: u64::from(evaluations),
            });
        }
        if wall_time_ms == 0 || wall_time_ms > MAX_TRIAL_WALL_TIME_MS {
            return Err(BudgetError::InvalidLimit {
                resource: BudgetResource::WallTimeMs,
                value: wall_time_ms,
            });
        }

        let fingerprint =
            fingerprint_limits(writer_tokens, controller_tokens, evaluations, wall_time_ms);
        Ok(Self {
            writer_tokens,
            controller_tokens,
            evaluations,
            wall_time_ms,
            fingerprint,
        })
    }

    pub const fn writer_tokens(self) -> u64 {
        self.writer_tokens
    }

    pub const fn controller_tokens(self) -> u64 {
        self.controller_tokens
    }

    pub const fn evaluations(self) -> u32 {
        self.evaluations
    }

    pub const fn wall_time_ms(self) -> u64 {
        self.wall_time_ms
    }

    pub const fn fingerprint(self) -> BlobId {
        self.fingerprint
    }

    pub(crate) fn verify_fingerprint(self) -> Result<(), BudgetError> {
        let expected = fingerprint_limits(
            self.writer_tokens,
            self.controller_tokens,
            self.evaluations,
            self.wall_time_ms,
        );
        if self.fingerprint != expected {
            return Err(BudgetError::FingerprintMismatch);
        }
        Ok(())
    }
}

/// A reservation or reconciled charge in all trial-budget dimensions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetAmount {
    writer_tokens: u64,
    controller_tokens: u64,
    evaluations: u32,
    wall_time_ms: u64,
}

impl BudgetAmount {
    pub fn new(
        writer_tokens: u64,
        controller_tokens: u64,
        evaluations: u32,
        wall_time_ms: u64,
    ) -> Result<Self, BudgetError> {
        let value = Self {
            writer_tokens,
            controller_tokens,
            evaluations,
            wall_time_ms,
        };
        value.verify_global_bounds()?;
        Ok(value)
    }

    pub const fn writer_tokens(self) -> u64 {
        self.writer_tokens
    }

    pub const fn controller_tokens(self) -> u64 {
        self.controller_tokens
    }

    pub const fn evaluations(self) -> u32 {
        self.evaluations
    }

    pub const fn wall_time_ms(self) -> u64 {
        self.wall_time_ms
    }

    pub const fn is_zero(self) -> bool {
        self.writer_tokens == 0
            && self.controller_tokens == 0
            && self.evaluations == 0
            && self.wall_time_ms == 0
    }

    pub(crate) fn verify_global_bounds(self) -> Result<(), BudgetError> {
        if self.writer_tokens > MAX_CAMPAIGN_TOKEN_BUDGET {
            return Err(BudgetError::InvalidAmount {
                resource: BudgetResource::WriterTokens,
                value: self.writer_tokens,
            });
        }
        if self.controller_tokens > MAX_CAMPAIGN_TOKEN_BUDGET {
            return Err(BudgetError::InvalidAmount {
                resource: BudgetResource::ControllerTokens,
                value: self.controller_tokens,
            });
        }
        if self.evaluations > MAX_CAMPAIGN_EVALUATIONS {
            return Err(BudgetError::InvalidAmount {
                resource: BudgetResource::Evaluations,
                value: u64::from(self.evaluations),
            });
        }
        if self.wall_time_ms > MAX_TRIAL_WALL_TIME_MS {
            return Err(BudgetError::InvalidAmount {
                resource: BudgetResource::WallTimeMs,
                value: self.wall_time_ms,
            });
        }
        Ok(())
    }

    pub(crate) fn checked_add(self, other: Self) -> Result<Self, BudgetError> {
        let writer_tokens = self
            .writer_tokens
            .checked_add(other.writer_tokens)
            .ok_or(BudgetError::CounterOverflow(BudgetResource::WriterTokens))?;
        let controller_tokens = self
            .controller_tokens
            .checked_add(other.controller_tokens)
            .ok_or(BudgetError::CounterOverflow(
                BudgetResource::ControllerTokens,
            ))?;
        let evaluations = self
            .evaluations
            .checked_add(other.evaluations)
            .ok_or(BudgetError::CounterOverflow(BudgetResource::Evaluations))?;
        let wall_time_ms = self
            .wall_time_ms
            .checked_add(other.wall_time_ms)
            .ok_or(BudgetError::CounterOverflow(BudgetResource::WallTimeMs))?;
        Self::new(writer_tokens, controller_tokens, evaluations, wall_time_ms)
    }

    pub(crate) fn checked_sub(self, other: Self) -> Result<Self, BudgetError> {
        Ok(Self {
            writer_tokens: self.writer_tokens.checked_sub(other.writer_tokens).ok_or(
                BudgetError::ReservationUnderflow(BudgetResource::WriterTokens),
            )?,
            controller_tokens: self
                .controller_tokens
                .checked_sub(other.controller_tokens)
                .ok_or(BudgetError::ReservationUnderflow(
                    BudgetResource::ControllerTokens,
                ))?,
            evaluations: self.evaluations.checked_sub(other.evaluations).ok_or(
                BudgetError::ReservationUnderflow(BudgetResource::Evaluations),
            )?,
            wall_time_ms: self.wall_time_ms.checked_sub(other.wall_time_ms).ok_or(
                BudgetError::ReservationUnderflow(BudgetResource::WallTimeMs),
            )?,
        })
    }

    pub(crate) const fn fits_within(self, other: Self) -> bool {
        self.writer_tokens <= other.writer_tokens
            && self.controller_tokens <= other.controller_tokens
            && self.evaluations <= other.evaluations
            && self.wall_time_ms <= other.wall_time_ms
    }

    pub(crate) const fn fits_limits(self, limits: TrialBudgetLimits) -> bool {
        self.writer_tokens <= limits.writer_tokens
            && self.controller_tokens <= limits.controller_tokens
            && self.evaluations <= limits.evaluations
            && self.wall_time_ms <= limits.wall_time_ms
    }

    pub(crate) fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.writer_tokens.to_be_bytes());
        digest.update(self.controller_tokens.to_be_bytes());
        digest.update(self.evaluations.to_be_bytes());
        digest.update(self.wall_time_ms.to_be_bytes());
    }
}

/// Derived, replayable accounting state. Reservations and charges are never
/// edited in place; journal events rebuild these totals deterministically.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BudgetLedger {
    reserved: BudgetAmount,
    charged: BudgetAmount,
}

impl BudgetLedger {
    pub const fn reserved(self) -> BudgetAmount {
        self.reserved
    }

    pub const fn charged(self) -> BudgetAmount {
        self.charged
    }

    pub const fn committed(self) -> BudgetAmount {
        BudgetAmount {
            writer_tokens: self.reserved.writer_tokens + self.charged.writer_tokens,
            controller_tokens: self.reserved.controller_tokens + self.charged.controller_tokens,
            evaluations: self.reserved.evaluations + self.charged.evaluations,
            wall_time_ms: self.reserved.wall_time_ms + self.charged.wall_time_ms,
        }
    }

    pub(crate) fn reserve(
        &mut self,
        limits: TrialBudgetLimits,
        amount: BudgetAmount,
    ) -> Result<(), BudgetError> {
        amount.verify_global_bounds()?;
        let next_reserved = self.reserved.checked_add(amount)?;
        let committed = self.charged.checked_add(next_reserved)?;
        if !committed.fits_limits(limits) {
            return Err(BudgetError::Exceeded);
        }
        self.reserved = next_reserved;
        Ok(())
    }

    pub(crate) fn reconcile(
        &mut self,
        limits: TrialBudgetLimits,
        reservation: BudgetAmount,
        actual: BudgetAmount,
    ) -> Result<(), BudgetError> {
        actual.verify_global_bounds()?;
        if !actual.fits_within(reservation) {
            return Err(BudgetError::ChargeExceedsReservation);
        }
        let next_reserved = self.reserved.checked_sub(reservation)?;
        let next_charged = self.charged.checked_add(actual)?;
        let committed = next_charged.checked_add(next_reserved)?;
        if !committed.fits_limits(limits) {
            return Err(BudgetError::Exceeded);
        }
        self.reserved = next_reserved;
        self.charged = next_charged;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetResource {
    WriterTokens,
    ControllerTokens,
    Evaluations,
    WallTimeMs,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BudgetError {
    #[error("invalid trial limit for {resource:?}: {value}")]
    InvalidLimit {
        resource: BudgetResource,
        value: u64,
    },
    #[error("invalid budget amount for {resource:?}: {value}")]
    InvalidAmount {
        resource: BudgetResource,
        value: u64,
    },
    #[error("budget counter overflow for {0:?}")]
    CounterOverflow(BudgetResource),
    #[error("budget reservation underflow for {0:?}")]
    ReservationUnderflow(BudgetResource),
    #[error("budget reservation would exceed the frozen trial ceiling")]
    Exceeded,
    #[error("actual charge exceeds its prior reservation")]
    ChargeExceedsReservation,
    #[error("frozen budget fingerprint mismatch")]
    FingerprintMismatch,
}

fn fingerprint_limits(
    writer_tokens: u64,
    controller_tokens: u64,
    evaluations: u32,
    wall_time_ms: u64,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(BUDGET_FINGERPRINT_DOMAIN);
    digest.update(writer_tokens.to_be_bytes());
    digest.update(controller_tokens.to_be_bytes());
    digest.update(evaluations.to_be_bytes());
    digest.update(wall_time_ms.to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_is_atomic_across_dimensions() {
        let limits = TrialBudgetLimits::new(10, 10, 2, 100).expect("limits");
        let mut ledger = BudgetLedger::default();
        ledger
            .reserve(limits, BudgetAmount::new(5, 0, 0, 10).expect("reservation"))
            .expect("first reservation");
        let before = ledger;
        assert_eq!(
            ledger.reserve(
                limits,
                BudgetAmount::new(0, 11, 0, 0).expect("bounded amount")
            ),
            Err(BudgetError::Exceeded)
        );
        assert_eq!(ledger, before);
    }

    #[test]
    fn controller_free_trial_limits_are_valid() {
        let limits = TrialBudgetLimits::new(10, 0, 2, 100).expect("controller-free limits");
        assert_eq!(limits.controller_tokens(), 0);
    }

    #[test]
    fn property_reconcile_never_spends_more_than_reserved() {
        let limits = TrialBudgetLimits::new(100, 100, 100, 100).expect("limits");
        for reserved in 0..=32_u64 {
            for actual in 0..=32_u64 {
                let reservation = BudgetAmount::new(reserved, 0, 0, 0).expect("reservation");
                let charge = BudgetAmount::new(actual, 0, 0, 0).expect("charge");
                let mut ledger = BudgetLedger::default();
                ledger.reserve(limits, reservation).expect("reserve");
                let result = ledger.reconcile(limits, reservation, charge);
                assert_eq!(result.is_ok(), actual <= reserved);
                if result.is_ok() {
                    assert_eq!(ledger.reserved(), BudgetAmount::default());
                    assert_eq!(ledger.charged(), charge);
                }
            }
        }
    }
}
