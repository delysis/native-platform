//! Native media from the admitted source and explicitly resolved documents.
//! Ordinary links never grant file access; only retained attachment identities
//! recognized by the existing context registry supply bytes.

use std::collections::HashSet;

use llama_native_types::MediaInput;
use loom_store::{LoadedDocument, ProjectStore};

use super::IpcFailure;
use super::context_attachments::resolve_for_generation_with_budget;
use super::document_bindings::ResolvedDocument;

const MAX_MEDIA: usize = 32;
const MAX_MEDIA_BYTES: usize = 128 * 1024 * 1024;

pub(super) fn resolve(
    store: &ProjectStore,
    source: &LoadedDocument,
    references: &[ResolvedDocument],
    context_tokens: u32,
) -> Result<Vec<MediaInput>, IpcFailure> {
    if references.len() > 32 {
        return Err(limit());
    }
    let documents = std::iter::once((source.document_id, source.text.as_str())).chain(
        references
            .iter()
            .map(|document| (document.document_id, document.text.as_str())),
    );
    let mut media = Vec::new();
    let mut seen_documents = HashSet::new();
    let mut seen = HashSet::new();
    let mut total_bytes = 0_usize;
    for (document_id, text) in documents {
        if !seen_documents.insert(document_id) {
            continue;
        }
        // Deliberately discard the contextual prose and manuscript window:
        // terminal prompts already bind their complete explicit input bytes.
        let resolved = resolve_for_generation_with_budget(
            store.root(),
            &document_id.to_string(),
            text,
            context_tokens,
            1,
            0,
        )
        .map_err(|error| IpcFailure::new("terminal_media_unavailable", error.to_string(), false))?;
        for item in resolved.media {
            if !seen.insert((item.kind, item.sha256.clone())) {
                continue;
            }
            total_bytes = total_bytes
                .checked_add(item.bytes.len())
                .ok_or_else(limit)?;
            if media.len() >= MAX_MEDIA || total_bytes > MAX_MEDIA_BYTES {
                return Err(limit());
            }
            media.push(item);
        }
    }
    Ok(media)
}

fn limit() -> IpcFailure {
    IpcFailure::new(
        "terminal_media_limit",
        "Use at most 32 referenced documents and 32 distinct media inputs totaling at most 128 MiB.",
        false,
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Cursor;

    use loom_document::DocumentContent;

    use super::*;
    use crate::context_attachments::import_recorded_wav;
    use crate::document_bindings::resolve_references;

    fn wav(sample: i16) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(
            &mut bytes,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .expect("WAV");
        writer.write_sample(sample).expect("sample");
        writer.finalize().expect("finished WAV");
        bytes.into_inner()
    }

    #[test]
    fn retained_source_and_reference_audio_are_ordered_and_links_grant_no_access() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").expect("project");
        let first_bytes = wav(42);
        let second_bytes = wav(-42);
        let first = import_recorded_wav(store.root(), "first.wav".into(), &first_bytes)
            .expect("retained source audio");
        let second = import_recorded_wav(store.root(), "second.wav".into(), &second_bytes)
            .expect("retained reference audio");
        for (path, text) in [
            (
                "source.md",
                format!(
                    "{}\n[unretained](not-present.wav)\n@unread",
                    first.inline_markdown
                ),
            ),
            (
                "reference.md",
                format!("{}\n{}", second.inline_markdown, first.inline_markdown),
            ),
            (
                "unread.md",
                "![not an authorized attachment](file:///private/secret.wav)".into(),
            ),
        ] {
            store
                .create_document_if_absent(path, DocumentContent::Prose(text), "fixture")
                .expect("document");
        }
        let source = store.read_document("source.md").expect("source");
        let references =
            resolve_references(&store, &["reference".into()]).expect("explicit reference");
        let media = resolve(&store, &source, &references, 32_768).expect("native media");
        assert_eq!(media.len(), 2);
        assert_eq!(media[0].bytes, first_bytes);
        assert_eq!(media[1].bytes, second_bytes);
        assert_eq!(
            resolve(&store, &source, &[], 32_768)
                .expect("source only")
                .len(),
            1
        );
    }
}
