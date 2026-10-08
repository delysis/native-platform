//! Host-wide admission planning; estimates include workspace and prefix copies.
use crate::config::Settings;
use crate::engine::ValidationBlocker;
use crate::receipts::Blocker;
use llama_native_engine::{
    MemoryEstimateBasis, estimate_memory_reservation, model_context_capacity,
};
use llama_native_types::NativeModelConfig;
use std::path::Path;

pub(crate) fn prefix_cache_bytes(budget: u64) -> u64 {
    (budget / 16).min(256 * 1024 * 1024)
}
pub(crate) fn resident_budget(budget: u64) -> u64 {
    budget.saturating_sub(prefix_cache_bytes(budget))
}

pub(crate) fn weights_fit(model: &Path, projector: Option<&Path>, ram: u64) -> bool {
    weight_bytes(model, projector).is_some_and(|bytes| bytes <= ram / 2)
}

fn weight_bytes(model: &Path, projector: Option<&Path>) -> Option<u64> {
    let model = std::fs::metadata(model).ok()?.len();
    let projector = match projector {
        Some(path) => std::fs::metadata(path).ok()?.len(),
        None => 0,
    };
    model.checked_add(projector)
}

pub(crate) fn plan_context(config: &NativeModelConfig, budget: u64) -> Option<u32> {
    let model_bytes = std::fs::metadata(&config.model_path).ok()?.len();
    let projector_bytes = match config.mmproj_path.as_deref() {
        Some(path) => std::fs::metadata(path).ok()?.len(),
        None => 0,
    };
    let mut probe = config.clone();
    probe.context_tokens = 512;
    let estimate = estimate_memory_reservation(&probe, model_bytes, projector_bytes);
    // Unknown recurrent/MLA layouts have no defensible transformer KV formula.
    if estimate.basis != MemoryEstimateBasis::AttentionMetadata {
        return None;
    }
    let fixed = estimate
        .model_and_projector_bytes
        .checked_add(estimate.backend_workspace_bytes)?;
    let per_block = estimate
        .kv_bytes
        .checked_add(estimate.sequence_metadata_bytes)?;
    context_from_cost(
        fixed,
        per_block,
        resident_budget(budget),
        model_context_capacity(config)?,
    )
}

fn context_from_cost(fixed: u64, per_block: u64, budget: u64, capacity: u32) -> Option<u32> {
    if per_block == 0 {
        return None;
    }
    let blocks = budget.checked_sub(fixed)? / per_block;
    let context = blocks.min(u64::from(capacity) / 512).checked_mul(512)?;
    u32::try_from(context).ok().filter(|tokens| *tokens >= 512)
}

pub(crate) fn validate_profile(
    settings: &Settings,
    config: &NativeModelConfig,
) -> Result<(), ValidationBlocker> {
    let blocked = |code, message| ValidationBlocker {
        readiness: "blocked_memory_budget".into(),
        blocker: Blocker::new(code, message, Vec::new()),
    };
    let ram = crate::config::physical_memory_bytes().ok_or_else(|| {
        blocked(
            "host_memory_unavailable",
            "System memory could not be determined.",
        )
    })?;
    if !weights_fit(&config.model_path, config.mmproj_path.as_deref(), ram) {
        return Err(blocked(
            "model_weights_exceed_ram_share",
            "Model weights and projector exceed half of system memory. Choose smaller weights.",
        ));
    }
    let context = plan_context(config, settings.resident_memory_budget_bytes).ok_or_else(|| {
        blocked(
            "model_memory_plan_unavailable",
            "This model has no supported memory plan or cannot fit the host budget.",
        )
    })?;
    if config.context_tokens > context {
        return Err(blocked(
            "model_context_exceeds_ram_share",
            "The global context does not fit this model within the host memory budget. Choose smaller weights.",
        ));
    }
    Ok(())
}

pub(crate) fn reconcile_context(settings: &mut Settings) {
    let Some(path) = &settings.model_path else {
        return;
    };
    let Some(ram) = crate::config::physical_memory_bytes() else {
        return;
    };
    if !weights_fit(path, settings.mmproj_path.as_deref(), ram) {
        return;
    }
    let mut config = NativeModelConfig::local(path.clone());
    config.mmproj_path = settings.mmproj_path.clone();
    config.batch_tokens = settings.batch_tokens;
    config.max_sequences = settings.max_parallel_sequences.clamp(1, 4);
    if let Some(context) = plan_context(&config, settings.resident_memory_budget_bytes) {
        settings.context_tokens = context;
        settings
            .upstream_settings
            .insert("nativeContextTokens".into(), serde_json::json!(context));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weights_include_the_projector_and_never_exceed_half_of_ram() {
        let root = std::env::temp_dir().join(format!("mom-weight-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("weight fixture directory");
        let model = root.join("model.gguf");
        let projector = root.join("projector.gguf");
        std::fs::File::create(&model)
            .expect("model fixture")
            .set_len(8 * 1024 * 1024)
            .expect("model length");
        std::fs::File::create(&projector)
            .expect("projector fixture")
            .set_len(1024 * 1024)
            .expect("projector length");
        assert!(weights_fit(&model, None, 16 * 1024 * 1024));
        assert!(!weights_fit(&model, Some(&projector), 16 * 1024 * 1024));
        assert!(!weights_fit(&model, None, 15 * 1024 * 1024));
        std::fs::remove_dir_all(root).expect("remove only isolated weight fixtures");
    }

    #[test]
    fn planning_reserves_fixed_memory_and_rounds_down_at_model_capacity() {
        assert_eq!(context_from_cost(100, 20, 179, 4096), Some(1536));
        assert_eq!(context_from_cost(100, 20, 1000, 2048), Some(2048));
        assert_eq!(context_from_cost(100, 20, 119, 4096), None);
        assert_eq!(context_from_cost(u64::MAX, 20, 1000, 4096), None);
        assert_eq!(resident_budget(1024) + prefix_cache_bytes(1024), 1024);
    }
}
