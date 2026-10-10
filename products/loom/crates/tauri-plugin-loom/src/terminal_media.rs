//! Native media from the admitted source and explicitly resolved documents.
//! Ordinary links never grant file access; only retained attachment identities
//! recognized by the existing context registry supply bytes.

use std::collections::HashSet;

use llama_native_types::MediaInput;
#[cfg(test)]
use llama_native_types::MediaKind;
use llama_native_types::media_identity::{
    MAX_MEDIA_PAYLOADS, MediaAdmission, MediaIdentityError, MediaIdentityLedger,
};
use loom_store::{LoadedDocument, ProjectStore};
use loom_types::BlobId;
use serde::{Deserialize, Serialize};
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
            if error.is_limit() {
                limit()
            } else {
                invalid(&error.to_string())
            }
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

pub(super) const MAX_MEDIA: usize = MAX_MEDIA_PAYLOADS;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RetainedMedia {
    pub id: String,
    pub kind: llama_native_types::MediaKind,
    pub mime: String,
    /// Content address of the exact bytes supplied to native inference.
    pub bytes_blob_id: BlobId,
}

/// Snapshot native inputs into the run's store, independently of source-store
/// lifetime. Validate the whole batch before publishing any input blobs.
pub(super) fn retain(
    store: &mut ProjectStore,
    media: &[MediaInput],
) -> Result<Vec<RetainedMedia>, IpcFailure> {
    if media.len() > MAX_MEDIA {
        return Err(limit());
    }
    let mut identities = MediaIdentityLedger::default();
    for item in media {
        identities.admit(item).map_err(|error| match error {
            MediaIdentityError::Digest => IpcFailure::new(
                "media_identity_mismatch",
                "The attached media bytes do not match their admitted identity.",
                false,
            ),
            error if error.is_limit() => limit(),
            error => invalid(&error.to_string()),
        })?;
    }
    media
        .iter()
        .map(|item| {
            let bytes_blob_id = store
                .store_provenance_blob(&item.bytes)
                .map_err(IpcFailure::store)?;
            Ok(RetainedMedia {
                id: item.id.clone(),
                kind: item.kind,
                mime: item.mime.clone(),
                bytes_blob_id,
            })
        })
        .collect()
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
            id: id.into(),
            kind: MediaKind::Audio,
            mime: "audio/wav".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn equal_payloads_share_bytes_in_first_occurrence_order() {
        let first = item("first", b"one");
        let second = item("second", b"two");
        let duplicate = item("another-occurrence", b"one");
        assert_eq!(
            merge(vec![first.clone()], vec![second.clone(), duplicate]).unwrap(),
            vec![first, second]
        );
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
        let first = (0_u8..16)
            .map(|index| item(&index.to_string(), &[index]))
            .collect();
        let second = (16_u8..33)
            .map(|index| item(&index.to_string(), &[index]))
            .collect();
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
    fn run_media_bytes_survive_original_source_store_removal() {
        let owner_directory = tempfile::tempdir().expect("source directory");
        let (owner, _) = ProjectStore::initialize(owner_directory.path().join("Owner"), "Owner")
            .expect("owner store");
        let bytes = wav(42);
        let source =
            import_recorded_wav(owner.root(), "voice.wav".into(), &bytes).expect("source audio");
        let media = crate::context_attachments::source_native_media(owner.root(), &source.id)
            .expect("admitted media");
        assert_eq!(media.len(), 1);
        let destination_directory = tempfile::tempdir().expect("run directory");
        let (mut destination, _) =
            ProjectStore::initialize(destination_directory.path().join("Writing"), "Writing")
                .expect("run store");
        let retained = retain(&mut destination, &media).expect("retained run media");
        drop(media);
        drop(owner);
        owner_directory
            .close()
            .expect("remove original test source store");
        assert_eq!(retained.len(), 1);
        assert_eq!(
            destination
                .read_blob(retained[0].bytes_blob_id)
                .expect("independent run bytes"),
            bytes
        );
        let serialized = serde_json::to_vec(&retained).expect("media provenance");
        let reopened: Vec<RetainedMedia> =
            serde_json::from_slice(&serialized).expect("retained metadata");
        assert_eq!(reopened[0].bytes_blob_id, BlobId::digest(&bytes));
    }

    #[test]
    fn media_hash_mismatch_rejects_entire_batch_before_retention() {
        let directory = tempfile::tempdir().expect("run directory");
        let (mut store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing")
            .expect("run store");
        let bytes = wav(7);
        let valid = MediaInput {
            id: "valid".into(),
            kind: llama_native_types::MediaKind::Audio,
            mime: "audio/wav".into(),
            sha256: BlobId::digest(&bytes).to_string(),
            bytes,
        };
        let invalid = MediaInput {
            sha256: BlobId::digest(b"different bytes").to_string(),
            ..valid.clone()
        };
        assert_eq!(
            retain(&mut store, &[valid.clone(), invalid])
                .expect_err("hash mismatch")
                .code,
            "media_identity_mismatch"
        );
        assert!(
            store.read_blob(BlobId::digest(&valid.bytes)).is_err(),
            "No preceding batch member was written before validation finished."
        );
    }

    #[test]
    fn conflicting_media_occurrences_reject_before_any_blob_is_retained() {
        let directory = tempfile::tempdir().expect("run directory");
        let (mut store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing")
            .expect("run store");
        let first_bytes = wav(7);
        let second_bytes = wav(8);
        let first = MediaInput {
            id: "voice".into(),
            kind: MediaKind::Audio,
            mime: "audio/wav".into(),
            sha256: BlobId::digest(&first_bytes).to_string(),
            bytes: first_bytes,
        };
        let second = MediaInput {
            sha256: BlobId::digest(&second_bytes).to_string(),
            bytes: second_bytes,
            ..first.clone()
        };
        let error = retain(&mut store, &[first.clone(), second.clone()])
            .expect_err("one occurrence cannot identify two payloads");
        assert_eq!(error.code, "terminal_media_identity_conflict");
        for item in [first, second] {
            assert!(store.read_blob(BlobId::digest(&item.bytes)).is_err());
        }
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
