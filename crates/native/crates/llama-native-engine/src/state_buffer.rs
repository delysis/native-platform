#![allow(unsafe_code)]

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::session::LlamaStateSeqFlags;
use llama_native_types::{
    ModelFingerprint, NativeError, NativeErrorCode, SequenceStateBlob, SequenceStateCell,
};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

const MAGIC: &[u8; 16] = b"native-seq-v1\0\0\0";
const HEADER_BYTES: usize = llama_native_types::SEQUENCE_STATE_HEADER_BYTES;
const MAX_LIVE_RECEIPTS: usize = 256;

const SIGNED_MAGIC: &[u8; 16] = b"native-seq-v2\0\0\0";
const SIGNATURE_BYTES: usize = 32;

/// An application-selected, private on-disk signing key. Only successful native
/// exports are signed. The signature binds all bytes, token IDs and the complete
/// model/build/context fingerprint; arbitrary serialized state is still refused.
pub(crate) struct PersistentAuthority {
    key: [u8; 32],
    verified: Mutex<VecDeque<VerifiedAllocation>>,
}

const MAX_VERIFIED_ALLOCATIONS: usize = 64;

struct VerifiedAllocation {
    bytes: Weak<Vec<u8>>,
    metadata_mac: [u8; 32],
}
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
        Ok(Arc::new(Self {
            key,
            verified: Mutex::new(VecDeque::new()),
        }))
    }

    fn mac(&self, state: &SequenceStateBlob, bytes: &[u8]) -> hmac::Hmac<Sha256> {
        use hmac::Mac;
        // HMAC accepts every key length; this key has exactly 256 random bits.
        let mut mac =
            hmac::Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts a 32 byte key");
        mac.update(b"native-state-auth-v1");
        mac.update(&state.sequence_id.to_le_bytes());
        mac.update(&(state.token_count as u64).to_le_bytes());
        mac.update(&(state.token_ids.len() as u64).to_le_bytes());
        for token in &state.token_ids {
            mac.update(&token.to_le_bytes());
        }
        if !state.cells.is_empty() {
            mac.update(b"native-state-auth-cells-v1");
            mac.update(&(state.cells.len() as u64).to_le_bytes());
            for cell in &state.cells {
                match cell {
                    SequenceStateCell::Text(token) => {
                        mac.update(&[0]);
                        mac.update(&token.to_le_bytes());
                    }
                    SequenceStateCell::Media {
                        digest,
                        chunk,
                        offset,
                    } => {
                        mac.update(&[1]);
                        mac.update(digest);
                        mac.update(&(*chunk as u64).to_le_bytes());
                        mac.update(&(*offset as u64).to_le_bytes());
                    }
                    SequenceStateCell::AudioFrame { digest } => {
                        mac.update(&[2]);
                        mac.update(digest);
                    }
                }
            }
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
        // The bytes are immutable while shared. A Weak keeps the allocation's
        // identity alive without retaining its multi-gigabyte payload; safe
        // Arc mutation must detach it. Metadata lives in separately mutable
        // vectors, so authenticate that small envelope on every lookup.
        let metadata_mac = self.mac(state, &[]).finalize().into_bytes().into();
        if self.verified_allocation(state, &metadata_mac) {
            return true;
        }
        let split = state.bytes.len() - SIGNATURE_BYTES;
        let valid = self
            .mac(state, &state.bytes[..split])
            .verify_slice(&state.bytes[split..])
            .is_ok();
        if valid {
            self.remember_verified(state, metadata_mac);
        }
        valid
    }

    fn verified_allocation(&self, state: &SequenceStateBlob, metadata_mac: &[u8; 32]) -> bool {
        let Ok(mut verified) = self.verified.lock() else {
            return false;
        };
        verified.retain(|entry| entry.bytes.strong_count() > 0);
        verified.iter().any(|entry| {
            &entry.metadata_mac == metadata_mac
                && entry
                    .bytes
                    .upgrade()
                    .is_some_and(|bytes| Arc::ptr_eq(&bytes, &state.bytes))
        })
    }

    fn remember_verified(&self, state: &SequenceStateBlob, metadata_mac: [u8; 32]) {
        let Ok(mut verified) = self.verified.lock() else {
            return;
        };
        verified.retain(|entry| entry.bytes.strong_count() > 0);
        if verified.len() >= MAX_VERIFIED_ALLOCATIONS {
            verified.pop_front();
        }
        verified.push_back(VerifiedAllocation {
            bytes: Arc::downgrade(&state.bytes),
            metadata_mac,
        });
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
    validate_cell_metadata(state)?;
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
    )
}

/// Export a complete mixed text/media lane while publishing its identity as
/// the ordinary cache sequence. This allows the resident media lane (seq 3)
/// to be restored into the cache's external seq 0 identity after a restart.
pub(crate) fn export_sequence_cells_bounded(
    context: &LlamaContext<'_>,
    fingerprint: &ModelFingerprint,
    source_sequence_id: i32,
    export_sequence_id: i32,
    cells: Vec<SequenceStateCell>,
    max_bytes: usize,
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
) -> Result<Option<SequenceStateBlob>, NativeError> {
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
    let written =
        context.state_seq_get_data_ext(&mut bytes[HEADER_BYTES..], source_sequence_id, flags);
    if written != size {
        return Err(incompatible("llama.cpp sequence export size changed"));
    }
    let mut state = SequenceStateBlob {
        sequence_id: export_sequence_id,
        token_count,
        bytes: bytes.into(),
        token_ids,
        cells,
    };
    PERSISTENT_AUTHORITY.with_borrow(|authority| {
        if let Some(authority) = authority {
            use hmac::Mac;
            Arc::make_mut(&mut state.bytes)[..MAGIC.len()].copy_from_slice(SIGNED_MAGIC);
            let signature = authority.mac(&state, &state.bytes).finalize().into_bytes();
            Arc::make_mut(&mut state.bytes).extend_from_slice(&signature);
            let metadata_mac = authority.mac(&state, &[]).finalize().into_bytes().into();
            authority.remember_verified(&state, metadata_mac);
        }
    });
    remember_export(receipt(&state));
    Ok(Some(state))
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
        // SAFETY: construction checks a native export signature or a live
        // worker receipt and the complete model/context binding. The borrowed
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
    if !(state.bytes.starts_with(MAGIC) || state.bytes.starts_with(SIGNED_MAGIC)) {
        return Ok(None);
    }
    let raw = validated_payload(state, fingerprint)?;
    let authenticated = PERSISTENT_AUTHORITY
        .with_borrow(|authority| authority.as_ref().is_some_and(|key| key.verifies(state)));
    if raw.is_empty() || !(authenticated || has_live_export(&receipt(state))) {
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

    struct TestTempDir(std::path::PathBuf);

    impl TestTempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};

            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            loop {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "llama-native-state-buffer-{}-{id}",
                    std::process::id()
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create temporary state directory: {error}"),
                }
            }
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

    fn authenticated_mixed_fixture(
        authority: &PersistentAuthority,
        fingerprint: &ModelFingerprint,
    ) -> SequenceStateBlob {
        use hmac::Mac;
        let mut state = envelope(vec![9, 8, 7, 6], fingerprint, 0, 3, vec![7]).expect("envelope");
        state.token_count = 3;
        state.cells = vec![
            SequenceStateCell::Text(7),
            SequenceStateCell::Media {
                digest: [0x11; 32],
                chunk: 2,
                offset: 640,
            },
            SequenceStateCell::AudioFrame { digest: [0x22; 32] },
        ];
        Arc::make_mut(&mut state.bytes)[..MAGIC.len()].copy_from_slice(SIGNED_MAGIC);
        let signature = authority.mac(&state, &state.bytes).finalize().into_bytes();
        Arc::make_mut(&mut state.bytes).extend_from_slice(&signature);
        state
    }

    #[test]
    fn persistent_authority_binds_mixed_cells_across_reopen() {
        let directory = TestTempDir::new();
        let fingerprint = fingerprint();
        let original = {
            let authority = PersistentAuthority::open(&directory.0).expect("authority");
            authenticated_mixed_fixture(&authority, &fingerprint)
        };
        assert!(validate_cell_metadata(&original).is_ok());
        let original_receipt = receipt(&original);

        let reopened = PersistentAuthority::open(&directory.0).expect("reopened authority");
        assert!(reopened.verifies(&original));
        let persisted = serde_json::to_vec(&original).expect("serialize mixed export");
        let reopened_state: SequenceStateBlob =
            serde_json::from_slice(&persisted).expect("decode mixed export");
        assert!(reopened.verifies(&reopened_state));
        assert!(
            reopened_state
                .reconstruction()
                .sequence(reopened_state.token_ids.clone())
                .is_none()
        );
        let mut changed = original.clone();
        changed.cells[1] = SequenceStateCell::Media {
            digest: [0x33; 32],
            chunk: 2,
            offset: 640,
        };
        assert_ne!(receipt(&changed), original_receipt);
        assert!(!reopened.verifies(&changed));
        assert!(validate_cell_metadata(&changed).is_ok());

        let mut malformed = original;
        malformed.cells[0] = SequenceStateCell::Text(99);
        assert!(validate_cell_metadata(&malformed).is_err());
    }

    #[test]
    fn verified_allocations_bind_bytes_metadata_and_authority() {
        use hmac::Mac;
        let directory = TestTempDir::new();
        let authority = PersistentAuthority::open(&directory.0).expect("authority");
        let original = authenticated_mixed_fixture(&authority, &fingerprint());
        let metadata_mac = authority.mac(&original, &[]).finalize().into_bytes().into();
        assert!(!authority.verified_allocation(&original, &metadata_mac));
        assert!(authority.verifies(&original));
        assert!(authority.verified_allocation(&original.clone(), &metadata_mac));

        let mut changed = original.clone();
        changed.cells[1] = SequenceStateCell::Media {
            digest: [0x55; 32],
            chunk: 2,
            offset: 640,
        };
        assert!(!authority.verifies(&changed));
        changed = original.clone();
        changed.token_count += 1;
        assert!(!authority.verifies(&changed));
        changed = original.clone();
        Arc::make_mut(&mut changed.bytes)[HEADER_BYTES] ^= 1;
        assert!(!authority.verified_allocation(&changed, &metadata_mac));
        assert!(!authority.verifies(&changed));

        let mut copied = original.clone();
        copied.bytes = Arc::new(original.bytes.as_ref().clone());
        assert!(!authority.verified_allocation(&copied, &metadata_mac));
        assert!(authority.verifies(&copied));
        let other_directory = TestTempDir::new();
        let other = PersistentAuthority::open(&other_directory.0).expect("other authority");
        assert!(!other.verifies(&original));
    }

    #[test]
    fn verified_allocations_are_bounded_and_do_not_retain_payloads() {
        let directory = TestTempDir::new();
        let authority = PersistentAuthority::open(&directory.0).expect("authority");
        let mut retained = Vec::new();
        for _ in 0..MAX_VERIFIED_ALLOCATIONS + 1 {
            let state = authenticated_fixture(&authority, &fingerprint());
            assert!(authority.verifies(&state));
            retained.push(state);
        }
        assert_eq!(
            authority.verified.lock().unwrap().len(),
            MAX_VERIFIED_ALLOCATIONS
        );
        let weak = Arc::downgrade(&retained[1].bytes);
        retained.clear();
        assert!(weak.upgrade().is_none());
        let state = authenticated_fixture(&authority, &fingerprint());
        assert!(authority.verifies(&state));
        assert_eq!(authority.verified.lock().unwrap().len(), 1);

        // Even with a sole strong owner, the cached Weak prevents mutation
        // in place. make_mut must detach before exposing mutable bytes.
        let mut state = state;
        assert!(Arc::get_mut(&mut state.bytes).is_none());
        Arc::make_mut(&mut state.bytes)[HEADER_BYTES] ^= 1;
        assert!(!authority.verifies(&state));
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
