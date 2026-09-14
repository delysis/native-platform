//! Admission estimates, not measured RSS or a hard allocator limit.
//!
//! Conventional attention uses F16 K/V (the context defaults in this engine),
//! one unified context shared by sequences, and padded context cells. Other
//! layouts retain an explicit heuristic rather than pretending a transformer
//! formula describes recurrent/MLA/hybrid state.
use llama_cpp_2::gguf::GgufContext;
use llama_native_types::NativeModelConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEstimateBasis {
    AttentionMetadata,
    ConfigurationHeuristic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeMemoryEstimate {
    pub model_and_projector_bytes: u64,
    pub kv_bytes: u64,
    pub backend_workspace_bytes: u64,
    pub sequence_metadata_bytes: u64,
    pub total_bytes: u64,
    pub basis: MemoryEstimateBasis,
}

#[derive(Clone, Copy)]
struct AttentionLayout {
    layers: u64,
    kv_heads: u64,
    key_width: u64,
    value_width: u64,
}

fn read_u32(metadata: &GgufContext, key: &str) -> Option<u64> {
    let index = metadata.find_key(key);
    (index >= 0 && metadata.kv_type(index) == llama_cpp_sys_2::GGUF_TYPE_UINT32)
        .then(|| u64::from(metadata.val_u32(index)))
}

fn attention_layout(config: &NativeModelConfig) -> Option<AttentionLayout> {
    let metadata = GgufContext::from_file(&config.model_path)?;
    let index = metadata.find_key("general.architecture");
    if index < 0 || metadata.kv_type(index) != llama_cpp_sys_2::GGUF_TYPE_STRING {
        return None;
    }
    let architecture = metadata.val_str(index)?;
    if !matches!(
        architecture,
        "llama" | "qwen2" | "qwen3" | "qwen2moe" | "qwen3moe" | "gemma" | "gemma2"
    ) {
        return None;
    }
    let read = |suffix: &str| read_u32(&metadata, &format!("{architecture}.{suffix}"));
    let embedding = read("embedding_length")?;
    let heads = read("attention.head_count")?;
    if heads == 0 || embedding == 0 || embedding % heads != 0 {
        return None;
    }
    let layout = AttentionLayout {
        layers: read("block_count")?,
        kv_heads: read("attention.head_count_kv").unwrap_or(heads),
        key_width: read("attention.key_length").unwrap_or(embedding / heads),
        value_width: read("attention.value_length").unwrap_or(embedding / heads),
    };
    (layout.layers > 0 && layout.kv_heads > 0 && layout.key_width > 0 && layout.value_width > 0)
        .then_some(layout)
}

/// Reads metadata only; never loads tensors or creates a native model context.
/// All totals are estimates. Backend scratch, graph buffers, mmap residency,
/// multimodal/recurrent state and other processes still require measurement.
#[must_use]
pub fn estimate_memory_reservation(
    config: &NativeModelConfig,
    model_bytes: u64,
    projector_bytes: u64,
) -> NativeMemoryEstimate {
    estimate(
        config,
        model_bytes,
        projector_bytes,
        attention_layout(config),
    )
}

fn scaled_reserve(bytes: u64, units: u64, baseline: u64) -> u64 {
    bytes
        .checked_mul(units)
        .map_or(u64::MAX, |product| product.div_ceil(baseline))
}

fn estimate(
    config: &NativeModelConfig,
    model_bytes: u64,
    projector_bytes: u64,
    layout: Option<AttentionLayout>,
) -> NativeMemoryEstimate {
    const MIN_WORKSPACE: u64 = 384 * 1024 * 1024;
    // Deliberately use requested context even when the model may cap it lower.
    // 256-cell rounding conservatively covers the pinned context padding.
    let context = u64::from(config.context_tokens.max(512)).div_ceil(256) * 256;
    let workspace = (model_bytes / 2).max(MIN_WORKSPACE);
    let (kv_bytes, basis, backend_baseline) = if let Some(layout) = layout {
        (
            context
                .saturating_mul(layout.layers)
                .saturating_mul(layout.kv_heads)
                .saturating_mul(layout.key_width.saturating_add(layout.value_width))
                .saturating_mul(2),
            MemoryEstimateBasis::AttentionMetadata,
            workspace,
        )
    } else {
        // Explicit fallback: divide the historical runtime reserve equally
        // between context and workspace, then scale each requested dimension.
        // This is not a conservative bound for unknown model architectures.
        (
            scaled_reserve(workspace / 2, context, 8192),
            MemoryEstimateBasis::ConfigurationHeuristic,
            workspace / 2,
        )
    };
    // Includes a conservative baseline for CPU/Metal graph and scratch storage;
    // host/device duplication and allocator peaks remain backend-dependent.
    let backend_workspace_bytes = scaled_reserve(
        backend_baseline,
        u64::from(config.batch_tokens.max(512)),
        512,
    );
    let sequence_metadata_bytes = context
        .saturating_mul(u64::from(config.max_sequences))
        .saturating_mul(16);
    let model_and_projector_bytes = model_bytes.saturating_add(projector_bytes);
    let total_bytes = model_and_projector_bytes
        .saturating_add(kv_bytes)
        .saturating_add(backend_workspace_bytes)
        .saturating_add(sequence_metadata_bytes);
    NativeMemoryEstimate {
        model_and_projector_bytes,
        kv_bytes,
        backend_workspace_bytes,
        sequence_metadata_bytes,
        total_bytes,
        basis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_estimate_tracks_context_batch_and_sequence_metadata_without_multiplying_unified_kv()
     {
        let mut config = NativeModelConfig::local("fixture.gguf".into());
        config.context_tokens = 8192;
        config.batch_tokens = 512;
        config.max_sequences = 1;
        let layout = Some(AttentionLayout {
            layers: 32,
            kv_heads: 8,
            key_width: 128,
            value_width: 128,
        });
        let base = estimate(&config, 4 << 30, 0, layout);
        assert_eq!(base.kv_bytes, 1 << 30);
        config.context_tokens *= 2;
        let longer = estimate(&config, 4 << 30, 0, layout);
        assert_eq!(longer.kv_bytes, 2 * base.kv_bytes);
        assert!(longer.total_bytes > base.total_bytes);
        config.max_sequences = 4;
        let sequences = estimate(&config, 4 << 30, 0, layout);
        assert_eq!(sequences.kv_bytes, longer.kv_bytes);
        assert!(sequences.sequence_metadata_bytes > longer.sequence_metadata_bytes);
        config.batch_tokens *= 2;
        assert!(
            estimate(&config, 4 << 30, 0, layout).backend_workspace_bytes
                > sequences.backend_workspace_bytes
        );
    }

    #[test]
    fn unknown_layout_is_labeled_and_overflow_saturates() {
        let mut config = NativeModelConfig::local("fixture.gguf".into());
        let base = estimate(&config, 100 << 20, 0, None);
        assert_eq!(base.basis, MemoryEstimateBasis::ConfigurationHeuristic);
        config.context_tokens *= 2;
        assert!(estimate(&config, 100 << 20, 0, None).total_bytes > base.total_bytes);
        assert_eq!(
            estimate(&config, u64::MAX, u64::MAX, None).total_bytes,
            u64::MAX
        );
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH and a supported attention GGUF"]
    fn real_model_metadata_drives_estimate_without_tensor_load() {
        let mut config = NativeModelConfig::local(
            std::env::var("MOM_LLAMA_MODEL_PATH")
                .expect("model path")
                .into(),
        );
        let size = std::fs::metadata(&config.model_path)
            .expect("model metadata")
            .len();
        let base = estimate_memory_reservation(&config, size, 0);
        eprintln!("memory estimate: {base:?}");
        assert_eq!(base.basis, MemoryEstimateBasis::AttentionMetadata);
        assert!(base.kv_bytes > 0);
        config.context_tokens *= 2;
        assert_eq!(
            estimate_memory_reservation(&config, size, 0).kv_bytes,
            2 * base.kv_bytes
        );
    }
}
