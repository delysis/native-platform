use loom_research_types::{MAX_CAMPAIGN_EVALUATIONS, MAX_CAMPAIGN_TOKEN_BUDGET};
use loom_trial::TrialBudgetLimits;
use loom_types::BlobId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// A campaign may reserve at most one year of aggregate measured worker time.
pub const MAX_CAMPAIGN_WALL_TIME_MS: u64 = 366 * 24 * 60 * 60 * 1_000;

const BUDGET_LIMIT_DOMAIN: &[u8] = b"loom/campaign-budget-limits/v1\0";

/// Aggregate immutable ceilings for one campaign.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CampaignBudgetLimits {
    writer_tokens: u64,
    controller_tokens: u64,
    evaluations: u32,
    wall_time_ms: u64,
    fingerprint: BlobId,
}

impl CampaignBudgetLimits {
    pub fn new(
        writer_tokens: u64,
        controller_tokens: u64,
        evaluations: u32,
        wall_time_ms: u64,
    ) -> Result<Self, CampaignBudgetError> {
        validate_limit(
            CampaignBudgetResource::WriterTokens,
            writer_tokens,
            MAX_CAMPAIGN_TOKEN_BUDGET,
        )?;
        validate_optional_limit(
            CampaignBudgetResource::ControllerTokens,
            controller_tokens,
            MAX_CAMPAIGN_TOKEN_BUDGET,
        )?;
        validate_limit(
            CampaignBudgetResource::Evaluations,
            u64::from(evaluations),
            u64::from(MAX_CAMPAIGN_EVALUATIONS),
        )?;
        validate_limit(
            CampaignBudgetResource::WallTimeMs,
            wall_time_ms,
            MAX_CAMPAIGN_WALL_TIME_MS,
        )?;
        Ok(Self {
            writer_tokens,
            controller_tokens,
            evaluations,
            wall_time_ms,
            fingerprint: fingerprint_limits(
                writer_tokens,
                controller_tokens,
                evaluations,
                wall_time_ms,
            ),
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

    pub(crate) fn verify(self) -> Result<(), CampaignBudgetError> {
        if fingerprint_limits(
            self.writer_tokens,
            self.controller_tokens,
            self.evaluations,
            self.wall_time_ms,
        ) != self.fingerprint
        {
            return Err(CampaignBudgetError::FingerprintMismatch);
        }
        Ok(())
    }
}

/// Reserved or charged resources across all campaign dimensions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignBudgetAmount {
    writer_tokens: u64,
    controller_tokens: u64,
    evaluations: u32,
    wall_time_ms: u64,
}

impl CampaignBudgetAmount {
    pub fn new(
        writer_tokens: u64,
        controller_tokens: u64,
        evaluations: u32,
        wall_time_ms: u64,
    ) -> Result<Self, CampaignBudgetError> {
        let value = Self {
            writer_tokens,
            controller_tokens,
            evaluations,
            wall_time_ms,
        };
        value.verify_bounds()?;
        Ok(value)
    }

    pub fn from_trial_limits(limits: TrialBudgetLimits) -> Result<Self, CampaignBudgetError> {
        Self::new(
            limits.writer_tokens(),
            limits.controller_tokens(),
            limits.evaluations(),
            limits.wall_time_ms(),
        )
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

    pub(crate) fn verify_bounds(self) -> Result<(), CampaignBudgetError> {
        validate_amount(
            CampaignBudgetResource::WriterTokens,
            self.writer_tokens,
            MAX_CAMPAIGN_TOKEN_BUDGET,
        )?;
        validate_amount(
            CampaignBudgetResource::ControllerTokens,
            self.controller_tokens,
            MAX_CAMPAIGN_TOKEN_BUDGET,
        )?;
        validate_amount(
            CampaignBudgetResource::Evaluations,
            u64::from(self.evaluations),
            u64::from(MAX_CAMPAIGN_EVALUATIONS),
        )?;
        validate_amount(
            CampaignBudgetResource::WallTimeMs,
            self.wall_time_ms,
            MAX_CAMPAIGN_WALL_TIME_MS,
        )
    }

    pub(crate) fn checked_add(self, other: Self) -> Result<Self, CampaignBudgetError> {
        Self::new(
            self.writer_tokens.checked_add(other.writer_tokens).ok_or(
                CampaignBudgetError::CounterOverflow(CampaignBudgetResource::WriterTokens),
            )?,
            self.controller_tokens
                .checked_add(other.controller_tokens)
                .ok_or(CampaignBudgetError::CounterOverflow(
                    CampaignBudgetResource::ControllerTokens,
                ))?,
            self.evaluations.checked_add(other.evaluations).ok_or(
                CampaignBudgetError::CounterOverflow(CampaignBudgetResource::Evaluations),
            )?,
            self.wall_time_ms.checked_add(other.wall_time_ms).ok_or(
                CampaignBudgetError::CounterOverflow(CampaignBudgetResource::WallTimeMs),
            )?,
        )
    }

    pub(crate) fn checked_sub(self, other: Self) -> Result<Self, CampaignBudgetError> {
        Ok(Self {
            writer_tokens: self.writer_tokens.checked_sub(other.writer_tokens).ok_or(
                CampaignBudgetError::ReservationUnderflow(CampaignBudgetResource::WriterTokens),
            )?,
            controller_tokens: self
                .controller_tokens
                .checked_sub(other.controller_tokens)
                .ok_or(CampaignBudgetError::ReservationUnderflow(
                    CampaignBudgetResource::ControllerTokens,
                ))?,
            evaluations: self.evaluations.checked_sub(other.evaluations).ok_or(
                CampaignBudgetError::ReservationUnderflow(CampaignBudgetResource::Evaluations),
            )?,
            wall_time_ms: self.wall_time_ms.checked_sub(other.wall_time_ms).ok_or(
                CampaignBudgetError::ReservationUnderflow(CampaignBudgetResource::WallTimeMs),
            )?,
        })
    }

    pub(crate) const fn fits(self, other: Self) -> bool {
        self.writer_tokens <= other.writer_tokens
            && self.controller_tokens <= other.controller_tokens
            && self.evaluations <= other.evaluations
            && self.wall_time_ms <= other.wall_time_ms
    }

    pub(crate) const fn fits_limits(self, limits: CampaignBudgetLimits) -> bool {
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

/// Replay-derived campaign accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CampaignBudgetLedger {
    reserved: CampaignBudgetAmount,
    charged: CampaignBudgetAmount,
}

impl CampaignBudgetLedger {
    pub const fn reserved(self) -> CampaignBudgetAmount {
        self.reserved
    }

    pub const fn charged(self) -> CampaignBudgetAmount {
        self.charged
    }

    pub fn committed(self) -> Result<CampaignBudgetAmount, CampaignBudgetError> {
        self.reserved.checked_add(self.charged)
    }

    pub(crate) fn reserve(
        &mut self,
        limits: CampaignBudgetLimits,
        amount: CampaignBudgetAmount,
    ) -> Result<(), CampaignBudgetError> {
        amount.verify_bounds()?;
        let next_reserved = self.reserved.checked_add(amount)?;
        if !next_reserved.checked_add(self.charged)?.fits_limits(limits) {
            return Err(CampaignBudgetError::Exceeded);
        }
        self.reserved = next_reserved;
        Ok(())
    }

    pub(crate) fn reconcile(
        &mut self,
        limits: CampaignBudgetLimits,
        reservation: CampaignBudgetAmount,
        actual: CampaignBudgetAmount,
    ) -> Result<(), CampaignBudgetError> {
        actual.verify_bounds()?;
        if !actual.fits(reservation) {
            return Err(CampaignBudgetError::ChargeExceedsReservation);
        }
        let next_reserved = self.reserved.checked_sub(reservation)?;
        let next_charged = self.charged.checked_add(actual)?;
        if !next_reserved.checked_add(next_charged)?.fits_limits(limits) {
            return Err(CampaignBudgetError::Exceeded);
        }
        self.reserved = next_reserved;
        self.charged = next_charged;
        Ok(())
    }

    pub(crate) fn release(
        &mut self,
        reservation: CampaignBudgetAmount,
    ) -> Result<(), CampaignBudgetError> {
        self.reserved = self.reserved.checked_sub(reservation)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CampaignBudgetResource {
    WriterTokens,
    ControllerTokens,
    Evaluations,
    WallTimeMs,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CampaignBudgetError {
    #[error("campaign {resource:?} limit {value} is outside 1..={maximum}")]
    InvalidLimit {
        resource: CampaignBudgetResource,
        value: u64,
        maximum: u64,
    },
    #[error("campaign {resource:?} amount {value} exceeds {maximum}")]
    InvalidAmount {
        resource: CampaignBudgetResource,
        value: u64,
        maximum: u64,
    },
    #[error("campaign budget fingerprint mismatch")]
    FingerprintMismatch,
    #[error("campaign budget would be exceeded")]
    Exceeded,
    #[error("campaign charge exceeds its reservation")]
    ChargeExceedsReservation,
    #[error("campaign {0:?} counter overflowed")]
    CounterOverflow(CampaignBudgetResource),
    #[error("campaign {0:?} reservation underflowed")]
    ReservationUnderflow(CampaignBudgetResource),
}

fn validate_limit(
    resource: CampaignBudgetResource,
    value: u64,
    maximum: u64,
) -> Result<(), CampaignBudgetError> {
    if value == 0 || value > maximum {
        return Err(CampaignBudgetError::InvalidLimit {
            resource,
            value,
            maximum,
        });
    }
    Ok(())
}

fn validate_optional_limit(
    resource: CampaignBudgetResource,
    value: u64,
    maximum: u64,
) -> Result<(), CampaignBudgetError> {
    if value > maximum {
        return Err(CampaignBudgetError::InvalidLimit {
            resource,
            value,
            maximum,
        });
    }
    Ok(())
}

fn validate_amount(
    resource: CampaignBudgetResource,
    value: u64,
    maximum: u64,
) -> Result<(), CampaignBudgetError> {
    if value > maximum {
        return Err(CampaignBudgetError::InvalidAmount {
            resource,
            value,
            maximum,
        });
    }
    Ok(())
}

fn fingerprint_limits(
    writer_tokens: u64,
    controller_tokens: u64,
    evaluations: u32,
    wall_time_ms: u64,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(BUDGET_LIMIT_DOMAIN);
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
    fn controller_free_campaign_limits_are_valid() {
        let limits = CampaignBudgetLimits::new(10, 0, 2, 100).expect("controller-free limits");
        assert_eq!(limits.controller_tokens(), 0);
    }
}
