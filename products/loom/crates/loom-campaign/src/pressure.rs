use loom_search::UnitScore;
use loom_types::BlobId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_PRESSURE_POINTS: usize = 64;

const PRESSURE_DOMAIN: &[u8] = b"loom/pressure-curve-decision/v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PressurePolicy {
    minimum_gain: UnitScore,
    maximum_compute_per_gain_millionth: u64,
    maximum_level: u16,
}

impl PressurePolicy {
    pub fn new(
        minimum_gain: UnitScore,
        maximum_compute_per_gain_millionth: u64,
        maximum_level: u16,
    ) -> Result<Self, PressureError> {
        if minimum_gain == UnitScore::ZERO
            || maximum_compute_per_gain_millionth == 0
            || maximum_level < 2
        {
            return Err(PressureError::InvalidPolicy);
        }
        Ok(Self {
            minimum_gain,
            maximum_compute_per_gain_millionth,
            maximum_level,
        })
    }

    pub const fn minimum_gain(self) -> UnitScore {
        self.minimum_gain
    }

    pub const fn maximum_compute_per_gain_millionth(self) -> u64 {
        self.maximum_compute_per_gain_millionth
    }

    pub const fn maximum_level(self) -> u16 {
        self.maximum_level
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PressurePoint {
    level: u16,
    quality: Option<UnitScore>,
    hard_gate_failures: u16,
    cumulative_compute: u64,
}

impl PressurePoint {
    pub const fn new(
        level: u16,
        quality: Option<UnitScore>,
        hard_gate_failures: u16,
        cumulative_compute: u64,
    ) -> Result<Self, PressureError> {
        if level == 0 || cumulative_compute == 0 {
            return Err(PressureError::InvalidPoint);
        }
        Ok(Self {
            level,
            quality,
            hard_gate_failures,
            cumulative_compute,
        })
    }

    pub const fn level(self) -> u16 {
        self.level
    }

    pub const fn quality(self) -> Option<UnitScore> {
        self.quality
    }

    pub const fn hard_gate_failures(self) -> u16 {
        self.hard_gate_failures
    }

    pub const fn cumulative_compute(self) -> u64 {
        self.cumulative_compute
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PressureAction {
    Continue { next_level: u16 },
    Stop { reason: PressureStopReason },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureStopReason {
    MaximumLevel,
    Abstention,
    HardGateRegression,
    QualityRegression,
    InsufficientGain,
    ComputeInefficient,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PressureCurveDecision {
    policy: PressurePolicy,
    points: Vec<PressurePoint>,
    action: PressureAction,
    fingerprint: BlobId,
}

impl PressureCurveDecision {
    pub fn decide(
        policy: PressurePolicy,
        points: Vec<PressurePoint>,
    ) -> Result<Self, PressureError> {
        if points.len() < 2 || points.len() > MAX_PRESSURE_POINTS {
            return Err(PressureError::InvalidPointCount(points.len()));
        }
        for pair in points.windows(2) {
            if pair[1].level <= pair[0].level {
                return Err(PressureError::NonIncreasingLevel);
            }
            if pair[1].cumulative_compute <= pair[0].cumulative_compute {
                return Err(PressureError::NonIncreasingCompute);
            }
        }
        let previous = points[points.len() - 2];
        let current = points[points.len() - 1];
        let action = decide_action(policy, previous, current)?;
        let fingerprint = fingerprint_decision(policy, &points, action);
        Ok(Self {
            policy,
            points,
            action,
            fingerprint,
        })
    }

    pub const fn policy(&self) -> PressurePolicy {
        self.policy
    }

    pub fn points(&self) -> &[PressurePoint] {
        &self.points
    }

    pub const fn action(&self) -> PressureAction {
        self.action
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PressureError {
    #[error("pressure policy bounds are invalid")]
    InvalidPolicy,
    #[error("pressure point must have nonzero level and compute")]
    InvalidPoint,
    #[error("pressure point count {0} is outside 2..={MAX_PRESSURE_POINTS}")]
    InvalidPointCount(usize),
    #[error("pressure levels are not strictly increasing")]
    NonIncreasingLevel,
    #[error("pressure compute is not strictly increasing")]
    NonIncreasingCompute,
    #[error("pressure arithmetic overflowed")]
    ArithmeticOverflow,
}

fn decide_action(
    policy: PressurePolicy,
    previous: PressurePoint,
    current: PressurePoint,
) -> Result<PressureAction, PressureError> {
    if current.level >= policy.maximum_level {
        return Ok(PressureAction::Stop {
            reason: PressureStopReason::MaximumLevel,
        });
    }
    let (Some(previous_quality), Some(current_quality)) = (previous.quality, current.quality)
    else {
        return Ok(PressureAction::Stop {
            reason: PressureStopReason::Abstention,
        });
    };
    if current.hard_gate_failures > previous.hard_gate_failures {
        return Ok(PressureAction::Stop {
            reason: PressureStopReason::HardGateRegression,
        });
    }
    let Some(gain) = current_quality
        .millionths()
        .checked_sub(previous_quality.millionths())
    else {
        return Ok(PressureAction::Stop {
            reason: PressureStopReason::QualityRegression,
        });
    };
    if gain < policy.minimum_gain.millionths() {
        return Ok(PressureAction::Stop {
            reason: PressureStopReason::InsufficientGain,
        });
    }
    let marginal_compute = current.cumulative_compute - previous.cumulative_compute;
    let allowed_compute = policy
        .maximum_compute_per_gain_millionth
        .checked_mul(u64::from(gain))
        .ok_or(PressureError::ArithmeticOverflow)?;
    if marginal_compute > allowed_compute {
        return Ok(PressureAction::Stop {
            reason: PressureStopReason::ComputeInefficient,
        });
    }
    Ok(PressureAction::Continue {
        next_level: current.level + 1,
    })
}

fn fingerprint_decision(
    policy: PressurePolicy,
    points: &[PressurePoint],
    action: PressureAction,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PRESSURE_DOMAIN);
    digest.update(policy.minimum_gain.millionths().to_be_bytes());
    digest.update(policy.maximum_compute_per_gain_millionth.to_be_bytes());
    digest.update(policy.maximum_level.to_be_bytes());
    digest.update((points.len() as u64).to_be_bytes());
    for point in points {
        digest.update(point.level.to_be_bytes());
        match point.quality {
            Some(score) => {
                digest.update([1]);
                digest.update(score.millionths().to_be_bytes());
            }
            None => digest.update([0]),
        }
        digest.update(point.hard_gate_failures.to_be_bytes());
        digest.update(point.cumulative_compute.to_be_bytes());
    }
    match action {
        PressureAction::Continue { next_level } => {
            digest.update([0]);
            digest.update(next_level.to_be_bytes());
        }
        PressureAction::Stop { reason } => digest.update([1, stop_reason_tag(reason)]),
    }
    BlobId::from_bytes(digest.finalize().into())
}

const fn stop_reason_tag(reason: PressureStopReason) -> u8 {
    match reason {
        PressureStopReason::MaximumLevel => 0,
        PressureStopReason::Abstention => 1,
        PressureStopReason::HardGateRegression => 2,
        PressureStopReason::QualityRegression => 3,
        PressureStopReason::InsufficientGain => 4,
        PressureStopReason::ComputeInefficient => 5,
    }
}
