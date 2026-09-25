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
