//! Admission estimates, not measured RSS or a hard allocator limit. The
//! model-size-scaled workspace reserve intentionally includes margin over
//! observed Gemma Metal graph buffers; it does not bound transient exports.
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

#[derive(Clone)]
struct AttentionLayout {
    layers: u64,
    kv_heads: u64,
    per_layer_kv_heads: Option<Vec<u64>>,
    key_width: u64,
    value_width: u64,
    per_layer_swa: Option<Vec<bool>>,
    key_width_swa: Option<u64>,
    value_width_swa: Option<u64>,
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
        "llama"
            | "qwen2"
            | "qwen3"
            | "qwen2moe"
            | "qwen3moe"
            | "gemma"
            | "gemma2"
            | "gemma3"
            | "gemma4"
    ) {
        return None;
    }
    let read = |suffix: &str| read_u32(&metadata, &format!("{architecture}.{suffix}"));
    let embedding = read("embedding_length")?;
    let heads = read("attention.head_count")?;
    if heads == 0 || embedding == 0 || embedding % heads != 0 {
        return None;
    }
    let layers = read("block_count")?;
    let layers_usize = usize::try_from(layers).ok()?;
    let read_i32_array = |suffix: &str| {
        let index = metadata.find_key(&format!("{architecture}.{suffix}"));
        (index >= 0)
            .then(|| metadata.val_array_i32(index))
            .flatten()
            .filter(|values| values.len() == layers_usize)
            .and_then(|values| {
                values
                    .into_iter()
                    .map(|value| u64::try_from(value).ok().filter(|value| *value > 0))
                    .collect::<Option<Vec<_>>>()
            })
    };
    let read_bool_array = |suffix: &str| {
        let index = metadata.find_key(&format!("{architecture}.{suffix}"));
        (index >= 0)
            .then(|| metadata.val_array_bool(index))
            .flatten()
            .filter(|values| values.len() == layers_usize)
    };
    let key_width = read("attention.key_length").unwrap_or(embedding / heads);
    let value_width = read("attention.value_length").unwrap_or(embedding / heads);
    let key_width_swa = read("attention.key_length_swa").filter(|width| *width > 0);
    let value_width_swa = read("attention.value_length_swa").filter(|width| *width > 0);
    let per_layer_kv_heads = read_i32_array("attention.head_count_kv");
    let per_layer_swa = read_bool_array("attention.sliding_window_pattern");
    let kv_heads = read("attention.head_count_kv").unwrap_or(heads);
    let layout = AttentionLayout {
        layers,
        kv_heads,
        per_layer_kv_heads,
        key_width,
        value_width,
        per_layer_swa,
        key_width_swa,
        value_width_swa,
    };
    (layout.layers > 0 && layout.kv_heads > 0 && layout.key_width > 0 && layout.value_width > 0)
        .then_some(layout)
}

fn kv_bytes_per_token(layout: &AttentionLayout) -> u64 {
    let conservative_width = layout
        .key_width
        .max(layout.key_width_swa.unwrap_or(0))
        .saturating_add(layout.value_width.max(layout.value_width_swa.unwrap_or(0)));
    let Some(layer_count) = usize::try_from(layout.layers).ok() else {
        return u64::MAX;
    };
    let Some(heads) = layout
        .per_layer_kv_heads
        .as_deref()
        .filter(|values| values.len() == layer_count)
    else {
        return layout
            .layers
            .saturating_mul(layout.kv_heads)
            .saturating_mul(conservative_width);
    };
    let Some(swa) = layout
        .per_layer_swa
        .as_deref()
        .filter(|values| values.len() == layer_count)
        .filter(|_| layout.key_width_swa.is_some() && layout.value_width_swa.is_some())
    else {
        return heads
            .iter()
            .copied()
            .fold(0_u64, u64::saturating_add)
            .saturating_mul(conservative_width);
    };

    heads
        .iter()
        .zip(swa)
        .map(|(heads, is_swa)| {
            let (key_width, value_width) = if *is_swa {
                (
                    layout.key_width_swa.unwrap_or(layout.key_width),
                    layout.value_width_swa.unwrap_or(layout.value_width),
                )
            } else {
                (layout.key_width, layout.value_width)
            };
            heads.saturating_mul(key_width.saturating_add(value_width))
        })
        .fold(0_u64, u64::saturating_add)
}

/// Reads metadata only; never loads tensors or creates a native model context.
/// All totals are estimates, never allocator caps. Backend scratch, graph
/// buffers, mmap residency, state-export peaks, multimodal/recurrent state and
/// other processes still require measurement.
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

/// Training context capacity from GGUF metadata, without loading tensor weights.
#[must_use]
pub fn model_context_capacity(config: &NativeModelConfig) -> Option<u32> {
    let metadata = GgufContext::from_file(&config.model_path)?;
    let index = metadata.find_key("general.architecture");
    if index < 0 || metadata.kv_type(index) != llama_cpp_sys_2::GGUF_TYPE_STRING {
        return None;
    }
    let architecture = metadata.val_str(index)?;
    u32::try_from(read_u32(
        &metadata,
        &format!("{architecture}.context_length"),
    )?)
    .ok()
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
    let requested = u64::from(config.context_tokens.max(512));
    let context = if config.kv_unified {
        requested.div_ceil(256) * 256
    } else {
        // Each independent stream is padded separately by the native context.
        let streams = u64::from(config.max_sequences.max(1));
        requested.div_ceil(streams).div_ceil(256) * 256 * streams
    };
    let workspace = (model_bytes / 2).max(MIN_WORKSPACE);
    let (kv_bytes, basis, backend_baseline) = if let Some(layout) = layout {
        (
            context
                .saturating_mul(kv_bytes_per_token(&layout))
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
            per_layer_kv_heads: None,
            key_width: 128,
            value_width: 128,
            per_layer_swa: None,
            key_width_swa: None,
            value_width_swa: None,
        });
        let base = estimate(&config, 4 << 30, 0, layout.clone());
        assert_eq!(base.kv_bytes, 1 << 30);
        config.context_tokens *= 2;
        let longer = estimate(&config, 4 << 30, 0, layout.clone());
        assert_eq!(longer.kv_bytes, 2 * base.kv_bytes);
        assert!(longer.total_bytes > base.total_bytes);
        config.max_sequences = 4;
        let sequences = estimate(&config, 4 << 30, 0, layout.clone());
        assert_eq!(sequences.kv_bytes, longer.kv_bytes);
        assert!(sequences.sequence_metadata_bytes > longer.sequence_metadata_bytes);
        config.batch_tokens *= 2;
        assert!(
            estimate(&config, 4 << 30, 0, layout).backend_workspace_bytes
                > sequences.backend_workspace_bytes
        );
    }

    #[test]
    fn gemma4_layer_arrays_and_swa_pattern_match_native_8192_kv_size() {
        let layout = AttentionLayout {
            layers: 48,
            kv_heads: 16,
            per_layer_kv_heads: Some(
                (0..48)
                    .map(|layer| if layer % 6 == 5 { 1 } else { 8 })
                    .collect(),
            ),
            key_width: 512,
            value_width: 512,
            per_layer_swa: Some((0..48).map(|layer| layer % 6 != 5).collect()),
            key_width_swa: Some(256),
            value_width_swa: Some(256),
        };
        let mut config = NativeModelConfig::local("fixture.gguf".into());
        config.context_tokens = 8192;
        let result = estimate(&config, 0, 0, Some(layout.clone()));
        assert_eq!(result.kv_bytes, 2688 * 1024 * 1024);

        // If a SWA pattern is malformed or unavailable, retain full global
        // widths while still using the valid per-layer KV head counts.
        let conservative = AttentionLayout {
            per_layer_swa: None,
            ..layout
        };
        assert_eq!(
            estimate(&config, 0, 0, Some(conservative)).kv_bytes,
            5248 * 1024 * 1024
        );
    }

    #[test]
    fn independent_stream_estimate_covers_per_stream_padding() {
        let mut config = NativeModelConfig::local("fixture.gguf".into());
        config.context_tokens = 512;
        config.max_sequences = 4;
        let unified = estimate(&config, 4 << 30, 0, None);
        config.kv_unified = false;
        let independent = estimate(&config, 4 << 30, 0, None);
        assert_eq!(independent.kv_bytes, unified.kv_bytes * 2);
        config.context_tokens = 8192;
        let independent = estimate(&config, 4 << 30, 0, None);
        config.kv_unified = true;
        assert_eq!(
            independent.kv_bytes,
            estimate(&config, 4 << 30, 0, None).kv_bytes
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
