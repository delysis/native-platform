//! Native media from the admitted source and explicitly resolved documents.
//! Ordinary links never grant file access; only retained attachment identities
//! recognized by the existing context registry supply bytes.

use std::collections::HashSet;

use llama_native_types::MediaInput;
use loom_store::{LoadedDocument, ProjectStore};

use super::IpcFailure;
use super::context_attachments::{resolve_inline_media, resolve_media_for_document};
use super::document_bindings::ResolvedDocument;

const MAX_MEDIA: usize = 32;
const MAX_MEDIA_BYTES: usize = 128 * 1024 * 1024;

pub(super) fn resolve(
    store: &ProjectStore,
    source: &LoadedDocument,
    references: &[ResolvedDocument],
    _context_tokens: u32,
) -> Result<Vec<MediaInput>, IpcFailure> {
    if references.len() > 32 {
        return Err(limit());
    }
    let mut media =
        resolve_media_for_document(store.root(), &source.document_id.to_string(), &source.text)
            .map_err(|error| {
                IpcFailure::new("terminal_media_unavailable", error.to_string(), false)
            })?;
    // Source context is explicitly selected for this run. Referenced documents
    // contribute their visible media only, without their private scratch cards.
    let mut seen = HashSet::new();
    media.retain(|item| seen.insert((item.kind, item.sha256.clone())));
    append_references(store, references, &mut media)?;
    Ok(media)
}

pub(super) fn append_references(
    store: &ProjectStore,
    references: &[ResolvedDocument],
    media: &mut Vec<MediaInput>,
) -> Result<(), IpcFailure> {
    if references.len() > 32 {
        return Err(limit());
    }
    let mut seen_documents = HashSet::new();
    let mut seen = media
        .iter()
        .map(|item| (item.kind, item.sha256.clone()))
        .collect::<HashSet<_>>();
    let mut total_bytes = media.iter().map(|item| item.bytes.len()).sum::<usize>();
    if media.len() > MAX_MEDIA || total_bytes > MAX_MEDIA_BYTES {
        return Err(limit());
    }
    for document in references {
        if !seen_documents.insert(document.document_id) {
            continue;
        }
        let resolved = resolve_inline_media(
            store.root(),
            &document.document_id.to_string(),
            &document.text,
        )
        .map_err(|error| IpcFailure::new("terminal_media_unavailable", error.to_string(), false))?;
        for item in resolved {
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
    Ok(())
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
    use crate::context_attachments::{import_recorded_wav, set_document_context_snapshot};
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
        let private = import_recorded_wav(store.root(), "private.wav".into(), &wav(7))
            .expect("private scratch audio");
        set_document_context_snapshot(
            store.root(),
            &references[0].document_id.to_string(),
            "Private scratch context is not part of this reference.",
            &[private.id],
        )
        .expect("private referenced context");
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
