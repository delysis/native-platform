#![no_main]

use attachment_native_inspect::{Inspector, ProvidedAttachment};
use attachment_native_types::{ArchivePathPolicy, InspectionPolicy, UnknownBinaryPolicy};
use libfuzzer_sys::fuzz_target;

const MAX_FUZZ_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|input: &[u8]| {
    let case = FuzzCase::decode(input);
    let mut policy = InspectionPolicy::default();
    policy.limits.max_root_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    policy.limits.max_object_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    policy.limits.max_parser_input_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    policy.limits.max_container_metadata_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    policy.limits.max_decoder_window_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    policy.limits.max_total_derived_bytes = 256 * 1024;
    policy.limits.max_retained_bytes = 256 * 1024;
    policy.limits.max_objects = 128;
    policy.limits.max_edges = 256;
    policy.limits.max_entries = 256;
    policy.limits.max_depth = 4;
    policy.limits.max_name_bytes = 512;
    policy.limits.max_declared_to_actual_ratio = 64;
    policy.limits.max_text_bytes = 128 * 1024;
    policy.limits.max_media_objects = 32;
    policy.limits.max_media_bytes = 128 * 1024;
    policy.limits.max_image_pixels = 4_000_000;
    policy.limits.max_transform_requests = 32;
    policy.limits.deadline_ms = 500;
    policy.unknown_binary = if case.control & 1 == 0 {
        UnknownBinaryPolicy::RecordOpaque
    } else {
        UnknownBinaryPolicy::Reject
    };
    policy.path_policy = if case.control & 2 == 0 {
        ArchivePathPolicy::SanitizeAndScan
    } else {
        ArchivePathPolicy::RejectEntry
    };
    policy.analyze_duplicate_content_once = case.control & 4 == 0;
    policy.continue_after_child_error = case.control & 8 == 0;
    policy.inspect_pdf_embedded_files = case.control & 16 == 0;

    let Ok(inspector) = Inspector::new(policy) else {
        return;
    };
    let attachment = ProvidedAttachment::from_bytes(case.name, case.media_type, case.bytes);
    if let Ok(bundle) = inspector.inspect(attachment) {
        assert!(bundle.validate().is_ok());
    }
});

struct FuzzCase {
    control: u8,
    name: String,
    media_type: Option<String>,
    bytes: Vec<u8>,
}

impl FuzzCase {
    fn decode(input: &[u8]) -> Self {
        let control = input.first().copied().unwrap_or_default();
        let name_len = input.get(1).copied().unwrap_or_default() as usize;
        let media_len = input.get(2).copied().unwrap_or_default() as usize;
        let mut cursor = 3usize.min(input.len());
        let name_end = cursor.saturating_add(name_len).min(input.len());
        let name = String::from_utf8_lossy(&input[cursor..name_end]).into_owned();
        cursor = name_end;
        let media_end = cursor.saturating_add(media_len).min(input.len());
        let media = String::from_utf8_lossy(&input[cursor..media_end]).into_owned();
        cursor = media_end;
        let bytes_end = cursor.saturating_add(MAX_FUZZ_INPUT_BYTES).min(input.len());
        Self {
            control,
            name,
            media_type: (!media.is_empty()).then_some(media),
            bytes: input[cursor..bytes_end].to_vec(),
        }
    }
}
