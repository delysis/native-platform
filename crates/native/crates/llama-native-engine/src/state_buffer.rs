#![allow(unsafe_code)]

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::session::LlamaStateSeqFlags;
use llama_native_types::{
    ModelFingerprint, NativeError, NativeErrorCode, SequenceStateBlob, SequenceStateCell,
    SequenceStateIdentity,
};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAGIC: &[u8; 16] = llama_native_types::SEQUENCE_STATE_MAGIC;
const HEADER_BYTES: usize = llama_native_types::SEQUENCE_STATE_HEADER_BYTES;
const MAX_LIVE_EXPORTS: usize = 256;

#[derive(Debug, Default)]
pub(crate) struct LiveExportRegistry {
    allocations: VecDeque<SequenceStateIdentity>,
}

impl LiveExportRegistry {
    fn clear(&mut self) {
        self.allocations.clear();
    }
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.allocations.is_empty()
    }
}

pub(crate) type LiveExports = Arc<Mutex<LiveExportRegistry>>;
thread_local! {
    static LIVE_EXPORTS: RefCell<LiveExports> = RefCell::new(LiveExports::default());
}

pub(crate) struct WorkerExports(LiveExports);
impl Drop for WorkerExports {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

pub(crate) fn bind_worker_exports(exports: &LiveExports) -> WorkerExports {
    LIVE_EXPORTS.with_borrow_mut(|current| *current = Arc::clone(exports));
    WorkerExports(Arc::clone(exports))
}

pub(crate) fn forget_live_exports() {
    LIVE_EXPORTS.with_borrow(|exports| {
        exports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    });
}

fn remember_allocation(exports: &LiveExports, state: &SequenceStateBlob) {
    if let Ok(mut registry) = exports.lock() {
        registry.allocations.retain(SequenceStateIdentity::is_alive);
        if registry.allocations.len() == MAX_LIVE_EXPORTS {
            registry.allocations.pop_front();
        }
        registry.allocations.push_back(state.allocation_identity());
    }
}

pub(crate) fn has_reusable_export(exports: &LiveExports, state: &SequenceStateBlob) -> bool {
    exports.lock().is_ok_and(|registry| {
        registry
            .allocations
            .iter()
            .any(|identity| identity.matches(state))
    })
}

fn incompatible(message: &str) -> NativeError {
    NativeError::new(NativeErrorCode::CacheIncompatible, message)
}

fn fingerprint_digest(fingerprint: &ModelFingerprint) -> Result<[u8; 32], NativeError> {
    let bytes = serde_json::to_vec(fingerprint)
        .map_err(|_| incompatible("could not encode native state fingerprint"))?;
    Ok(Sha256::digest(bytes).into())
}

#[cfg(test)]
fn envelope(
    raw: Vec<u8>,
    fingerprint: &ModelFingerprint,
    sequence_id: i32,
    token_count: usize,
    token_ids: Vec<i32>,
) -> Result<SequenceStateBlob, NativeError> {
    let mut bytes = Vec::with_capacity(HEADER_BYTES + raw.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&fingerprint_digest(fingerprint)?);
    bytes.extend_from_slice(&raw);
    Ok(SequenceStateBlob {
        storage_proof: None,
        sequence_id,
        token_count,
        bytes: bytes.into(),
        token_ids,
        cells: Vec::new(),
    })
}

fn validate_cell_metadata(state: &SequenceStateBlob) -> Result<(), NativeError> {
    if state.cells.is_empty() {
        if state.token_count != state.token_ids.len() {
            return Err(incompatible("text sequence token metadata is inconsistent"));
        }
        return Ok(());
    }
    if state.token_count != state.cells.len() {
        return Err(incompatible("mixed sequence cell metadata is inconsistent"));
    }
    let leading_text: Vec<i32> = state
        .cells
        .iter()
        .take_while(|cell| matches!(cell, SequenceStateCell::Text(_)))
        .filter_map(|cell| match cell {
            SequenceStateCell::Text(token) => Some(*token),
            _ => None,
        })
        .collect();
    if state.token_ids != leading_text {
        return Err(incompatible(
            "mixed sequence leading text metadata is inconsistent",
        ));
    }
    Ok(())
}

fn validated_payload<'a>(
    state: &'a SequenceStateBlob,
    fingerprint: &ModelFingerprint,
) -> Result<&'a [u8], NativeError> {
    if state.bytes.len() < HEADER_BYTES || !state.bytes.starts_with(MAGIC) {
        return Err(incompatible(
            "saved sequence has no supported context binding",
        ));
    }
    if state.bytes[MAGIC.len()..HEADER_BYTES] != fingerprint_digest(fingerprint)? {
        return Err(incompatible(
            "saved sequence belongs to a different model or context",
        ));
    }
    Ok(&state.bytes[HEADER_BYTES..])
}

/// Check the current configuration binding before any native mutation.
pub(crate) fn validate_binding(
    state: &SequenceStateBlob,
    fingerprint: &ModelFingerprint,
) -> Result<(), NativeError> {
    validate_cell_metadata(state)?;
    if state.bytes.starts_with(MAGIC) {
        validated_payload(state, fingerprint)?;
    }
    Ok(())
}

pub(crate) fn export_sequence(
    context: &LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    sequence_id: i32,
    token_count: usize,
    token_ids: Vec<i32>,
) -> Result<SequenceStateBlob, NativeError> {
    export_sequence_bounded(
        context,
        fingerprint,
        sequence_id,
        token_count,
        token_ids,
        usize::MAX,
    )?
    .ok_or_else(|| incompatible("sequence exceeds addressable memory"))
}

pub(crate) fn export_sequence_bounded(
    context: &LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    sequence_id: i32,
    token_count: usize,
    token_ids: Vec<i32>,
    max_bytes: usize,
) -> Result<Option<SequenceStateBlob>, NativeError> {
    export_sequence_bounded_while(
        context,
        fingerprint,
        sequence_id,
        token_count,
        token_ids,
        max_bytes,
        &|| true,
    )
}

pub(crate) fn export_sequence_bounded_while(
    context: &LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    sequence_id: i32,
    token_count: usize,
    token_ids: Vec<i32>,
    max_bytes: usize,
    admitted: &dyn Fn() -> bool,
) -> Result<Option<SequenceStateBlob>, NativeError> {
    export_sequence_cells_internal(
        context,
        fingerprint,
        SequenceExportMetadata {
            source_sequence_id: sequence_id,
            export_sequence_id: sequence_id,
            token_count,
            token_ids,
            cells: Vec::new(),
        },
        max_bytes,
        admitted,
    )
}

pub(crate) fn export_sequence_cells_bounded_while(
    context: &LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    source_sequence_id: i32,
    export_sequence_id: i32,
    cells: Vec<SequenceStateCell>,
    max_bytes: usize,
    admitted: &dyn Fn() -> bool,
) -> Result<Option<SequenceStateBlob>, NativeError> {
    if cells.is_empty() {
        return Err(incompatible("mixed sequence export requires cells"));
    }
    let token_ids = cells
        .iter()
        .take_while(|cell| matches!(cell, SequenceStateCell::Text(_)))
        .filter_map(|cell| match cell {
            SequenceStateCell::Text(token) => Some(*token),
            _ => None,
        })
        .collect();
    let token_count = cells.len();
    export_sequence_cells_internal(
        context,
        fingerprint,
        SequenceExportMetadata {
            source_sequence_id,
            export_sequence_id,
            token_count,
            token_ids,
            cells,
        },
        max_bytes,
        admitted,
    )
}

struct SequenceExportMetadata {
    source_sequence_id: i32,
    export_sequence_id: i32,
    token_count: usize,
    token_ids: Vec<i32>,
    cells: Vec<SequenceStateCell>,
}

fn export_sequence_cells_internal(
    context: &LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    metadata: SequenceExportMetadata,
    max_bytes: usize,
    admitted: &dyn Fn() -> bool,
) -> Result<Option<SequenceStateBlob>, NativeError> {
    if !admitted() {
        return Ok(None);
    }
    let SequenceExportMetadata {
        source_sequence_id,
        export_sequence_id,
        token_count,
        token_ids,
        cells,
    } = metadata;
    if source_sequence_id < 0
        || source_sequence_id as u32 >= fingerprint.max_sequences
        || export_sequence_id < 0
        || export_sequence_id as u32 >= fingerprint.max_sequences
        || token_count == 0
        || token_count > fingerprint.context_tokens as usize
        || (cells.is_empty() && token_count != token_ids.len())
        || (!cells.is_empty()
            && (token_count != cells.len()
                || token_ids
                    != cells
                        .iter()
                        .take_while(|cell| matches!(cell, SequenceStateCell::Text(_)))
                        .filter_map(|cell| match cell {
                            SequenceStateCell::Text(token) => Some(*token),
                            _ => None,
                        })
                        .collect::<Vec<_>>()))
    {
        return Err(incompatible(
            "sequence export is outside the resident context",
        ));
    }
    // Compact SWA exports discard keys needed after a longest-prefix rewind.
    let flags = LlamaStateSeqFlags::KEEP_ALL_SWA_CELLS;
    let size = context.state_seq_get_size_ext(source_sequence_id, flags);
    if size == 0 {
        return Err(incompatible("llama.cpp reported an empty sequence state"));
    }
    let payload_end = HEADER_BYTES
        .checked_add(size)
        .ok_or_else(|| incompatible("sequence size overflow"))?;
    let capacity = payload_end;
    if capacity > max_bytes {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| incompatible("insufficient memory for sequence export"))?;
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&fingerprint_digest(fingerprint)?);
    while bytes.len() < payload_end {
        if !admitted() {
            return Ok(None);
        }
        bytes.resize((bytes.len() + 1024 * 1024).min(payload_end), 0);
    }
    if !admitted() {
        return Ok(None);
    }
    // Write directly into the final envelope. A full SWA state can be gigabytes;
    // a second raw buffer would needlessly double the export's peak memory.
    let written =
        context.state_seq_get_data_ext(&mut bytes[HEADER_BYTES..], source_sequence_id, flags);
    if written != size {
        return Err(incompatible("llama.cpp sequence export size changed"));
    }
    let state = SequenceStateBlob {
        storage_proof: None,
        sequence_id: export_sequence_id,
        token_count,
        bytes: bytes.into(),
        token_ids,
        cells,
    };
    Ok(finish_sequence_export(state, admitted))
}

fn finish_sequence_export(
    state: SequenceStateBlob,
    admitted: &dyn Fn() -> bool,
) -> Option<SequenceStateBlob> {
    if !admitted() {
        return None;
    }
    LIVE_EXPORTS.with_borrow(|exports| remember_allocation(exports, &state));
    Some(state)
}

/// An authenticated, fingerprint-bound native export. Construct this before
/// releasing useful resident state so rejected disk entries cannot evict it.
pub(crate) struct PreparedSequenceImport<'a> {
    raw: &'a [u8],
}

impl PreparedSequenceImport<'_> {
    pub(crate) fn restore(
        self,
        context: &mut LlamaContext<'_>,
        destination_sequence_id: i32,
    ) -> Result<(), NativeError> {
        // SAFETY: construction checks authenticated storage or an immutable live
        // worker allocation and the complete model/context binding. The borrowed
        // bytes cannot change between authentication and native import.
        let restored = unsafe {
            context.state_seq_set_data_ext(
                self.raw,
                destination_sequence_id,
                LlamaStateSeqFlags::empty(),
            )
        };
        if !restored {
            return Err(incompatible("llama.cpp rejected the sequence state"));
        }
        Ok(())
    }
}

/// None permits checked text-token replay, never the raw native parser.
pub(crate) fn prepare_sequence_import<'a>(
    fingerprint: &ModelFingerprint,
    state: &'a SequenceStateBlob,
) -> Result<Option<PreparedSequenceImport<'a>>, NativeError> {
    validate_cell_metadata(state)?;
    if !(state.bytes.starts_with(MAGIC)) {
        return Ok(None);
    }
    let raw = validated_payload(state, fingerprint)?;
    let authenticated = state.has_authenticated_storage_proof();
    if raw.is_empty()
        || !(authenticated
            || LIVE_EXPORTS.with_borrow(|exports| has_reusable_export(exports, state)))
    {
        return Ok(None);
    }
    Ok(Some(PreparedSequenceImport { raw }))
}

/// `false` requests token replay. It never authorizes the raw native parser.
pub(crate) fn import_sequence(
    context: &mut LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    state: &SequenceStateBlob,
    destination_sequence_id: i32,
) -> Result<bool, NativeError> {
    let Some(prepared) = prepare_sequence_import(fingerprint, state)? else {
        return Ok(false);
    };
    prepared.restore(context, destination_sequence_id)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint() -> ModelFingerprint {
        ModelFingerprint {
            model_id: "fixture".into(),
            model_size: 1,
            model_sha256: "model-a".into(),
            tokenizer_sha256: "tokens".into(),
            chat_template_sha256: "template".into(),
            multimodal_projector_sha256: None,
            binding_version: "binding".into(),
            build_id: "build".into(),
            backend: "cpu".into(),
            context_tokens: 8192,
            batch_tokens: 512,
            max_sequences: 4,
            rope_config_sha256: "rope".into(),
            kv_layout_sha256: "kv".into(),
        }
    }

    #[test]
    fn state_envelope_rejects_other_model_context_backend_and_unbound_bytes() {
        let original = fingerprint();
        let state = envelope(vec![1, 2, 3], &original, 0, 1, vec![7]).expect("fixture envelope");
        assert_eq!(
            validated_payload(&state, &original).expect("same binding"),
            &[1, 2, 3]
        );
        for field in ["model", "context", "backend", "kv", "sequences", "rope"] {
            let mut other = original.clone();
            match field {
                "model" => other.model_sha256 = "same-shape-other-model".into(),
                "context" => other.context_tokens += 1,
                "backend" => other.backend = "metal".into(),
                "kv" => other.kv_layout_sha256 = "other".into(),
                "sequences" => other.max_sequences += 1,
                _ => other.rope_config_sha256 = "other".into(),
            }
            assert!(validated_payload(&state, &other).is_err(), "{field}");
        }
        let mut arbitrary = state;
        arbitrary.bytes = vec![1, 2, 3].into();
        assert!(validated_payload(&arbitrary, &original).is_err());
    }

    #[test]
    fn live_allocations_expire_with_worker_and_bounded_registry_without_losing_replay() {
        let exports = LiveExports::default();
        let lifetime = bind_worker_exports(&exports);
        let state = envelope(vec![3; 4096], &fingerprint(), 0, 1, vec![7]).expect("state");
        remember_allocation(&exports, &state);
        assert!(has_reusable_export(&exports, &state));
        let compact = state
            .reconstruction()
            .sequence(state.token_ids.clone())
            .expect("replay");
        assert_eq!(compact.bytes.len(), HEADER_BYTES);
        assert!(!has_reusable_export(&exports, &compact));
        let mut retained = Vec::new();
        for _ in 0..MAX_LIVE_EXPORTS {
            let next = envelope(vec![3], &fingerprint(), 0, 1, vec![7]).expect("next state");
            remember_allocation(&exports, &next);
            retained.push(next);
        }
        assert!(!has_reusable_export(&exports, &state));
        remember_allocation(&exports, &state);
        drop(lifetime);
        assert!(exports.lock().expect("exports lock").is_empty());
        assert!(!has_reusable_export(&exports, &state));
        assert!(
            validated_payload(&compact, &fingerprint())
                .expect("validate compact replay")
                .is_empty()
        );
    }

    #[test]
    fn live_identity_rejects_mutation_and_untrusted_byte_copies_without_hashing() {
        let exports = LiveExports::default();
        let state = envelope(vec![1, 2, 3], &fingerprint(), 0, 1, vec![7]).expect("state");
        remember_allocation(&exports, &state);
        assert!(has_reusable_export(&exports, &state.clone()));
        for field in 0..5 {
            let mut altered = state.clone();
            match field {
                0 => altered.sequence_id = 1,
                1 => altered.token_count = 2,
                2 => altered.token_ids[0] = 8,
                3 => altered.cells.push(SequenceStateCell::Text(7)),
                _ => Arc::make_mut(&mut altered.bytes)[HEADER_BYTES] ^= 1,
            }
            assert!(!has_reusable_export(&exports, &altered));
        }
        let decoded: SequenceStateBlob =
            serde_json::from_slice(&serde_json::to_vec(&state).expect("serialize state"))
                .expect("deserialize state");
        assert_eq!(state, decoded);
        assert!(!has_reusable_export(&exports, &decoded));
        let mut sole = state;
        Arc::make_mut(&mut sole.bytes)[HEADER_BYTES] ^= 1;
        assert!(!has_reusable_export(&exports, &sole));
    }

    #[test]
    fn checkpoint_preemption_never_publishes_partial_receipt() {
        std::thread::spawn(|| {
            use std::cell::Cell;
            let exports = LiveExports::default();
            let _lifetime = bind_worker_exports(&exports);
            let state = envelope(vec![19; 4 * 1024 * 1024], &fingerprint(), 0, 2, vec![7, 8])
                .expect("state");
            let mut completed = None;
            for budget in 0..16 {
                let checks = Cell::new(0);
                let result = finish_sequence_export(state.clone(), &|| {
                    let n = checks.get();
                    checks.set(n + 1);
                    n < budget
                });
                if let Some(result) = result {
                    completed = Some(result);
                    break;
                }
                assert!(exports.lock().expect("receipts").is_empty());
            }
            let completed = completed.expect("full export eventually succeeds");
            assert_eq!(completed, state);
            assert!(has_reusable_export(&exports, &completed));
        })
        .join()
        .expect("fixture thread");
    }

    #[test]
    fn storage_admission_is_explicit_and_cannot_bypass_binding_or_mutation_checks() {
        let fp = fingerprint();
        let mut state = envelope(vec![9, 8, 7, 6], &fp, 0, 3, vec![7]).expect("state");
        Arc::make_mut(&mut state.bytes)[..MAGIC.len()].copy_from_slice(MAGIC);
        state.cells = vec![
            SequenceStateCell::Text(7),
            SequenceStateCell::AudioFrame { digest: [2; 32] },
            SequenceStateCell::AudioFrame { digest: [3; 32] },
        ];
        assert!(
            prepare_sequence_import(&fp, &state)
                .expect("binding")
                .is_none()
        );
        // Simulate the trusted adapter boundary; AEAD verification itself is
        // exercised by the product store corruption/roundtrip tests.
        state.mark_authenticated_storage();
        assert!(
            prepare_sequence_import(&fp, &state)
                .expect("admitted")
                .is_some()
        );
        let decoded: SequenceStateBlob =
            serde_json::from_slice(&serde_json::to_vec(&state).expect("serialize"))
                .expect("decode");
        assert!(
            prepare_sequence_import(&fp, &decoded)
                .expect("binding")
                .is_none()
        );
        let mut changed = state.clone();
        changed.cells[1] = SequenceStateCell::AudioFrame { digest: [4; 32] };
        assert!(
            prepare_sequence_import(&fp, &changed)
                .expect("binding")
                .is_none()
        );
        changed = state.clone();
        Arc::make_mut(&mut changed.bytes)[HEADER_BYTES] ^= 1;
        assert!(
            prepare_sequence_import(&fp, &changed)
                .expect("binding")
                .is_none()
        );
        let mut other_fp = fp;
        other_fp.context_tokens += 1;
        assert!(prepare_sequence_import(&other_fp, &state).is_err());
    }
}
