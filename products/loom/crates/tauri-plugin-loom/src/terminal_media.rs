//! Native media from the admitted source and explicitly resolved documents.
//! Ordinary links never grant file access; only retained attachment identities
//! recognized by the existing context registry supply bytes.

use std::collections::{BTreeMap, HashSet};

use llama_native_types::{MediaInput, MediaKind};
use loom_store::{LoadedDocument, ProjectStore};
use sha2::{Digest as _, Sha256};

use super::IpcFailure;
use super::context_attachments::resolve_media_for_document;
use super::document_bindings::ResolvedDocument;

const MAX_MEDIA: usize = 32;
const MAX_MEDIA_BYTES: usize = 128 * 1024 * 1024;
const MAX_MEDIA_OCCURRENCES: usize = MAX_MEDIA * 257;

/// The sole ordered media admission/budget implementation for source documents,
/// references and terminal/context composition. Payload sharing never changes
/// the source occurrences retained by the caller's document/evidence bindings.
#[derive(Default)]
pub(super) struct MediaAccumulator {
    items: Vec<MediaInput>,
    identities: BTreeMap<String, (MediaKind, String, String)>,
    payloads: BTreeMap<(MediaKind, String), String>,
    bytes: usize,
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
        if item.id.is_empty() || item.id.len() > 256 || item.mime.is_empty() || item.mime.len() > 256 {
            return Err(invalid("Referenced media metadata exceeds its bounded identity contract."));
        }
        if !self.identities.contains_key(&item.id) && self.identities.len() >= MAX_MEDIA_OCCURRENCES {
            return Err(limit());
        }
        if item.bytes.len() > MAX_MEDIA_BYTES {
            return Err(limit());
        }
        // Validate every occurrence BEFORE deduplication. A matching claimed
        // digest must not hide corrupt bytes or conflicting MIME/ID bindings.
        if item.sha256.len() != 64
            || !item.sha256.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || format!("{:x}", Sha256::digest(&item.bytes)) != item.sha256
        {
            return Err(invalid("Referenced media bytes do not match their retained digest."));
        }
        let identity = (item.kind, item.sha256.clone(), item.mime.clone());
        if self.identities.get(&item.id).is_some_and(|existing| existing != &identity) {
            return Err(invalid("One retained media ID refers to conflicting payloads or media types."));
        }
        let key = (item.kind, item.sha256.clone());
        if let Some(mime) = self.payloads.get(&key) {
            if mime != &item.mime {
                return Err(invalid("Identical media payloads have conflicting retained MIME types."));
            }
            self.identities.insert(item.id, identity);
            return Ok(());
        }
        let bytes = self.bytes.checked_add(item.bytes.len()).ok_or_else(limit)?;
        if self.items.len() >= MAX_MEDIA || bytes > MAX_MEDIA_BYTES {
            return Err(limit());
        }
        self.identities.insert(item.id.clone(), identity);
        self.payloads.insert(key, item.mime.clone());
        self.bytes = bytes;
        self.items.push(item);
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
