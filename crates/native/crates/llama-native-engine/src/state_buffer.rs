#![allow(unsafe_code)]

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::session::LlamaStateSeqFlags;
use llama_native_types::{NativeError, NativeErrorCode, SequenceStateBlob};

pub(crate) fn export_sequence(
    context: &LlamaContext<'_>,
    sequence_id: i32,
    token_count: usize,
    token_ids: Vec<i32>,
) -> Result<SequenceStateBlob, NativeError> {
    let flags = LlamaStateSeqFlags::empty();
    let size = context.state_seq_get_size_ext(sequence_id, flags);
    if size == 0 {
        return Err(NativeError::new(
            NativeErrorCode::Internal,
            format!("llama.cpp reported an empty state for sequence {sequence_id}"),
        ));
    }
    let mut bytes = vec![0_u8; size];
    // SAFETY: `bytes` is allocated to the exact size reported by llama.cpp for this
    // context and sequence, and the context remains exclusively owned by the worker.
    let written = unsafe { context.state_seq_get_data_ext(bytes.as_mut_ptr(), sequence_id, flags) };
    if written != size {
        return Err(NativeError::new(
            NativeErrorCode::Internal,
            format!("llama.cpp wrote {written} state bytes after reporting {size}"),
        ));
    }
    Ok(SequenceStateBlob {
        sequence_id,
        token_count,
        bytes,
        token_ids,
    })
}

pub(crate) fn import_sequence(
    context: &mut LlamaContext<'_>,
    state: &SequenceStateBlob,
    destination_sequence_id: i32,
) -> Result<(), NativeError> {
    if state.bytes.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::CacheIncompatible,
            "sequence state is empty",
        ));
    }
    // SAFETY: state bytes originate from `export_sequence` for a fingerprint-checked
    // model/context configuration. The context is exclusively owned by this worker.
    let restored = unsafe {
        context.state_seq_set_data_ext(
            state.bytes.as_slice(),
            destination_sequence_id,
            LlamaStateSeqFlags::empty(),
        )
    };
    if !restored {
        return Err(NativeError::new(
            NativeErrorCode::CacheIncompatible,
            "llama.cpp rejected the sequence state",
        ));
    }
    Ok(())
}
