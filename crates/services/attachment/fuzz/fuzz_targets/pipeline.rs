#![no_main]

use attachment_native_host::{AttachmentHost, AttachmentHostConfig, ProvidedAttachment};
use attachment_native_types::{MediaFamily, TargetCapabilities};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;

const MAX_FUZZ_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|input: &[u8]| {
    let case = FuzzCase::decode(input);
    let mut config = AttachmentHostConfig::default();
    config.inspection.limits.max_root_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    config.inspection.limits.max_object_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    config.inspection.limits.max_parser_input_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    config.inspection.limits.max_container_metadata_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    config.inspection.limits.max_decoder_window_bytes = MAX_FUZZ_INPUT_BYTES as u64;
    config.inspection.limits.max_total_derived_bytes = 256 * 1024;
    config.inspection.limits.max_retained_bytes = 256 * 1024;
    config.inspection.limits.max_objects = 128;
    config.inspection.limits.max_edges = 256;
    config.inspection.limits.max_entries = 256;
    config.inspection.limits.max_depth = 4;
    config.inspection.limits.max_name_bytes = 512;
    config.inspection.limits.max_declared_to_actual_ratio = 64;
    config.inspection.limits.max_text_bytes = 128 * 1024;
    config.inspection.limits.max_media_objects = 32;
    config.inspection.limits.max_media_bytes = 128 * 1024;
    config.inspection.limits.max_image_pixels = 4_000_000;
    config.inspection.limits.max_transform_requests = 32;
    config.inspection.limits.deadline_ms = 500;
    config.documents.max_processor_input_bytes = MAX_FUZZ_INPUT_BYTES;
    config.documents.max_pdf_input_bytes = MAX_FUZZ_INPUT_BYTES;
    config.documents.max_pdf_pages = 32;
    config.documents.max_pdf_page_decompressed_bytes = 128 * 1024;
    config.documents.max_xml_events = 16_384;
    config.documents.max_xml_depth = 64;
    config.documents.max_csv_rows = 2_048;
    config.documents.max_csv_cells = 16_384;
    config.documents.max_notebook_cells = 1_024;
    config.documents.max_html_depth = 64;
    config.documents.max_segments = 4_096;

    let mut accepted_media_types = BTreeSet::new();
    if case.control & 1 != 0 {
        accepted_media_types.insert("image/png".to_string());
    }
    if case.control & 2 != 0 {
        accepted_media_types.insert("audio/wav".to_string());
    }
    if case.control & 4 != 0 {
        accepted_media_types.insert("video/mp4".to_string());
    }
    let mut accepted_media_families = BTreeSet::new();
    if case.control & 8 != 0 {
        accepted_media_families.insert(MediaFamily::Image);
    }
    if case.control & 16 != 0 {
        accepted_media_families.insert(MediaFamily::Audio);
    }
    if case.control & 32 != 0 {
        accepted_media_families.insert(MediaFamily::Video);
    }
    let target = TargetCapabilities {
        target_id: "fuzz-target".to_string(),
        fingerprint: format!("fuzz:{:02x}", case.control),
        accepted_media_types,
        accepted_media_families,
        max_media_objects: 16,
        max_media_bytes: 128 * 1024,
        max_text_bytes: 128 * 1024,
        supports_markdown: case.control & 64 != 0,
        supports_native_pdf: case.control & 128 != 0,
        supports_native_video: case.control & 4 != 0,
    };

    let Ok(host) = AttachmentHost::new(config) else {
        return;
    };
    let attachment = ProvidedAttachment::from_bytes(case.name, case.media_type, case.bytes);
    if let Ok(output) = host.process(attachment, &target) {
        assert!(output.bundle.validate().is_ok());
        assert!(output.plan.validate().is_ok());
        assert!(!output.receipt.network_used);
        assert!(!output.receipt.process_used);
        assert!(!output.receipt.model_invoked);
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
