use llama_native_types::{MAX_PARALLEL_SEQUENCES, NativeModelConfig};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MINIMUM_CONTEXT_TOKENS: u32 = 512;
pub const DEFAULT_MAXIMUM_CONTEXT_TOKENS: u32 = 262_144;
const GIB: u64 = 1024 * 1024 * 1024;

/// The user's execution limits. Native inspection may cap the actual context
/// to model metadata; these requested values must not be silently rewritten.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelExecutionLimits {
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub parallel_sequences: u32,
}

impl ModelExecutionLimits {
    pub fn validate(self) -> Result<Self, ContextPlanError> {
        if self.context_tokens < MINIMUM_CONTEXT_TOKENS {
            return Err(ContextPlanError::InvalidLimits(
                "context_tokens must be at least 512",
            ));
        }
        if self.batch_tokens == 0 {
            return Err(ContextPlanError::InvalidLimits(
                "batch_tokens must be positive",
            ));
        }
        if !(1..=MAX_PARALLEL_SEQUENCES).contains(&self.parallel_sequences) {
            return Err(ContextPlanError::InvalidLimits(
                "parallel_sequences is outside native capacity",
            ));
        }
        Ok(self)
    }

    pub fn apply_to(self, config: &mut NativeModelConfig) -> Result<(), ContextPlanError> {
        self.validate()?;
        config.context_tokens = self.context_tokens;
        config.batch_tokens = self.batch_tokens;
        config.max_sequences = self.parallel_sequences;
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContextPlanError {
    #[error("invalid model execution limits: {0}")]
    InvalidLimits(&'static str),
    #[error(
        "automatic context estimate cannot fit the minimum 512 tokens in the available host budget"
    )]
    InsufficientEstimatedMemory,
}

/// Product headroom policy, not an allocator limit or a live memory reading.
#[must_use]
pub fn automatic_resident_memory_budget(physical_memory_bytes: Option<u64>) -> u64 {
    physical_memory_bytes.map_or(8 * GIB, |bytes| (bytes / 2).clamp(2 * GIB, 64 * GIB))
}

/// Inputs must be freshly read by the caller. The estimator deliberately owns
/// no sysinfo or filesystem access and cannot authorize model loading.
#[derive(Clone, Copy, Debug)]
pub struct ContextMemoryBudget {
    pub model_file_bytes: u64,
    pub projector_file_bytes: u64,
    pub available_memory_bytes: u64,
    pub total_memory_bytes: u64,
    pub host_memory_budget_bytes: u64,
    /// Explicit model-family heuristic. It is not a measured universal KV cost.
    pub estimated_kv_bytes_per_token: u64,
}

/// A request plan. The loaded native descriptor remains allocation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextPlan {
    pub requested_maximum_tokens: u32,
    pub selected_tokens: u32,
    pub estimated_kv_budget_bytes: u64,
    pub estimated_kv_bytes_per_token: u64,
}

impl ContextMemoryBudget {
    pub fn plan(self, maximum_tokens: u32) -> Result<ContextPlan, ContextPlanError> {
        if maximum_tokens < MINIMUM_CONTEXT_TOKENS || self.estimated_kv_bytes_per_token == 0 {
            return Err(ContextPlanError::InvalidLimits(
                "maximum context must be at least 512 and KV estimate must be positive",
            ));
        }
        let runtime_without_kv = self
            .model_file_bytes
            .saturating_add((self.model_file_bytes / 2).max(384 * 1024 * 1024))
            .saturating_add(self.projector_file_bytes);
        let headroom = (self.total_memory_bytes / 8).max(2 * GIB);
        let kv_budget = self
            .available_memory_bytes
            .saturating_sub(headroom)
            .min(self.host_memory_budget_bytes)
            .saturating_sub(runtime_without_kv);
        let affordable =
            (kv_budget / self.estimated_kv_bytes_per_token).min(u64::from(maximum_tokens));
        let affordable =
            u32::try_from(affordable).map_err(|_| ContextPlanError::InsufficientEstimatedMemory)?;
        if affordable < MINIMUM_CONTEXT_TOKENS {
            return Err(ContextPlanError::InsufficientEstimatedMemory);
        }
        Ok(ContextPlan {
            requested_maximum_tokens: maximum_tokens,
            selected_tokens: 1_u32 << affordable.ilog2(),
            estimated_kv_budget_bytes: kv_budget,
            estimated_kv_bytes_per_token: self.estimated_kv_bytes_per_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> ContextMemoryBudget {
        ContextMemoryBudget {
            model_file_bytes: 7 * GIB,
            projector_file_bytes: 0,
            available_memory_bytes: 112 * GIB,
            total_memory_bytes: 128 * GIB,
            host_memory_budget_bytes: u64::MAX,
            estimated_kv_bytes_per_token: 384 * 1024,
        }
    }

    #[test]
    fn automatic_plan_preserves_requested_cap_and_labels_estimate() {
        let plan = budget().plan(100_000).expect("fits");
        assert_eq!(plan.requested_maximum_tokens, 100_000);
        assert_eq!(plan.selected_tokens, 65_536);
        assert_eq!(plan.estimated_kv_bytes_per_token, 384 * 1024);
    }

    #[test]
    fn automatic_context_never_invents_an_affordable_minimum() {
        let mut budget = budget();
        budget.host_memory_budget_bytes = 8 * GIB;
        assert_eq!(
            budget.plan(262_144),
            Err(ContextPlanError::InsufficientEstimatedMemory)
        );
        budget.model_file_bytes = u64::MAX;
        assert_eq!(
            budget.plan(262_144),
            Err(ContextPlanError::InsufficientEstimatedMemory)
        );
        assert!(budget.plan(511).is_err());
    }

    #[test]
    fn invalid_explicit_limits_leave_destination_unchanged() {
        let mut config = NativeModelConfig::local("model.gguf".into());
        let original = config.clone();
        let limits = ModelExecutionLimits {
            context_tokens: 8_192,
            batch_tokens: 512,
            parallel_sequences: 5,
        };
        assert!(limits.apply_to(&mut config).is_err());
        assert_eq!(config, original);
    }

    #[test]
    fn automatic_host_budget_retains_conservative_bounds() {
        assert_eq!(automatic_resident_memory_budget(None), 8 * GIB);
        assert_eq!(automatic_resident_memory_budget(Some(GIB)), 2 * GIB);
        assert_eq!(automatic_resident_memory_budget(Some(16 * GIB)), 8 * GIB);
        assert_eq!(automatic_resident_memory_budget(Some(512 * GIB)), 64 * GIB);
    }
}
