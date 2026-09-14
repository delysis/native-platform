#![allow(unsafe_code)]

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::session::LlamaStateSeqFlags;
use llama_native_types::{ModelFingerprint, NativeError, NativeErrorCode, SequenceStateBlob};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::VecDeque;

const MAGIC: &[u8; 16] = b"native-seq-v1\0\0\0";
const HEADER_BYTES: usize = MAGIC.len() + 32;
const MAX_LIVE_RECEIPTS: usize = 256;

// Only the exporting worker can grant native-byte import authority. Receipts
// are bounded, contain no state bytes, and disappear with that worker. Eviction
// costs recomputation, never correctness. Persisted envelopes retain the full
// configuration binding but are replayed from validated token IDs after restart.
thread_local! {
    static LIVE_EXPORTS: RefCell<VecDeque<[u8; 32]>> = const { RefCell::new(VecDeque::new()) };
}

/// A failed native mutation invalidates live acceleration authority as well as
/// the resident KV contents. Saved token IDs remain eligible for fresh replay.
pub(crate) fn forget_live_exports() {
    LIVE_EXPORTS.with_borrow_mut(VecDeque::clear);
}

fn incompatible(message: &str) -> NativeError {
    NativeError::new(NativeErrorCode::CacheIncompatible, message)
}

fn fingerprint_digest(fingerprint: &ModelFingerprint) -> Result<[u8; 32], NativeError> {
    let bytes = serde_json::to_vec(fingerprint)
        .map_err(|_| incompatible("could not encode native state fingerprint"))?;
    Ok(Sha256::digest(bytes).into())
}

fn receipt(state: &SequenceStateBlob) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"native-sequence-export-receipt-v1");
    hash.update(state.sequence_id.to_le_bytes());
    hash.update((state.token_count as u64).to_le_bytes());
    hash.update((state.token_ids.len() as u64).to_le_bytes());
    for token in &state.token_ids {
        hash.update(token.to_le_bytes());
    }
    hash.update(&state.bytes);
    hash.finalize().into()
}

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
        sequence_id,
        token_count,
        bytes,
        token_ids,
    })
}

fn validated_payload<'a>(
    state: &'a SequenceStateBlob,
    fingerprint: &ModelFingerprint,
) -> Result<&'a [u8], NativeError> {
    if state.bytes.len() <= HEADER_BYTES || !state.bytes.starts_with(MAGIC) {
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

/// Check binding without modifying native state. Legacy blobs only authorize
/// token replay; a versioned envelope must match before any mutation.
pub(crate) fn validate_binding(
    state: &SequenceStateBlob,
    fingerprint: &ModelFingerprint,
) -> Result<(), NativeError> {
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
    if sequence_id < 0
        || sequence_id as u32 >= fingerprint.max_sequences
        || token_count == 0
        || token_count != token_ids.len()
        || token_count > fingerprint.context_tokens as usize
    {
        return Err(incompatible(
            "sequence export is outside the resident context",
        ));
    }
    let flags = LlamaStateSeqFlags::empty();
    let size = context.state_seq_get_size_ext(sequence_id, flags);
    if size == 0 {
        return Err(incompatible("llama.cpp reported an empty sequence state"));
    }
    let mut bytes = vec![0_u8; size];
    let written = context.state_seq_get_data_ext(&mut bytes, sequence_id, flags);
    if written != size {
        return Err(incompatible("llama.cpp sequence export size changed"));
    }
    let state = envelope(bytes, fingerprint, sequence_id, token_count, token_ids)?;
    LIVE_EXPORTS.with_borrow_mut(|receipts| {
        if receipts.len() == MAX_LIVE_RECEIPTS {
            receipts.pop_front();
        }
        receipts.push_back(receipt(&state));
    });
    Ok(state)
}

/// `false` requests token replay. It never authorizes the raw native parser.
pub(crate) fn import_sequence(
    context: &mut LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    state: &SequenceStateBlob,
    destination_sequence_id: i32,
) -> Result<bool, NativeError> {
    // Legacy serialized snapshots have no binding envelope and cannot enter
    // the native parser. Their already-validated token IDs remain replayable.
    if !state.bytes.starts_with(MAGIC) {
        return Ok(false);
    }
    let raw = validated_payload(state, fingerprint)?;
    if !LIVE_EXPORTS.with_borrow(|receipts| receipts.contains(&receipt(state))) {
        return Ok(false);
    }
    // SAFETY: the full state and token metadata match a SHA-256 receipt created
    // only after successful native export on this worker. The envelope binds
    // the full model/context fingerprint, checked above. Caller-constructed,
    // altered or receipt-expired bytes never reach this call. A serialized
    // round-trip on this same worker is allowed only while its receipt survives.
    let restored = unsafe {
        context.state_seq_set_data_ext(raw, destination_sequence_id, LlamaStateSeqFlags::empty())
    };
    if !restored {
        return Err(incompatible("llama.cpp rejected the sequence state"));
    }
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
        arbitrary.bytes = vec![1, 2, 3];
        assert!(validated_payload(&arbitrary, &original).is_err());
    }

    #[test]
    fn live_receipt_covers_every_caller_mutable_state_field() {
        let state = envelope(vec![1, 2, 3], &fingerprint(), 0, 1, vec![7]).expect("fixture");
        let original = receipt(&state);
        assert_eq!(original, receipt(&state.clone()));
        for field in 0..4 {
            let mut altered = state.clone();
            match field {
                0 => altered.sequence_id = 1,
                1 => altered.token_count = 2,
                2 => altered.token_ids[0] = 8,
                _ => altered.bytes[HEADER_BYTES] ^= 1,
            }
            assert_ne!(original, receipt(&altered));
        }
        // Serde retains reconstruction data, never creates a live receipt.
        let persisted = serde_json::to_vec(&state).expect("serialize");
        let decoded: SequenceStateBlob = serde_json::from_slice(&persisted).expect("decode");
        assert_eq!(state, decoded);
        assert!(!LIVE_EXPORTS.with_borrow(|receipts| receipts.contains(&receipt(&decoded))));
    }
}
