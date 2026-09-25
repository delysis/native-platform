//! Native media from the admitted source and explicitly resolved documents.
//! Ordinary links never grant file access; only retained attachment identities
//! recognized by the existing context registry supply bytes.

use std::collections::HashSet;

use llama_native_types::MediaInput;
use llama_native_types::media_identity::{MediaAdmission, MediaIdentityLedger};
#[cfg(test)]
use llama_native_types::MediaKind;
use loom_store::{LoadedDocument, ProjectStore};
#[cfg(test)]
use sha2::{Digest as _, Sha256};

use super::IpcFailure;
use super::context_attachments::resolve_media_for_document;
use super::document_bindings::ResolvedDocument;


/// The sole ordered media admission/budget implementation for source documents,
/// references and terminal/context composition. Payload sharing never changes
/// the source occurrences retained by the caller's document/evidence bindings.
#[derive(Default)]
pub(super) struct MediaAccumulator {
    items: Vec<MediaInput>,
    identities: MediaIdentityLedger,
}

impl MediaAccumulator {
    pub(super) fn extend(
        &mut self,
        media: impl IntoIterator<Item = MediaInput>,
    ) -> Result<(), IpcFailure> {
        for item in media {
            self.push(item)?;
        }
        Ok(())
    }

    fn push(&mut self, item: MediaInput) -> Result<(), IpcFailure> {
        let admission = self.identities.admit(&item).map_err(|error| {
            if error.is_limit() { limit() } else { invalid(&error.to_string()) }
        })?;
        if admission == MediaAdmission::NewPayload {
            self.items.push(item);
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Vec<MediaInput> {
        self.items
    }
}

pub(super) fn resolve(
    store: &ProjectStore,
    source: &LoadedDocument,
    references: &[ResolvedDocument],
    _context_tokens: u32,
) -> Result<Vec<MediaInput>, IpcFailure> {
    if references.len() > 32 {
        return Err(limit());
    }
    let documents = std::iter::once((source.document_id, source.text.as_str())).chain(
        references
            .iter()
            .map(|document| (document.document_id, document.text.as_str())),
    );
    let mut media = MediaAccumulator::default();
    let mut seen_documents = HashSet::new();
    for (document_id, text) in documents {
        if !seen_documents.insert(document_id) {
            continue;
        }
        let resolved = resolve_media_for_document(store.root(), &document_id.to_string(), text)
            .map_err(|error| {
                IpcFailure::new("terminal_media_unavailable", error.to_string(), false)
            })?;
        media.extend(resolved)?;
    }
    Ok(media.finish())
}

/// Apply the same aggregate budget after combining independently resolved inputs.
pub(super) fn merge(
    first: Vec<MediaInput>,
    second: Vec<MediaInput>,
) -> Result<Vec<MediaInput>, IpcFailure> {
    let mut media = MediaAccumulator::default();
    media.extend(first)?;
    media.extend(second)?;
    Ok(media.finish())
}

fn invalid(message: &str) -> IpcFailure {
    IpcFailure::new("terminal_media_identity_conflict", message, false)
}

fn limit() -> IpcFailure {
    IpcFailure::new(
        "terminal_media_limit",
        "Use at most 32 referenced documents and 32 distinct media inputs totaling at most 128 MiB.",
        false,
    )
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    fn item(id: &str, bytes: &[u8]) -> MediaInput {
        MediaInput {
            id: id.into(), kind: MediaKind::Audio, mime: "audio/wav".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)), bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn equal_payloads_share_bytes_in_first_occurrence_order() {
        let first = item("first", b"one");
        let second = item("second", b"two");
        let duplicate = item("another-occurrence", b"one");
        assert_eq!(merge(vec![first.clone()], vec![second.clone(), duplicate]).unwrap(), vec![first, second]);
    }

    #[test]
    fn a_duplicate_claim_cannot_hide_corrupt_bytes() {
        let first = item("first", b"one");
        let mut corrupt = first.clone();
        corrupt.id = "another-occurrence".into();
        corrupt.bytes = b"two".to_vec();
        assert!(merge(vec![first], vec![corrupt]).is_err());
    }

    #[test]
    fn repeated_ids_cannot_rebind_payloads() {
        assert!(merge(vec![item("same", b"one")], vec![item("same", b"two")]).is_err());
    }

    #[test]
    fn duplicate_payloads_cannot_rebind_mime() {
        let first = item("first", b"one");
        let mut changed = item("second", b"one");
        changed.mime = "image/png".into();
        assert!(merge(vec![first], vec![changed]).is_err());
    }

    #[test]
    fn same_bytes_of_different_kinds_are_not_silently_deduplicated() {
        let audio = item("audio", b"one");
        let mut image = item("image", b"one");
        image.kind = MediaKind::Image;
        image.mime = "image/png".into();
        assert_eq!(merge(vec![audio], vec![image]).unwrap().len(), 2);
        // Codec/capability admission still belongs to Attachment and Native;
        // this accumulator does not call arbitrary bytes a decoded image.
    }

    #[test]
    fn count_limit_applies_across_independently_resolved_documents() {
        let first = (0_u8..16).map(|index| item(&index.to_string(), &[index])).collect();
        let second = (16_u8..33).map(|index| item(&index.to_string(), &[index])).collect();
        assert!(merge(first, second).is_err());
    }

    #[test]
    fn invalid_occurrence_does_not_mutate_the_accumulator() {
        let first = item("first", b"one");
        let mut media = MediaAccumulator::default();
        media.extend([first.clone()]).unwrap();
        assert!(media.extend([item("first", b"two")]).is_err());
        assert_eq!(media.finish(), vec![first]);
    }
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
