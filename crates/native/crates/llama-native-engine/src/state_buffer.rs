#![allow(unsafe_code)]

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::session::LlamaStateSeqFlags;
use llama_native_types::{ModelFingerprint, NativeError, NativeErrorCode, SequenceStateBlob};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAGIC: &[u8; 16] = b"native-seq-v1\0\0\0";
const HEADER_BYTES: usize = llama_native_types::SEQUENCE_STATE_HEADER_BYTES;
const MAX_LIVE_RECEIPTS: usize = 256;

const SIGNED_MAGIC: &[u8; 16] = b"native-seq-v2\0\0\0";
const SIGNATURE_BYTES: usize = 32;

/// An application-selected, private on-disk signing key. Only successful native
/// exports are signed. The signature binds all bytes, token IDs and the complete
/// model/build/context fingerprint; arbitrary serialized state is still refused.
pub(crate) struct PersistentAuthority([u8; 32]);
impl std::fmt::Debug for PersistentAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersistentAuthority([redacted])")
    }
}
impl PersistentAuthority {
    pub(crate) fn open(directory: &std::path::Path) -> Result<Arc<Self>, NativeError> {
        use std::io::{Read, Seek, Write};
        std::fs::create_dir_all(directory)
            .map_err(|e| incompatible(&format!("cache authority directory: {e}")))?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(directory.join("state-auth-v1.key"))
            .map_err(|e| incompatible(&format!("cache authority key: {e}")))?;
        file.lock()
            .map_err(|e| incompatible(&format!("cache authority lock: {e}")))?;
        let size = file
            .metadata()
            .map_err(|e| incompatible(&e.to_string()))?
            .len();
        let mut key = [0; 32];
        if size == 0 {
            getrandom::fill(&mut key)
                .map_err(|e| incompatible(&format!("cache authority entropy: {e}")))?;
            file.write_all(&key)
                .and_then(|()| file.sync_all())
                .map_err(|e| incompatible(&e.to_string()))?;
        } else {
            if size != 32 {
                return Err(incompatible("invalid cache authority key size"));
            }
            file.rewind()
                .and_then(|_| file.read_exact(&mut key))
                .map_err(|e| incompatible(&e.to_string()))?;
        }
        Ok(Arc::new(Self(key)))
    }

    fn mac(&self, state: &SequenceStateBlob, bytes: &[u8]) -> hmac::Hmac<Sha256> {
        use hmac::Mac;
        // HMAC accepts every key length; this key has exactly 256 random bits.
        let mut mac =
            hmac::Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC accepts a 32 byte key");
        mac.update(b"native-state-auth-v1");
        mac.update(&state.sequence_id.to_le_bytes());
        mac.update(&(state.token_count as u64).to_le_bytes());
        mac.update(&(state.token_ids.len() as u64).to_le_bytes());
        for token in &state.token_ids {
            mac.update(&token.to_le_bytes());
        }
        mac.update(bytes);
        mac
    }

    pub(crate) fn verifies(&self, state: &SequenceStateBlob) -> bool {
        use hmac::Mac;
        if !state.bytes.starts_with(SIGNED_MAGIC)
            || state.bytes.len() <= HEADER_BYTES + SIGNATURE_BYTES
        {
            return false;
        }
        let split = state.bytes.len() - SIGNATURE_BYTES;
        self.mac(state, &state.bytes[..split])
            .verify_slice(&state.bytes[split..])
            .is_ok()
    }
}

thread_local! {
    static PERSISTENT_AUTHORITY: RefCell<Option<Arc<PersistentAuthority>>> = const { RefCell::new(None) };
}
pub(crate) fn bind_persistent_authority(authority: Option<Arc<PersistentAuthority>>) {
    PERSISTENT_AUTHORITY.with_borrow_mut(|current| *current = authority);
}

// Shared only for a cheap storage-mode query. The native importer still checks
// the full exported bytes against this worker's receipt before parsing them.
pub(crate) type LiveExports = Arc<Mutex<VecDeque<[u8; 32]>>>;
thread_local! {
    static LIVE_EXPORTS: RefCell<LiveExports> = RefCell::new(Arc::new(Mutex::new(VecDeque::new())));
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

fn remember_export(receipt: [u8; 32]) {
    LIVE_EXPORTS.with_borrow(|exports| {
        let mut receipts = exports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if receipts.len() == MAX_LIVE_RECEIPTS {
            receipts.pop_front();
        }
        receipts.push_back(receipt);
    });
}

fn has_live_export(receipt: &[u8; 32]) -> bool {
    LIVE_EXPORTS.with_borrow(|exports| {
        exports
            .lock()
            .is_ok_and(|receipts| receipts.contains(receipt))
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

fn receipt(state: &SequenceStateBlob) -> [u8; 32] {
    state.export_receipt()
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
        sequence_id,
        token_count,
        bytes: bytes.into(),
        token_ids,
    })
}

fn validated_payload<'a>(
    state: &'a SequenceStateBlob,
    fingerprint: &ModelFingerprint,
) -> Result<&'a [u8], NativeError> {
    if state.bytes.len() < HEADER_BYTES
        || !(state.bytes.starts_with(MAGIC) || state.bytes.starts_with(SIGNED_MAGIC))
    {
        return Err(incompatible(
            "saved sequence has no supported context binding",
        ));
    }
    if state.bytes[MAGIC.len()..HEADER_BYTES] != fingerprint_digest(fingerprint)? {
        return Err(incompatible(
            "saved sequence belongs to a different model or context",
        ));
    }
    let end = if state.bytes.starts_with(SIGNED_MAGIC) {
        state
            .bytes
            .len()
            .checked_sub(SIGNATURE_BYTES)
            .filter(|end| *end >= HEADER_BYTES)
            .ok_or_else(|| incompatible("truncated authenticated native state"))?
    } else {
        state.bytes.len()
    };
    Ok(&state.bytes[HEADER_BYTES..end])
}

/// Check binding without modifying native state. Legacy blobs only authorize
/// token replay; a versioned envelope must match before any mutation.
pub(crate) fn validate_binding(
    state: &SequenceStateBlob,
    fingerprint: &ModelFingerprint,
) -> Result<(), NativeError> {
    if state.bytes.starts_with(MAGIC) || state.bytes.starts_with(SIGNED_MAGIC) {
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
    // Compact SWA exports discard keys needed after a longest-prefix rewind.
    let flags = LlamaStateSeqFlags::KEEP_ALL_SWA_CELLS;
    let size = context.state_seq_get_size_ext(sequence_id, flags);
    if size == 0 {
        return Err(incompatible("llama.cpp reported an empty sequence state"));
    }
    let payload_end = HEADER_BYTES
        .checked_add(size)
        .ok_or_else(|| incompatible("sequence size overflow"))?;
    let capacity = payload_end
        .checked_add(SIGNATURE_BYTES)
        .ok_or_else(|| incompatible("sequence size overflow"))?;
    if capacity > max_bytes {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| incompatible("insufficient memory for sequence export"))?;
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&fingerprint_digest(fingerprint)?);
    bytes.resize(payload_end, 0);
    // Write directly into the final envelope. A full SWA state can be gigabytes;
    // a second raw buffer would needlessly double the export's peak memory.
    let written = context.state_seq_get_data_ext(&mut bytes[HEADER_BYTES..], sequence_id, flags);
    if written != size {
        return Err(incompatible("llama.cpp sequence export size changed"));
    }
    let mut state = SequenceStateBlob {
        sequence_id,
        token_count,
        bytes: bytes.into(),
        token_ids,
    };
    PERSISTENT_AUTHORITY.with_borrow(|authority| {
        if let Some(authority) = authority {
            use hmac::Mac;
            Arc::make_mut(&mut state.bytes)[..MAGIC.len()].copy_from_slice(SIGNED_MAGIC);
            let signature = authority.mac(&state, &state.bytes).finalize().into_bytes();
            Arc::make_mut(&mut state.bytes).extend_from_slice(&signature);
        }
    });
    remember_export(receipt(&state));
    Ok(Some(state))
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
    if !(state.bytes.starts_with(MAGIC) || state.bytes.starts_with(SIGNED_MAGIC)) {
        return Ok(false);
    }
    let raw = validated_payload(state, fingerprint)?;
    let authenticated = PERSISTENT_AUTHORITY
        .with_borrow(|authority| authority.as_ref().is_some_and(|key| key.verifies(state)));
    if raw.is_empty() || !(authenticated || has_live_export(&receipt(state))) {
        return Ok(false);
    }
    // SAFETY: the full state and token metadata have either an authenticated
    // native-export signature or a same-worker SHA-256 receipt created
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

    struct TestTempDir(std::path::PathBuf);

    impl TestTempDir {
        fn new() -> Self {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "llama-native-state-buffer-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create temporary state directory");
            Self(path)
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

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
    fn live_receipts_expire_with_worker_and_bounded_registry_without_losing_replay() {
        let exports = LiveExports::default();
        let lifetime = bind_worker_exports(&exports);
        let state = envelope(vec![3; 4096], &fingerprint(), 0, 1, vec![7]).expect("state");
        let original = receipt(&state);
        remember_export(original);
        assert!(has_live_export(&original));
        let compact = state
            .reconstruction()
            .sequence(state.token_ids.clone())
            .expect("replay");
        assert_eq!(compact.bytes.len(), HEADER_BYTES);
        assert_eq!(compact.token_ids, state.token_ids);
        assert!(
            validated_payload(&compact, &fingerprint())
                .expect("binding")
                .is_empty()
        );
        assert_ne!(receipt(&compact), original);
        for index in 0..MAX_LIVE_RECEIPTS {
            let mut next = [0; 32];
            next[..8].copy_from_slice(&(index as u64).to_le_bytes());
            remember_export(next);
        }
        assert!(!has_live_export(&original));
        remember_export(original);
        drop(lifetime);
        assert!(exports.lock().expect("registry").is_empty());
        let replacement = LiveExports::default();
        let _replacement_lifetime = bind_worker_exports(&replacement);
        assert!(!has_live_export(&original));
        assert!(validated_payload(&compact, &fingerprint()).is_ok());
        let mut changed = fingerprint();
        changed.context_tokens += 1;
        assert!(validated_payload(&compact, &changed).is_err());
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
                _ => std::sync::Arc::make_mut(&mut altered.bytes)[HEADER_BYTES] ^= 1,
            }
            assert_ne!(original, receipt(&altered));
        }
        // Serde retains reconstruction data, never creates a live receipt.
        let persisted = serde_json::to_vec(&state).expect("serialize");
        let decoded: SequenceStateBlob = serde_json::from_slice(&persisted).expect("decode");
        assert_eq!(state, decoded);
        assert!(!has_live_export(&receipt(&decoded)));
    }

    fn authenticated_fixture(
        authority: &PersistentAuthority,
        fingerprint: &ModelFingerprint,
    ) -> SequenceStateBlob {
        use hmac::Mac;
        let mut state =
            envelope(vec![9, 8, 7, 6], fingerprint, 0, 2, vec![7, 8]).expect("fixture envelope");
        Arc::make_mut(&mut state.bytes)[..MAGIC.len()].copy_from_slice(SIGNED_MAGIC);
        let signature = authority.mac(&state, &state.bytes).finalize().into_bytes();
        Arc::make_mut(&mut state.bytes).extend_from_slice(&signature);
        state
    }

    #[test]
    fn persistent_authority_reopens_and_rejects_mutated_state() {
        let directory = TestTempDir::new();
        let fingerprint = fingerprint();
        let original = {
            let authority = PersistentAuthority::open(&directory.0).expect("authority");
            authenticated_fixture(&authority, &fingerprint)
        };

        let reopened = PersistentAuthority::open(&directory.0).expect("reopened authority");
        assert!(reopened.verifies(&original));
        assert_eq!(
            validated_payload(&original, &fingerprint).expect("bound payload"),
            &[9, 8, 7, 6]
        );

        let mut changed_bytes = original.clone();
        Arc::make_mut(&mut changed_bytes.bytes)[HEADER_BYTES] ^= 1;
        assert!(!reopened.verifies(&changed_bytes));

        let mut changed_tokens = original.clone();
        changed_tokens.token_ids[0] ^= 1;
        assert!(!reopened.verifies(&changed_tokens));

        let mut changed_sequence = original.clone();
        changed_sequence.sequence_id = 1;
        assert!(!reopened.verifies(&changed_sequence));

        let mut other_fingerprint = fingerprint;
        other_fingerprint.context_tokens += 1;
        assert!(validated_payload(&original, &other_fingerprint).is_err());
    }
}
