use serde::{Deserialize, Serialize};
use thiserror::Error;

const MINIMUM_RUNTIME_RESERVE_BYTES: u64 = 384 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelFitInput {
    pub model_file_bytes: u64,
    pub context_tokens: u32,
    /// Measured or metadata-derived KV bytes per token. Keep absent when the
    /// model's KV layout is not known exactly enough for fit accounting.
    pub kv_bytes_per_token: Option<u64>,
    pub available_ram_bytes: Option<u64>,
    /// Total bytes the selected offload plan says will reside in VRAM.
    pub planned_vram_bytes: Option<u64>,
    pub available_vram_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteEstimateBasis {
    ExactInput,
    ExactKvBytesPerTokenTimesContext,
    ConservativeFilePlusRuntimeReserve,
    ConservativeFileReservePlusExactKv,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "knowledge", rename_all = "snake_case")]
pub enum ByteEstimate {
    Known {
        bytes: u64,
        basis: ByteEstimateBasis,
    },
    Unknown {
        reason: String,
        minimum_bytes: Option<u64>,
    },
}

impl ByteEstimate {
    pub const fn known_bytes(&self) -> Option<u64> {
        match self {
            Self::Known { bytes, .. } => Some(*bytes),
            Self::Unknown { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum FitVerdict {
    Fits {
        required_bytes: u64,
        available_bytes: u64,
        headroom_bytes: u64,
    },
    DoesNotFit {
        required_bytes: u64,
        available_bytes: u64,
        shortfall_bytes: u64,
    },
    Unknown {
        reason: String,
        minimum_required_bytes: Option<u64>,
        available_bytes: Option<u64>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelFitEstimate {
    pub model_file: ByteEstimate,
    pub conservative_runtime_without_kv: ByteEstimate,
    pub kv_cache: ByteEstimate,
    pub conservative_total: ByteEstimate,
    pub ram: FitVerdict,
    pub vram: FitVerdict,
    /// Performance is a benchmark result, not a fit calculation.
    pub performance_unknown_reason: String,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FitEstimationError {
    #[error("context token count must be positive")]
    ZeroContext,
    #[error("fit estimate overflowed 64-bit byte accounting")]
    ByteOverflow,
}

pub fn estimate_model_fit(input: &ModelFitInput) -> Result<ModelFitEstimate, FitEstimationError> {
    if input.context_tokens == 0 {
        return Err(FitEstimationError::ZeroContext);
    }
    let runtime_reserve = (input.model_file_bytes / 2).max(MINIMUM_RUNTIME_RESERVE_BYTES);
    let minimum_runtime = input
        .model_file_bytes
        .checked_add(runtime_reserve)
        .ok_or(FitEstimationError::ByteOverflow)?;
    let kv_cache = input
        .kv_bytes_per_token
        .map(|bytes_per_token| {
            bytes_per_token
                .checked_mul(u64::from(input.context_tokens))
                .ok_or(FitEstimationError::ByteOverflow)
        })
        .transpose()?;
    let conservative_total = kv_cache
        .map(|kv_bytes| {
            minimum_runtime
                .checked_add(kv_bytes)
                .ok_or(FitEstimationError::ByteOverflow)
        })
        .transpose()?;

    let ram = memory_verdict(
        conservative_total,
        minimum_runtime,
        input.available_ram_bytes,
        "KV bytes per token or available RAM is unknown",
    );
    let vram = match (input.planned_vram_bytes, input.available_vram_bytes) {
        (Some(required), Some(available)) => compare_known(required, available),
        (required, available) => FitVerdict::Unknown {
            reason: "the selected offload plan or available VRAM is unknown".to_string(),
            minimum_required_bytes: required,
            available_bytes: available,
        },
    };

    Ok(ModelFitEstimate {
        model_file: ByteEstimate::Known {
            bytes: input.model_file_bytes,
            basis: ByteEstimateBasis::ExactInput,
        },
        conservative_runtime_without_kv: ByteEstimate::Known {
            bytes: minimum_runtime,
            basis: ByteEstimateBasis::ConservativeFilePlusRuntimeReserve,
        },
        kv_cache: kv_cache.map_or_else(
            || ByteEstimate::Unknown {
                reason: "KV bytes per token were not supplied from inspected model metadata"
                    .to_string(),
                minimum_bytes: None,
            },
            |bytes| ByteEstimate::Known {
                bytes,
                basis: ByteEstimateBasis::ExactKvBytesPerTokenTimesContext,
            },
        ),
        conservative_total: conservative_total.map_or_else(
            || ByteEstimate::Unknown {
                reason: "total cannot be calculated without trustworthy KV layout inputs"
                    .to_string(),
                minimum_bytes: Some(minimum_runtime),
            },
            |bytes| ByteEstimate::Known {
                bytes,
                basis: ByteEstimateBasis::ConservativeFileReservePlusExactKv,
            },
        ),
        ram,
        vram,
        performance_unknown_reason: "no benchmark was executed for this model and device"
            .to_string(),
    })
}

fn memory_verdict(
    total: Option<u64>,
    minimum: u64,
    available: Option<u64>,
    unknown_reason: &str,
) -> FitVerdict {
    let Some(available) = available else {
        return FitVerdict::Unknown {
            reason: unknown_reason.to_string(),
            minimum_required_bytes: Some(minimum),
            available_bytes: None,
        };
    };
    if available < minimum {
        return FitVerdict::DoesNotFit {
            required_bytes: minimum,
            available_bytes: available,
            shortfall_bytes: minimum - available,
        };
    }
    match total {
        Some(required) => compare_known(required, available),
        None => FitVerdict::Unknown {
            reason: unknown_reason.to_string(),
            minimum_required_bytes: Some(minimum),
            available_bytes: Some(available),
        },
    }
}

const fn compare_known(required: u64, available: u64) -> FitVerdict {
    if required <= available {
        FitVerdict::Fits {
            required_bytes: required,
            available_bytes: available,
            headroom_bytes: available - required,
        }
    } else {
        FitVerdict::DoesNotFit {
            required_bytes: required,
            available_bytes: available,
            shortfall_bytes: required - available,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_kv_inputs_produce_a_labeled_conservative_fit() {
        let estimate = estimate_model_fit(&ModelFitInput {
            model_file_bytes: 1_000_000_000,
            context_tokens: 1_000,
            kv_bytes_per_token: Some(100_000),
            available_ram_bytes: Some(2_000_000_000),
            planned_vram_bytes: Some(800_000_000),
            available_vram_bytes: Some(1_000_000_000),
        })
        .expect("estimate fit");

        assert_eq!(estimate.kv_cache.known_bytes(), Some(100_000_000));
        assert!(matches!(estimate.ram, FitVerdict::Fits { .. }));
        assert!(matches!(estimate.vram, FitVerdict::Fits { .. }));
        assert!(estimate.performance_unknown_reason.contains("no benchmark"));
    }

    #[test]
    fn missing_kv_inputs_stay_unknown_but_provable_shortfall_fails() {
        let estimate = estimate_model_fit(&ModelFitInput {
            model_file_bytes: 1_000_000_000,
            context_tokens: 8_192,
            kv_bytes_per_token: None,
            available_ram_bytes: Some(500_000_000),
            planned_vram_bytes: None,
            available_vram_bytes: Some(8_000_000_000),
        })
        .expect("estimate fit");

        assert!(matches!(estimate.kv_cache, ByteEstimate::Unknown { .. }));
        assert!(matches!(estimate.ram, FitVerdict::DoesNotFit { .. }));
        assert!(matches!(estimate.vram, FitVerdict::Unknown { .. }));
    }
}
