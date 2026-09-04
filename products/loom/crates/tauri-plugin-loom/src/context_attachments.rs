use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Cursor;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use atomic_write_file::AtomicWriteFile;
use attachment_native_host::{AttachmentHost, AttachmentHostConfig, ProvidedAttachment};
use attachment_native_types::{
    AttachmentReceipt, AudioPreparationPolicy, Coverage, MediaFamily, PreparationPlan,
    PreparationPolicy, PreparedPart, TargetCapabilities,
};
use llama_native_types::{MediaInput, MediaKind};
use same_file::Handle as FileIdentityHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const MAX_ATTACHMENT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CONTEXT_ATTACHMENTS: usize = 32;
const MAX_CANONICAL_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_MANUAL_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_TEXT_IMPORT_RECEIPTS: usize = 256;
const MAX_NATIVE_MEDIA_OBJECTS: usize = 32;
const MAX_NATIVE_MEDIA_OBJECT_BYTES: u64 = MAX_ATTACHMENT_BYTES;
const MAX_NATIVE_MEDIA_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const CONTEXT_QUERY_BYTES: usize = 16 * 1024;
const EXCERPT_CHUNK_BYTES: usize = 2 * 1024;
const PROMPT_OVERHEAD_TOKENS: u32 = 1_024;
const RETRIEVAL_REBALANCE_BYTES: usize = 16 * 1024;
const PREAMBLE_CACHE_ENTRIES: usize = 8;
const PREAMBLE_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MANIFEST_SCHEMA_V2: &str = "loom.context-attachment.v2";
const MANIFEST_SCHEMA: &str = "loom.context-attachment.v3";
const CONTEXT_SCHEMA_V1: &str = "loom.document-context.v1";
const CONTEXT_SCHEMA_V2: &str = "loom.document-context.v2";
const CONTEXT_SCHEMA: &str = "loom.document-context.v3";
const RETRIEVAL_SCHEMA: &str = "loom.context-retrieval.v3";
const LEGACY_TARGET_FINGERPRINT: &str = "loom:gemma-4-12b-it:native-image-audio:v1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static CONTEXT_WRITE_LOCK: Mutex<()> = Mutex::new(());
static PREAMBLE_CACHE: OnceLock<Mutex<PreambleCache>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StoredAttachment {
    pub(crate) id: String,
    pub(crate) file_name: String,
    pub(crate) byte_count: u64,
    pub(crate) detected_format: String,
    pub(crate) coverage_complete: bool,
    pub(crate) text_bytes: u64,
    pub(crate) media_kinds: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) inline_markdown: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextAttachmentPresentationKind {
    Text,
    Image,
    Audio,
    Mixed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ContextMediaPresentation {
    pub(crate) id: String,
    pub(crate) kind: MediaKind,
    pub(crate) mime_type: String,
    pub(crate) sha256: String,
    pub(crate) byte_count: u64,
    pub(crate) preview_token: Option<String>,
    pub(crate) waveform_peaks: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ContextAttachmentPresentation {
    pub(crate) id: String,
    pub(crate) file_name: String,
    pub(crate) detected_format: String,
    pub(crate) coverage_complete: bool,
    pub(crate) text_bytes: u64,
    pub(crate) presentation_kind: ContextAttachmentPresentationKind,
    pub(crate) media: Vec<ContextMediaPresentation>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DocumentContextSnapshot {
    pub(crate) markdown: String,
    pub(crate) attachments: Vec<ContextAttachmentPresentation>,
    pub(crate) text_sources: Vec<ContextTextSourcePresentation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ContextTextSourcePresentation {
    pub(crate) attachment_id: String,
    pub(crate) file_name: String,
    pub(crate) source_sha256: String,
    pub(crate) source_bytes: u64,
    pub(crate) inserted_sha256: String,
    pub(crate) inserted_bytes: u64,
    pub(crate) complete_projection: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct LoadedContextMedia {
    pub(crate) bytes: Vec<u8>,
    pub(crate) mime_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredMedia {
    id: String,
    kind: MediaKind,
    mime: String,
    sha256: String,
    byte_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    waveform_peaks: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct AttachmentManifest {
    schema: String,
    attachment: StoredAttachment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_text_sha256: Option<String>,
    media: Vec<StoredMedia>,
    preparation_plan: PreparationPlan,
    processing_receipt: AttachmentReceipt,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct DocumentContexts {
    schema: String,
    documents: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    manual_text: BTreeMap<String, String>,
    #[serde(default)]
    text_imports: BTreeMap<String, Vec<ContextTextSourcePresentation>>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ContextExcerptEvidence {
    attachment_id: String,
    source_text_sha256: String,
    start_byte: u64,
    end_byte: u64,
    excerpt_sha256: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ContextRetrievalEvidence {
    schema: String,
    manuscript_prefix_sha256: String,
    manuscript_window_start_byte: u64,
    manuscript_window_end_byte: u64,
    manuscript_window_sha256: String,
    manuscript_query_sha256: String,
    context_window_tokens: u32,
    conservative_prompt_byte_budget: u64,
    retrieval_rebalance_epoch: u64,
    manuscript_omitted_start_byte: Option<u64>,
    manuscript_omitted_end_byte: Option<u64>,
    manuscript_retained_head_end_byte: u64,
    manuscript_retained_tail_start_byte: u64,
    manual_text_sha256: Option<String>,
    excerpts: Vec<ContextExcerptEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreambleCacheKey {
    project_root: PathBuf,
    attachment_ids: Vec<String>,
    attachment_text_fingerprints: Vec<String>,
    manual_text_sha256: Option<String>,
    visible_context_sha256: Option<String>,
    manuscript_query_sha256: String,
    retrieval_rebalance_epoch: u64,
    context_budget: usize,
}

#[derive(Clone, Debug)]
struct PreambleCacheValue {
    text: String,
    excerpts: Vec<ContextExcerptEvidence>,
}

#[derive(Debug, Default)]
struct PreambleCache {
    entries: VecDeque<(PreambleCacheKey, PreambleCacheValue)>,
    retained_bytes: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResolvedContext {
    pub(crate) manuscript_prompt: String,
    pub(crate) context_preamble: String,
    pub(crate) media: Vec<MediaInput>,
    pub(crate) attachment_ids: Vec<String>,
    pub(crate) retrieval_evidence: ContextRetrievalEvidence,
}

#[derive(Debug, Error)]
pub(crate) enum ContextAttachmentError {
    #[error("the attachment path is not an ordinary local file")]
    UnsafeSource,
    #[error("the attachment is empty or exceeds Loom's 128 MB per-file limit")]
    SourceSize,
    #[error("attachment processing failed: {0}")]
    Processing(String),
    #[error("the attachment produced no safe representation for Gemma 4")]
    NoRepresentation,
    #[error("the document context already contains Loom's 32-attachment limit")]
    ContextLimit,
    #[error("pasted context exceeds Loom's 256 KB saved-text limit")]
    ManualTextLimit,
    #[error("the document already contains Loom's 256 text-import receipt limit")]
    TextImportLimit,
    #[error("the attachment context is corrupt or no longer matches its content identity")]
    ContextInvalid,
    #[error("attachment storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("attachment metadata failed: {0}")]
    Json(#[from] serde_json::Error),
}

// This is deliberately one linear grant -> inspect -> canonicalize -> persist
// authority boundary; keeping those checks adjacent makes their order auditable.
#[allow(clippy::too_many_lines)]
pub(crate) fn import_path(
    project_root: &Path,
    source_path: &Path,
) -> Result<StoredAttachment, ContextAttachmentError> {
    let file_name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .ok_or(ContextAttachmentError::UnsafeSource)?
        .to_owned();
    let metadata = fs::symlink_metadata(source_path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_ATTACHMENT_BYTES
    {
        return Err(ContextAttachmentError::SourceSize);
    }
    let mut source = File::open(source_path)?;
    let identity = FileIdentityHandle::from_file(source.try_clone()?)?;
    let provided = ProvidedAttachment::read_bounded(
        file_name.clone(),
        None,
        &mut source,
        MAX_ATTACHMENT_BYTES,
    )
    .map_err(|error| ContextAttachmentError::Processing(error.safe_message))?;
    let visible = FileIdentityHandle::from_path(source_path)?;
    if visible != identity {
        return Err(ContextAttachmentError::UnsafeSource);
    }

    let mut host_config = AttachmentHostConfig {
        preparation: PreparationPolicy {
            // Gemma consumes decoded audio itself. Loom must never silently
            // replace or supplement it with a transcription.
            audio: AudioPreparationPolicy::DirectOnly,
            ..PreparationPolicy::default()
        },
        ..AttachmentHostConfig::default()
    };
    host_config.inspection.limits.max_text_bytes = MAX_CANONICAL_TEXT_BYTES as u64;
    host_config.inspection.limits.max_objects = 65_536;
    host_config.inspection.limits.max_edges = 131_072;
    host_config.inspection.limits.max_entries = 65_536;
    host_config.documents.max_processor_input_bytes = MAX_CANONICAL_TEXT_BYTES;
    let host = AttachmentHost::new(host_config)
        .map_err(|error| ContextAttachmentError::Processing(error.safe_message))?;
    let prepared = host
        .process(provided, &gemma_target())
        .map_err(|error| ContextAttachmentError::Processing(error.safe_message))?;
    let coverage_complete = matches!(prepared.bundle.graph.coverage, Coverage::Complete);
    let mut warnings = coverage_warnings(&prepared.bundle.graph.coverage);
    warnings.extend(
        prepared
            .bundle
            .graph
            .issues
            .iter()
            .map(|issue| issue.safe_message.clone()),
    );
    warnings.sort();
    warnings.dedup();

    let mut canonical_text = Vec::new();
    let mut stored_media = Vec::new();
    let mut media_kinds = BTreeSet::new();
    for part in &prepared.plan.parts {
        match part {
            PreparedPart::UntrustedText { text, .. } => canonical_text.push(text.clone()),
            PreparedPart::DirectMedia {
                artifact_id,
                family,
                blob,
                ..
            } => {
                if !coverage_complete {
                    warnings.push(
                        "Direct media was omitted because the enclosing file was only partially inspected."
                            .to_owned(),
                    );
                    continue;
                }
                let kind = match family {
                    MediaFamily::Image => MediaKind::Image,
                    MediaFamily::Audio => MediaKind::Audio,
                    MediaFamily::Video => continue,
                };
                let bytes = prepared
                    .bundle
                    .blobs
                    .get(&blob.object_id)
                    .ok_or(ContextAttachmentError::ContextInvalid)?;
                if format!("{:x}", Sha256::digest(bytes)) != blob.sha256 {
                    return Err(ContextAttachmentError::ContextInvalid);
                }
                install_object(project_root, &blob.sha256, bytes)?;
                media_kinds.insert(match kind {
                    MediaKind::Image => "image".to_owned(),
                    MediaKind::Audio => "audio".to_owned(),
                });
                stored_media.push(StoredMedia {
                    id: artifact_id.0.clone(),
                    kind,
                    mime: blob.media_type.clone(),
                    sha256: blob.sha256.clone(),
                    byte_count: blob.byte_len,
                    waveform_peaks: (kind == MediaKind::Audio)
                        .then(|| wav_waveform_peaks(bytes))
                        .flatten(),
                });
            }
            PreparedPart::OpaqueReference { .. } => {}
        }
    }
    let canonical_text = canonical_text.join("\n\n");
    if canonical_text.len() > MAX_CANONICAL_TEXT_BYTES {
        return Err(ContextAttachmentError::Processing(
            "canonical attachment text exceeded Loom's 64 MB retained-text limit".to_owned(),
        ));
    }
    if canonical_text.is_empty() && stored_media.is_empty() {
        let detail = coverage_failure_detail(&prepared.bundle.graph.coverage);
        return if detail.is_empty() {
            Err(ContextAttachmentError::NoRepresentation)
        } else {
            Err(ContextAttachmentError::Processing(detail))
        };
    }
    let id = prepared.bundle.graph.root.0.clone();
    let detected_format = prepared
        .bundle
        .graph
        .objects
        .iter()
        .find(|object| object.id == prepared.bundle.graph.root)
        .and_then(|object| object.detection.selected)
        .map_or_else(
            || "unknown".to_owned(),
            |format| format!("{format:?}").to_ascii_lowercase(),
        );
    let text_bytes = u64::try_from(canonical_text.len()).unwrap_or(u64::MAX);
    let canonical_text_sha256 = (!canonical_text.is_empty())
        .then(|| format!("{:x}", Sha256::digest(canonical_text.as_bytes())));
    if let Some(sha256) = canonical_text_sha256.as_deref() {
        install_object(project_root, sha256, canonical_text.as_bytes())?;
    }
    let inline_markdown = inline_attachment_markdown(&id, &file_name);
    let attachment = StoredAttachment {
        id: id.clone(),
        file_name,
        byte_count: metadata.len(),
        detected_format,
        coverage_complete,
        text_bytes,
        media_kinds: media_kinds.into_iter().collect(),
        warnings: warnings
            .into_iter()
            .chain(
                prepared
                    .plan
                    .warnings
                    .iter()
                    .chain(
                        prepared
                            .plan
                            .blockers
                            .iter()
                            .filter(|blocker| blocker.code != "unsupported_opaque_attachment")
                            .map(|blocker| &blocker.safe_message),
                    )
                    .cloned(),
            )
            .collect(),
        inline_markdown,
    };
    let manifest = AttachmentManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        attachment: attachment.clone(),
        canonical_text: None,
        canonical_text_sha256,
        media: stored_media,
        preparation_plan: prepared.plan,
        processing_receipt: prepared.receipt,
    };
    if let Some(existing) = read_manifest_if_present(project_root, &id)? {
        if read_canonical_text(project_root, &existing)? != canonical_text
            || existing.media != manifest.media
        {
            return Err(ContextAttachmentError::ContextInvalid);
        }
        return Ok(existing.attachment);
    }
    write_manifest(project_root, &manifest)?;
    Ok(attachment)
}

#[cfg(test)]
fn document_context(
    project_root: &Path,
    document_id: &str,
) -> Result<Vec<StoredAttachment>, ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    let ids = authoritative_context_ids(&contexts, document_id);
    attachments_for_ids(project_root, &ids)
}

#[cfg(test)]
fn document_context_text(
    project_root: &Path,
    document_id: &str,
) -> Result<String, ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    effective_context_text(project_root, &contexts, document_id)
}

pub(crate) fn document_context_snapshot(
    project_root: &Path,
    document_id: &str,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    snapshot_from_contexts(project_root, &contexts, document_id)
}

pub(crate) fn set_document_context_snapshot(
    project_root: &Path,
    document_id: &str,
    markdown: &str,
    attachment_ids: &[String],
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    set_document_context_snapshot_with_sources(
        project_root,
        document_id,
        markdown,
        attachment_ids,
        None,
    )
}

pub(crate) fn set_document_context_snapshot_with_sources(
    project_root: &Path,
    document_id: &str,
    markdown: &str,
    attachment_ids: &[String],
    text_sources: Option<&[ContextTextSourcePresentation]>,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    if markdown.contains("(loom-attachment:") || markdown.contains("(loom-media:") {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let ids = ordered_unique_attachment_ids(attachment_ids)?;
    let manifests = media_manifest_metadata_for_ids(project_root, &ids)?;
    let internal = encode_context_markdown(markdown, &manifests);
    if internal.len() > MAX_MANUAL_CONTEXT_BYTES {
        return Err(ContextAttachmentError::ManualTextLimit);
    }
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    if let Some(text_sources) = text_sources {
        validate_text_source_receipts(project_root, markdown, text_sources)?;
        if text_sources.is_empty() {
            contexts.text_imports.remove(document_id);
        } else {
            contexts
                .text_imports
                .insert(document_id.to_owned(), text_sources.to_vec());
        }
    } else if markdown.is_empty() {
        contexts.text_imports.remove(document_id);
    }
    set_authoritative_context(&mut contexts, document_id, internal, ids);
    write_contexts(project_root, &contexts)?;
    snapshot_from_contexts(project_root, &contexts, document_id)
}

fn validate_text_source_receipts(
    project_root: &Path,
    _markdown: &str,
    receipts: &[ContextTextSourcePresentation],
) -> Result<(), ContextAttachmentError> {
    if receipts.len() > MAX_TEXT_IMPORT_RECEIPTS {
        return Err(ContextAttachmentError::TextImportLimit);
    }
    for receipt in receipts {
        let manifest = read_manifest(project_root, &receipt.attachment_id)?;
        let canonical = read_canonical_text(project_root, &manifest)?;
        let projection_bytes = usize::try_from(receipt.inserted_bytes)
            .map_err(|_| ContextAttachmentError::ContextInvalid)?;
        let projection = middle_out_manuscript(&canonical, projection_bytes).0;
        if receipt.file_name != manifest.attachment.file_name
            || receipt.source_bytes != u64::try_from(canonical.len()).unwrap_or(u64::MAX)
            || receipt.source_sha256 != format!("{:x}", Sha256::digest(canonical.as_bytes()))
            || receipt.inserted_sha256 != format!("{:x}", Sha256::digest(projection.as_bytes()))
            || projection.len() != projection_bytes
        {
            return Err(ContextAttachmentError::ContextInvalid);
        }
    }
    Ok(())
}

pub(crate) fn add_document_context_snapshot(
    project_root: &Path,
    document_id: &str,
    attachment_ids: &[String],
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    let requested_ids = ordered_unique_attachment_ids(attachment_ids)?;
    let requested = manifests_for_ids(project_root, &requested_ids)?;
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    let current_internal = effective_context_text(project_root, &contexts, document_id)?;
    let mut selected_ids = media_attachment_ids(
        project_root,
        &authoritative_context_ids(&contexts, document_id),
    )?;
    let mut markdown = strip_inline_attachment_markers(&current_internal);
    for (id, manifest) in requested {
        let already_selected = selected_ids.iter().any(|candidate| candidate == &id);
        if already_selected && manifest.attachment.text_bytes == 0 {
            continue;
        }
        if !manifest.media.is_empty() && !already_selected {
            selected_ids.push(id.clone());
        }
        let canonical = read_canonical_text(project_root, &manifest)?;
        let source_sha256 = format!("{:x}", Sha256::digest(canonical.as_bytes()));
        let prior_receipt = contexts
            .text_imports
            .get(document_id)
            .and_then(|imports| {
                imports.iter().find(|receipt| {
                    receipt.attachment_id == id && receipt.source_sha256 == source_sha256
                })
            })
            .cloned();
        let already_projected = prior_receipt.as_ref().is_some_and(|receipt| {
            usize::try_from(receipt.inserted_bytes)
                .ok()
                .map(|bytes| middle_out_manuscript(&canonical, bytes).0)
                .is_some_and(|projection| {
                    format!("{:x}", Sha256::digest(projection.as_bytes()))
                        == receipt.inserted_sha256
                        && markdown.contains(&projection)
                })
        });
        if canonical.is_empty() || already_projected {
            continue;
        }
        if prior_receipt.is_some()
            && let Some(imports) = contexts.text_imports.get_mut(document_id)
        {
            imports.retain(|receipt| {
                receipt.attachment_id != id || receipt.source_sha256 != source_sha256
            });
        }
        let selected = media_manifest_metadata_for_ids(project_root, &selected_ids)?;
        let marker_bytes = encoded_marker_bytes(&markdown, &selected);
        let separator_bytes = usize::from(!markdown.is_empty()) * 2;
        let available = MAX_MANUAL_CONTEXT_BYTES
            .saturating_sub(marker_bytes)
            .saturating_sub(markdown.len())
            .saturating_sub(separator_bytes);
        if !markdown.contains(&canonical) {
            if available == 0 {
                return Err(ContextAttachmentError::ManualTextLimit);
            }
            let (projection, _) = middle_out_manuscript(&canonical, available);
            if !markdown.is_empty() {
                markdown.push_str("\n\n");
            }
            markdown.push_str(&projection);
            let receipt = ContextTextSourcePresentation {
                attachment_id: id,
                file_name: manifest.attachment.file_name,
                source_sha256,
                source_bytes: u64::try_from(canonical.len()).unwrap_or(u64::MAX),
                inserted_sha256: format!("{:x}", Sha256::digest(projection.as_bytes())),
                inserted_bytes: u64::try_from(projection.len()).unwrap_or(u64::MAX),
                complete_projection: projection.len() == canonical.len(),
            };
            let imports = contexts
                .text_imports
                .entry(document_id.to_owned())
                .or_default();
            if imports.len() >= MAX_TEXT_IMPORT_RECEIPTS {
                return Err(ContextAttachmentError::TextImportLimit);
            }
            imports.push(receipt);
        }
    }
    let selected = media_manifest_metadata_for_ids(project_root, &selected_ids)?;
    let internal = encode_context_markdown(&markdown, &selected);
    if internal.len() > MAX_MANUAL_CONTEXT_BYTES {
        return Err(ContextAttachmentError::ManualTextLimit);
    }
    set_authoritative_context(&mut contexts, document_id, internal, selected_ids);
    write_contexts(project_root, &contexts)?;
    snapshot_from_contexts(project_root, &contexts, document_id)
}

pub(crate) fn remove_document_context_snapshot(
    project_root: &Path,
    document_id: &str,
    attachment_id: &str,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    let internal = effective_context_text(project_root, &contexts, document_id)?;
    let markdown = strip_inline_attachment_markers(&internal);
    let ids = media_attachment_ids(
        project_root,
        &authoritative_context_ids(&contexts, document_id),
    )?
    .into_iter()
    .filter(|id| id != attachment_id)
    .collect::<Vec<_>>();
    let manifests = manifest_metadata_for_ids(project_root, &ids)?;
    let internal = encode_context_markdown(&markdown, &manifests);
    set_authoritative_context(&mut contexts, document_id, internal, ids);
    write_contexts(project_root, &contexts)?;
    snapshot_from_contexts(project_root, &contexts, document_id)
}

pub(crate) fn read_context_media(
    project_root: &Path,
    attachment_id: &str,
    media_sha256: &str,
) -> Result<LoadedContextMedia, ContextAttachmentError> {
    let manifest = read_manifest_metadata(project_root, attachment_id)?;
    let media = manifest
        .media
        .iter()
        .find(|media| media.sha256 == media_sha256)
        .ok_or(ContextAttachmentError::ContextInvalid)?;
    Ok(LoadedContextMedia {
        bytes: read_object(project_root, &media.sha256, media.byte_count)?,
        mime_type: media.mime.clone(),
    })
}

#[cfg(test)]
fn set_document_context_text(
    project_root: &Path,
    document_id: &str,
    text: String,
) -> Result<String, ContextAttachmentError> {
    if text.len() > MAX_MANUAL_CONTEXT_BYTES {
        return Err(ContextAttachmentError::ManualTextLimit);
    }
    let ids = inline_attachment_ids(&text);
    if ids.len() > MAX_CONTEXT_ATTACHMENTS {
        return Err(ContextAttachmentError::ContextLimit);
    }
    let _ = attachments_for_ids(project_root, &ids)?;
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    let unchanged_text = contexts.manual_text.get(document_id) == Some(&text)
        || (text.is_empty() && !contexts.manual_text.contains_key(document_id));
    let unchanged_ids = contexts.documents.get(document_id).map(Vec::as_slice)
        == (!ids.is_empty()).then_some(ids.as_slice());
    if unchanged_text && unchanged_ids {
        return Ok(text);
    }
    set_authoritative_context(&mut contexts, document_id, text.clone(), ids);
    write_contexts(project_root, &contexts)?;
    Ok(text)
}

#[cfg(test)]
fn add_document_context(
    project_root: &Path,
    document_id: &str,
    attachment_id: &str,
) -> Result<Vec<StoredAttachment>, ContextAttachmentError> {
    add_document_contexts(project_root, document_id, &[attachment_id.to_owned()])
}

#[cfg(test)]
fn add_document_contexts(
    project_root: &Path,
    document_id: &str,
    attachment_ids: &[String],
) -> Result<Vec<StoredAttachment>, ContextAttachmentError> {
    let manifests = attachment_ids
        .iter()
        .map(|id| read_manifest(project_root, id))
        .collect::<Result<Vec<_>, _>>()?;
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    let mut text = effective_context_text(project_root, &contexts, document_id)?;
    for manifest in &manifests {
        if !inline_attachment_ids(&text).contains(&manifest.attachment.id) {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&manifest.attachment.inline_markdown);
        }
    }
    if text.len() > MAX_MANUAL_CONTEXT_BYTES {
        return Err(ContextAttachmentError::ManualTextLimit);
    }
    let ids = inline_attachment_ids(&text);
    if ids.len() > MAX_CONTEXT_ATTACHMENTS {
        return Err(ContextAttachmentError::ContextLimit);
    }
    let attachments = attachments_for_ids(project_root, &ids)?;
    set_authoritative_context(&mut contexts, document_id, text, ids);
    write_contexts(project_root, &contexts)?;
    Ok(attachments)
}

#[cfg(test)]
fn remove_document_context(
    project_root: &Path,
    document_id: &str,
    attachment_id: &str,
) -> Result<Vec<StoredAttachment>, ContextAttachmentError> {
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    let text = effective_context_text(project_root, &contexts, document_id)?;
    let text = remove_inline_attachment_markers(&text, attachment_id);
    let ids = inline_attachment_ids(&text);
    let attachments = attachments_for_ids(project_root, &ids)?;
    set_authoritative_context(&mut contexts, document_id, text, ids);
    write_contexts(project_root, &contexts)?;
    Ok(attachments)
}

fn authoritative_context_ids(contexts: &DocumentContexts, document_id: &str) -> Vec<String> {
    let mut ids = contexts
        .documents
        .get(document_id)
        .cloned()
        .unwrap_or_default();
    if let Some(text) = contexts.manual_text.get(document_id) {
        for id in inline_attachment_ids(text)
            .into_iter()
            .chain(inline_media_ids(text))
        {
            if !ids.iter().any(|candidate| candidate == &id) {
                ids.push(id);
            }
        }
    }
    if let Some(imports) = contexts.text_imports.get(document_id) {
        for receipt in imports {
            if !ids
                .iter()
                .any(|candidate| candidate == &receipt.attachment_id)
            {
                ids.push(receipt.attachment_id.clone());
            }
        }
    }
    ids
}

fn effective_context_text(
    project_root: &Path,
    contexts: &DocumentContexts,
    document_id: &str,
) -> Result<String, ContextAttachmentError> {
    if let Some(text) = contexts.manual_text.get(document_id) {
        return Ok(text.clone());
    }
    authoritative_context_ids(contexts, document_id)
        .iter()
        .map(|id| {
            read_manifest(project_root, id).map(|manifest| manifest.attachment.inline_markdown)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|markers| markers.join("\n\n"))
}

#[cfg(test)]
fn attachments_for_ids(
    project_root: &Path,
    ids: &[String],
) -> Result<Vec<StoredAttachment>, ContextAttachmentError> {
    ids.iter()
        .map(|id| read_manifest(project_root, id).map(|manifest| manifest.attachment))
        .collect()
}

fn set_authoritative_context(
    contexts: &mut DocumentContexts,
    document_id: &str,
    text: String,
    mut ids: Vec<String>,
) {
    if text.is_empty() {
        contexts.manual_text.remove(document_id);
    } else {
        contexts.manual_text.insert(document_id.to_owned(), text);
    }
    if let Some(imports) = contexts.text_imports.get(document_id) {
        for receipt in imports {
            if !ids
                .iter()
                .any(|candidate| candidate == &receipt.attachment_id)
            {
                ids.push(receipt.attachment_id.clone());
            }
        }
    }
    if ids.is_empty() {
        contexts.documents.remove(document_id);
    } else {
        contexts.documents.insert(document_id.to_owned(), ids);
    }
}

fn ordered_unique_attachment_ids(
    attachment_ids: &[String],
) -> Result<Vec<String>, ContextAttachmentError> {
    let mut ids = Vec::new();
    for id in attachment_ids {
        if !is_sha256(id) {
            return Err(ContextAttachmentError::ContextInvalid);
        }
        if !ids.iter().any(|candidate| candidate == id) {
            ids.push(id.clone());
        }
    }
    if ids.len() > MAX_CONTEXT_ATTACHMENTS {
        return Err(ContextAttachmentError::ContextLimit);
    }
    Ok(ids)
}

fn manifests_for_ids(
    project_root: &Path,
    ids: &[String],
) -> Result<Vec<(String, AttachmentManifest)>, ContextAttachmentError> {
    ids.iter()
        .map(|id| read_manifest(project_root, id).map(|manifest| (id.clone(), manifest)))
        .collect()
}

fn manifest_metadata_for_ids(
    project_root: &Path,
    ids: &[String],
) -> Result<Vec<(String, AttachmentManifest)>, ContextAttachmentError> {
    ids.iter()
        .map(|id| read_manifest_metadata(project_root, id).map(|manifest| (id.clone(), manifest)))
        .collect()
}

fn media_manifest_metadata_for_ids(
    project_root: &Path,
    ids: &[String],
) -> Result<Vec<(String, AttachmentManifest)>, ContextAttachmentError> {
    let manifests = manifest_metadata_for_ids(project_root, ids)?;
    if manifests
        .iter()
        .any(|(_, manifest)| manifest.media.is_empty())
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(manifests)
}

fn media_attachment_ids(
    project_root: &Path,
    ids: &[String],
) -> Result<Vec<String>, ContextAttachmentError> {
    manifest_metadata_for_ids(project_root, ids).map(|manifests| {
        manifests
            .into_iter()
            .filter_map(|(id, manifest)| (!manifest.media.is_empty()).then_some(id))
            .collect()
    })
}

fn snapshot_from_contexts(
    project_root: &Path,
    contexts: &DocumentContexts,
    document_id: &str,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    let internal = effective_context_text(project_root, contexts, document_id)?;
    let ids = authoritative_context_ids(contexts, document_id);
    let manifests = manifest_metadata_for_ids(project_root, &ids)?;
    Ok(DocumentContextSnapshot {
        markdown: strip_inline_attachment_markers(&internal),
        attachments: manifests
            .into_iter()
            .filter_map(|(_, manifest)| {
                (!manifest.media.is_empty()).then(|| attachment_presentation(manifest))
            })
            .collect(),
        text_sources: contexts
            .text_imports
            .get(document_id)
            .cloned()
            .unwrap_or_default(),
    })
}

fn attachment_presentation(manifest: AttachmentManifest) -> ContextAttachmentPresentation {
    let has_text = manifest.attachment.text_bytes > 0;
    let has_image = manifest
        .media
        .iter()
        .any(|media| media.kind == MediaKind::Image);
    let has_audio = manifest
        .media
        .iter()
        .any(|media| media.kind == MediaKind::Audio);
    let presentation_kind = match (has_text, has_image, has_audio) {
        (true, false, false) => ContextAttachmentPresentationKind::Text,
        (false, true, false) => ContextAttachmentPresentationKind::Image,
        (false, false, true) => ContextAttachmentPresentationKind::Audio,
        _ => ContextAttachmentPresentationKind::Mixed,
    };
    ContextAttachmentPresentation {
        id: manifest.attachment.id,
        file_name: manifest.attachment.file_name,
        detected_format: manifest.attachment.detected_format,
        coverage_complete: manifest.attachment.coverage_complete,
        text_bytes: manifest.attachment.text_bytes,
        presentation_kind,
        media: manifest
            .media
            .into_iter()
            .map(|media| ContextMediaPresentation {
                id: media.id,
                kind: media.kind,
                mime_type: media.mime,
                sha256: media.sha256,
                byte_count: media.byte_count,
                preview_token: None,
                waveform_peaks: media.waveform_peaks,
            })
            .collect(),
        warnings: manifest.attachment.warnings,
    }
}

fn encode_context_markdown(markdown: &str, manifests: &[(String, AttachmentManifest)]) -> String {
    let mut internal = markdown.to_owned();
    for (_, manifest) in manifests {
        if !internal.is_empty() {
            internal.push_str("\n\n");
        }
        internal.push_str(&inline_media_markdown(
            &manifest.attachment.id,
            &manifest.attachment.file_name,
        ));
    }
    internal
}

fn encoded_marker_bytes(markdown: &str, manifests: &[(String, AttachmentManifest)]) -> usize {
    encode_context_markdown(markdown, manifests)
        .len()
        .saturating_sub(markdown.len())
}

fn strip_inline_attachment_markers(markdown: &str) -> String {
    let mut ids = inline_attachment_ids(markdown);
    ids.extend(inline_media_ids(markdown));
    ids.iter().fold(markdown.to_owned(), |text, id| {
        let text = remove_inline_attachment_markers(&text, id);
        remove_inline_media_markers(&text, id)
    })
}

#[allow(clippy::too_many_lines)]
pub(crate) fn resolve_for_generation_with_budget(
    project_root: &Path,
    document_id: &str,
    manuscript_prefix: &str,
    context_window_tokens: u32,
    branch_count: u32,
    max_generated_tokens: u32,
) -> Result<ResolvedContext, ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    let mut ordered_ids = authoritative_context_ids(&contexts, document_id);
    let internal_manual_text = effective_context_text(project_root, &contexts, document_id)?;
    let visible_context_text = strip_inline_attachment_markers(&internal_manual_text);
    let imports = contexts
        .text_imports
        .get(document_id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let (manual_text, imported_boundary_stale) =
        author_steering_without_imported_text(project_root, &visible_context_text, imports)?;
    for id in inline_attachment_ids(manuscript_prefix) {
        if !ordered_ids.iter().any(|candidate| candidate == &id) {
            ordered_ids.push(id);
        }
    }
    if ordered_ids.len() > MAX_CONTEXT_ATTACHMENTS {
        return Err(ContextAttachmentError::ContextLimit);
    }
    let generated_cells = branch_count.saturating_mul(max_generated_tokens);
    let prompt_byte_budget = usize::try_from(
        context_window_tokens
            .saturating_sub(generated_cells)
            .saturating_sub(PROMPT_OVERHEAD_TOKENS),
    )
    .unwrap_or(usize::MAX);
    let context_budget = if manuscript_prefix.len() > prompt_byte_budget / 2 {
        prompt_byte_budget / 4
    } else {
        prompt_byte_budget / 2
    };
    let mut stable_query_end = if manuscript_prefix.len() <= RETRIEVAL_REBALANCE_BYTES {
        manuscript_prefix.len()
    } else {
        (manuscript_prefix.len() / RETRIEVAL_REBALANCE_BYTES) * RETRIEVAL_REBALANCE_BYTES
    };
    while stable_query_end > 0 && !manuscript_prefix.is_char_boundary(stable_query_end) {
        stable_query_end -= 1;
    }
    let query = trailing_utf8(&manuscript_prefix[..stable_query_end], CONTEXT_QUERY_BYTES);
    let query_sha256 = format!("{:x}", Sha256::digest(query.as_bytes()));
    let manual_text_sha256 =
        (!manual_text.is_empty()).then(|| format!("{:x}", Sha256::digest(manual_text.as_bytes())));
    let visible_context_sha256 = (!visible_context_text.is_empty())
        .then(|| format!("{:x}", Sha256::digest(visible_context_text.as_bytes())));
    let rebalance_epoch = (stable_query_end / RETRIEVAL_REBALANCE_BYTES) as u64;
    let manifests = ordered_ids
        .iter()
        .map(|id| read_manifest_metadata(project_root, id).map(|manifest| (id.clone(), manifest)))
        .collect::<Result<Vec<_>, _>>()?;
    preflight_native_media(&manifests)?;
    let cache_key = PreambleCacheKey {
        project_root: project_root.to_path_buf(),
        attachment_ids: ordered_ids.clone(),
        attachment_text_fingerprints: manifests
            .iter()
            .map(|(id, manifest)| attachment_text_fingerprint(id, manifest))
            .collect(),
        manual_text_sha256: manual_text_sha256.clone(),
        visible_context_sha256,
        manuscript_query_sha256: query_sha256.clone(),
        retrieval_rebalance_epoch: rebalance_epoch,
        context_budget,
    };
    let (text, excerpts) = if let Some(cached) = cached_preamble(&cache_key) {
        (cached.text, cached.excerpts)
    } else {
        let mut sources = Vec::new();
        if imported_boundary_stale && !visible_context_text.is_empty() {
            let mut presentation = manifests
                .iter()
                .find(|(id, _)| imports.iter().any(|item| item.attachment_id == *id))
                .map(|(_, manifest)| manifest.attachment.clone())
                .ok_or(ContextAttachmentError::ContextInvalid)?;
            "edited context containing imported material".clone_into(&mut presentation.file_name);
            sources.push((
                "edited-imported-context".to_owned(),
                presentation,
                visible_context_text.clone(),
            ));
        } else {
            for (id, manifest) in &manifests {
                let canonical_text = read_canonical_text(project_root, manifest)?;
                if !canonical_text.is_empty() {
                    sources.push((id.clone(), manifest.attachment.clone(), canonical_text));
                }
            }
        }
        let (text, excerpts) =
            assemble_generation_context(&manual_text, &sources, query, context_budget);
        cache_preamble(
            cache_key,
            PreambleCacheValue {
                text: text.clone(),
                excerpts: excerpts.clone(),
            },
        );
        (text, excerpts)
    };
    let mut media = Vec::new();
    for (id, manifest) in manifests {
        for item in manifest.media {
            let bytes = read_object(project_root, &item.sha256, item.byte_count)?;
            media.push(MediaInput {
                id: format!("{id}:{}", item.id),
                kind: item.kind,
                mime: item.mime,
                sha256: item.sha256,
                bytes,
            });
        }
    }
    let manuscript_budget = prompt_byte_budget.saturating_sub(text.len());
    let (manuscript_prompt, omitted) = middle_out_manuscript(manuscript_prefix, manuscript_budget);
    let retained_head_end = omitted.map_or(manuscript_prefix.len(), |(start, _)| start);
    let retained_tail_start = omitted.map_or(manuscript_prefix.len(), |(_, end)| end);
    let manuscript_window_start = if retained_head_end > 0 {
        0
    } else if retained_tail_start < manuscript_prefix.len() {
        retained_tail_start
    } else {
        0
    };
    let manuscript_window_end = if manuscript_prompt.is_empty() {
        manuscript_window_start
    } else {
        manuscript_prefix.len()
    };
    let manuscript_window_sha256 = format!("{:x}", Sha256::digest(manuscript_prompt.as_bytes()));
    Ok(ResolvedContext {
        manuscript_prompt,
        context_preamble: text,
        media,
        attachment_ids: ordered_ids,
        retrieval_evidence: ContextRetrievalEvidence {
            schema: RETRIEVAL_SCHEMA.to_owned(),
            manuscript_prefix_sha256: format!("{:x}", Sha256::digest(manuscript_prefix.as_bytes())),
            manuscript_window_start_byte: manuscript_window_start as u64,
            manuscript_window_end_byte: manuscript_window_end as u64,
            manuscript_window_sha256,
            manuscript_query_sha256: query_sha256,
            context_window_tokens,
            conservative_prompt_byte_budget: prompt_byte_budget as u64,
            retrieval_rebalance_epoch: rebalance_epoch,
            manuscript_omitted_start_byte: omitted.map(|(start, _)| start as u64),
            manuscript_omitted_end_byte: omitted.map(|(_, end)| end as u64),
            manuscript_retained_head_end_byte: retained_head_end as u64,
            manuscript_retained_tail_start_byte: retained_tail_start as u64,
            manual_text_sha256,
            excerpts,
        },
    })
}

fn author_steering_without_imported_text(
    project_root: &Path,
    visible_text: &str,
    imports: &[ContextTextSourcePresentation],
) -> Result<(String, bool), ContextAttachmentError> {
    if imports.is_empty() {
        return Ok((visible_text.to_owned(), false));
    }
    let mut ranges = Vec::with_capacity(imports.len());
    for receipt in imports {
        let manifest = read_manifest(project_root, &receipt.attachment_id)?;
        let canonical = read_canonical_text(project_root, &manifest)?;
        let inserted_bytes = usize::try_from(receipt.inserted_bytes)
            .map_err(|_| ContextAttachmentError::ContextInvalid)?;
        let projection = middle_out_manuscript(&canonical, inserted_bytes).0;
        if receipt.file_name != manifest.attachment.file_name
            || receipt.source_bytes != u64::try_from(canonical.len()).unwrap_or(u64::MAX)
            || receipt.source_sha256 != format!("{:x}", Sha256::digest(canonical.as_bytes()))
            || receipt.inserted_sha256 != format!("{:x}", Sha256::digest(projection.as_bytes()))
            || projection.len() != inserted_bytes
        {
            return Err(ContextAttachmentError::ContextInvalid);
        }
        let Some(start) = visible_text
            .match_indices(&projection)
            .find_map(|(start, _)| {
                let end = start + projection.len();
                ranges
                    .iter()
                    .all(|(prior_start, prior_end)| end <= *prior_start || start >= *prior_end)
                    .then_some(start)
            })
        else {
            // Once an imported region is edited, its exact range is no longer
            // provable. Fail closed by treating the whole pane as untrusted.
            return Ok((String::new(), true));
        };
        ranges.push((start, start + projection.len()));
    }
    ranges.sort_unstable();
    let mut author = String::new();
    let mut cursor = 0;
    for (start, end) in ranges {
        if start > cursor {
            author.push_str(&visible_text[cursor..start]);
        }
        cursor = end;
    }
    author.push_str(&visible_text[cursor..]);
    Ok((author.trim().to_owned(), false))
}

fn preflight_native_media(
    manifests: &[(String, AttachmentManifest)],
) -> Result<(), ContextAttachmentError> {
    let mut object_count = 0_usize;
    let mut total_bytes = 0_u64;
    for (_, manifest) in manifests {
        for media in &manifest.media {
            object_count = object_count.saturating_add(1);
            total_bytes = total_bytes.checked_add(media.byte_count).ok_or_else(|| {
                ContextAttachmentError::Processing(
                    "native image/audio context exceeds Loom's per-request media limits".to_owned(),
                )
            })?;
            if object_count > MAX_NATIVE_MEDIA_OBJECTS
                || media.byte_count > MAX_NATIVE_MEDIA_OBJECT_BYTES
                || total_bytes > MAX_NATIVE_MEDIA_TOTAL_BYTES
            {
                return Err(ContextAttachmentError::Processing(
                    "native image/audio context exceeds Loom's per-request media limits".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn wav_waveform_peaks(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).ok()?;
    let spec = reader.spec();
    let sample_count = usize::try_from(reader.len()).ok()?;
    if sample_count == 0 {
        return Some(Vec::new());
    }
    let bin_count = sample_count.min(64);
    let mut peaks = vec![0.0_f32; bin_count];
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (index, sample) in reader.samples::<f32>().enumerate() {
                let value = sample.ok()?.abs().clamp(0.0, 1.0);
                let bin = index.saturating_mul(bin_count) / sample_count;
                peaks[bin.min(bin_count - 1)] = peaks[bin.min(bin_count - 1)].max(value);
            }
        }
        hound::SampleFormat::Int => {
            let exponent = u32::from(spec.bits_per_sample.saturating_sub(1));
            let maximum = (2_u64.checked_pow(exponent)? as f64).max(1.0);
            for (index, sample) in reader.samples::<i32>().enumerate() {
                let value = ((f64::from(sample.ok()?).abs() / maximum).clamp(0.0, 1.0)) as f32;
                let bin = index.saturating_mul(bin_count) / sample_count;
                peaks[bin.min(bin_count - 1)] = peaks[bin.min(bin_count - 1)].max(value);
            }
        }
    }
    Some(
        peaks
            .into_iter()
            .map(|peak| (peak * 255.0).round() as u8)
            .collect(),
    )
}

fn attachment_text_fingerprint(id: &str, manifest: &AttachmentManifest) -> String {
    let mut digest = Sha256::new();
    digest.update(id.as_bytes());
    digest.update([0]);
    digest.update(manifest.attachment.file_name.as_bytes());
    digest.update([0]);
    digest.update(manifest.attachment.text_bytes.to_le_bytes());
    digest.update([u8::from(manifest.attachment.coverage_complete)]);
    if let Some(sha256) = manifest.canonical_text_sha256.as_deref() {
        digest.update(sha256.as_bytes());
    } else if let Some(text) = manifest.canonical_text.as_deref() {
        digest.update(Sha256::digest(text.as_bytes()));
    }
    format!("{:x}", digest.finalize())
}

fn preamble_cache() -> &'static Mutex<PreambleCache> {
    PREAMBLE_CACHE.get_or_init(|| Mutex::new(PreambleCache::default()))
}

fn cached_preamble(key: &PreambleCacheKey) -> Option<PreambleCacheValue> {
    let mut cache = preamble_cache().lock().ok()?;
    let index = cache
        .entries
        .iter()
        .position(|(candidate, _)| candidate == key)?;
    let entry = cache.entries.remove(index)?;
    let value = entry.1.clone();
    cache.entries.push_back(entry);
    Some(value)
}

fn cache_preamble(key: PreambleCacheKey, value: PreambleCacheValue) {
    let value_bytes = preamble_value_bytes(&value);
    if value_bytes > PREAMBLE_CACHE_BYTES {
        return;
    }
    let Ok(mut cache) = preamble_cache().lock() else {
        return;
    };
    if let Some(index) = cache
        .entries
        .iter()
        .position(|(candidate, _)| candidate == &key)
        && let Some((_, previous)) = cache.entries.remove(index)
    {
        cache.retained_bytes = cache
            .retained_bytes
            .saturating_sub(preamble_value_bytes(&previous));
    }
    while cache.entries.len() >= PREAMBLE_CACHE_ENTRIES
        || cache.retained_bytes.saturating_add(value_bytes) > PREAMBLE_CACHE_BYTES
    {
        let Some((_, evicted)) = cache.entries.pop_front() else {
            break;
        };
        cache.retained_bytes = cache
            .retained_bytes
            .saturating_sub(preamble_value_bytes(&evicted));
    }
    cache.retained_bytes = cache.retained_bytes.saturating_add(value_bytes);
    cache.entries.push_back((key, value));
}

fn preamble_value_bytes(value: &PreambleCacheValue) -> usize {
    value.text.len().saturating_add(
        value.excerpts.len().saturating_mul(
            std::mem::size_of::<ContextExcerptEvidence>() + 3 * Sha256::output_size(),
        ),
    )
}

#[cfg(test)]
fn resolve_for_generation(
    project_root: &Path,
    document_id: &str,
    manuscript_prefix: &str,
) -> Result<ResolvedContext, ContextAttachmentError> {
    resolve_for_generation_with_budget(project_root, document_id, manuscript_prefix, 32_768, 4, 96)
}

fn assemble_generation_context(
    manual_text: &str,
    sources: &[(String, StoredAttachment, String)],
    query: &str,
    context_budget: usize,
) -> (String, Vec<ContextExcerptEvidence>) {
    let mut rendered = String::new();
    let mut evidence = Vec::new();

    if !manual_text.is_empty() {
        let header = "[BEGIN AUTHOR STEERING CONTEXT]\n";
        let footer = "\n[END AUTHOR STEERING CONTEXT]";
        let available = context_budget.saturating_sub(header.len() + footer.len());
        let budget = if sources.is_empty() {
            available
        } else {
            available.min(context_budget / 3)
        };
        let (selected, mut selected_evidence) =
            select_excerpts("manual", manual_text, query, budget);
        if !selected.is_empty() {
            rendered.push_str(header);
            rendered.push_str(&selected);
            rendered.push_str(footer);
            evidence.append(&mut selected_evidence);
        }
    }

    for (index, (id, attachment, source_text)) in sources.iter().enumerate() {
        let separator = usize::from(!rendered.is_empty()) * 2;
        let remaining = context_budget.saturating_sub(rendered.len() + separator);
        let remaining_sources = sources.len().saturating_sub(index).max(1);
        let header = format!(
            "[BEGIN UNTRUSTED ATTACHMENT EXCERPTS id={} name={:?} coverage={}]\n",
            id,
            attachment.file_name,
            if attachment.coverage_complete {
                "complete"
            } else {
                "partial"
            }
        );
        let footer = format!("\n[END UNTRUSTED ATTACHMENT EXCERPTS id={id}]");
        let fair_share = remaining / remaining_sources;
        let payload_budget = fair_share.saturating_sub(header.len() + footer.len());
        let (selected, mut selected_evidence) =
            select_excerpts(id, source_text, query, payload_budget);
        if selected.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        rendered.push_str(&header);
        rendered.push_str(&selected);
        rendered.push_str(&footer);
        evidence.append(&mut selected_evidence);
    }
    debug_assert!(rendered.len() <= context_budget);
    (rendered, evidence)
}

fn middle_out_manuscript(text: &str, budget: usize) -> (String, Option<(usize, usize)>) {
    if text.len() <= budget {
        return (text.to_owned(), None);
    }
    if budget == 0 {
        return (String::new(), Some((0, text.len())));
    }
    let marker = "\n\n[... earlier manuscript middle omitted ...]\n\n";
    if budget <= marker.len() + 2 {
        let tail = trailing_utf8(text, budget);
        return (tail.to_owned(), Some((0, text.len() - tail.len())));
    }
    let payload = budget - marker.len();
    let mut head_end = (payload / 4).min(text.len());
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let tail = trailing_utf8(text, payload - head_end);
    let tail_start = text.len() - tail.len();
    let mut rendered = String::with_capacity(head_end + marker.len() + tail.len());
    rendered.push_str(&text[..head_end]);
    rendered.push_str(marker);
    rendered.push_str(tail);
    (rendered, Some((head_end, tail_start)))
}

fn select_excerpts(
    source_id: &str,
    text: &str,
    query: &str,
    budget: usize,
) -> (String, Vec<ContextExcerptEvidence>) {
    if text.is_empty() || budget == 0 {
        return (String::new(), Vec::new());
    }
    let source_text_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    if text.len() <= budget {
        return (
            text.to_owned(),
            vec![excerpt_evidence(
                source_id,
                &source_text_sha256,
                0,
                text.len(),
                text,
            )],
        );
    }

    let terms = query_terms(query);
    let ranges = excerpt_chunk_ranges(text);
    let mut ranked = ranges
        .iter()
        .copied()
        .map(|(start, end)| {
            let score = lexical_score(&text[start..end], &terms);
            (score, start, end)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    if let Some(first) = ranked.iter().position(|(_, start, _)| *start == 0) {
        let front = ranked.remove(first);
        ranked.insert(0, front);
    }
    if ranked.iter().all(|(score, _, _)| *score == 0)
        && let Some(last) = ranges.last().copied()
        && let Some(last_index) = ranked
            .iter()
            .position(|(_, start, end)| (*start, *end) == last)
        && last_index > 1
    {
        let tail = ranked.remove(last_index);
        ranked.insert(1.min(ranked.len()), tail);
    }

    let mut chosen = Vec::new();
    let mut used = 0_usize;
    for (_, start, end) in ranked {
        let marker = excerpt_marker(start, end, text.len());
        let addition = marker.len() + end.saturating_sub(start) + usize::from(!chosen.is_empty());
        if used.saturating_add(addition) > budget {
            continue;
        }
        used += addition;
        chosen.push((start, end));
    }
    chosen.sort_unstable();

    let mut rendered = String::with_capacity(used);
    let mut evidence = Vec::with_capacity(chosen.len());
    for (index, (start, end)) in chosen.into_iter().enumerate() {
        if index > 0 {
            rendered.push('\n');
        }
        rendered.push_str(&excerpt_marker(start, end, text.len()));
        let excerpt = &text[start..end];
        rendered.push_str(excerpt);
        evidence.push(excerpt_evidence(
            source_id,
            &source_text_sha256,
            start,
            end,
            excerpt,
        ));
    }
    (rendered, evidence)
}

fn excerpt_evidence(
    source_id: &str,
    source_text_sha256: &str,
    start: usize,
    end: usize,
    excerpt: &str,
) -> ContextExcerptEvidence {
    ContextExcerptEvidence {
        attachment_id: source_id.to_owned(),
        source_text_sha256: source_text_sha256.to_owned(),
        start_byte: start as u64,
        end_byte: end as u64,
        excerpt_sha256: format!("{:x}", Sha256::digest(excerpt.as_bytes())),
    }
}

fn excerpt_marker(start: usize, end: usize, total: usize) -> String {
    format!("[EXCERPT bytes {start}..{end} of {total}]\n")
}

fn excerpt_chunk_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    while start < text.len() {
        let mut end = (start + EXCERPT_CHUNK_BYTES).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = text[start..]
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(offset, _)| start + offset);
        }
        if end < text.len() {
            let floor = start + (end - start) / 2;
            if let Some(relative) = text[floor..end].rfind("\n\n") {
                let paragraph_end = floor + relative + 2;
                if paragraph_end > start {
                    end = paragraph_end;
                }
            } else if let Some(relative) = text[floor..end].rfind('\n') {
                let line_end = floor + relative + 1;
                if line_end > start {
                    end = line_end;
                }
            }
        }
        ranges.push((start, end));
        start = end;
    }
    ranges
}

fn query_terms(query: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for word in query.rsplit(|character: char| !character.is_alphanumeric()) {
        if word.chars().count() < 4 {
            continue;
        }
        let word = word.to_lowercase();
        if CONTEXT_STOP_WORDS.contains(&word.as_str()) {
            continue;
        }
        terms.insert(word);
        if terms.len() == 128 {
            break;
        }
    }
    terms
}

fn lexical_score(text: &str, terms: &BTreeSet<String>) -> usize {
    if terms.is_empty() {
        return 0;
    }
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .map(str::to_lowercase)
        .filter(|word| terms.contains(word))
        .collect::<BTreeSet<_>>()
        .len()
}

fn trailing_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

const CONTEXT_STOP_WORDS: &[&str] = &[
    "about", "after", "again", "also", "been", "before", "being", "between", "could", "from",
    "have", "into", "just", "more", "most", "other", "over", "same", "some", "such", "than",
    "that", "their", "them", "then", "there", "these", "they", "this", "those", "through", "under",
    "very", "what", "when", "where", "which", "while", "with", "would", "your",
];

fn gemma_target() -> TargetCapabilities {
    TargetCapabilities {
        target_id: "google.gemma-4-12b-it".to_owned(),
        fingerprint: "loom:gemma-4-12b-it:native-image-audio:excerpted-text:v2".to_owned(),
        accepted_media_types: BTreeSet::new(),
        accepted_media_families: BTreeSet::from([MediaFamily::Image, MediaFamily::Audio]),
        max_media_objects: 32,
        max_media_bytes: MAX_ATTACHMENT_BYTES,
        max_text_bytes: MAX_CANONICAL_TEXT_BYTES as u64,
        supports_markdown: true,
        supports_native_pdf: false,
        supports_native_video: false,
    }
}

fn coverage_warnings(coverage: &Coverage) -> Vec<String> {
    match coverage {
        Coverage::Complete => Vec::new(),
        Coverage::Partial { reasons } => reasons
            .iter()
            .map(|reason| format!("Only part of this file was canonicalized ({reason})."))
            .collect(),
    }
}

fn coverage_failure_detail(coverage: &Coverage) -> String {
    match coverage {
        Coverage::Complete => String::new(),
        Coverage::Partial { reasons } if reasons.is_empty() => {
            "attachment inspection was incomplete and produced no usable text or direct media"
                .to_owned()
        }
        Coverage::Partial { reasons } => format!(
            "attachment inspection was incomplete and produced no usable representation: {}",
            reasons.join(", ")
        ),
    }
}

fn inline_attachment_markdown(id: &str, file_name: &str) -> String {
    let label = file_name
        .replace(['[', ']', '\\'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[📎 {}](loom-attachment:{id})",
        if label.is_empty() {
            "Attachment"
        } else {
            &label
        }
    )
}

fn inline_media_markdown(id: &str, file_name: &str) -> String {
    inline_markdown_with_scheme(id, file_name, "loom-media")
}

fn inline_markdown_with_scheme(id: &str, file_name: &str, scheme: &str) -> String {
    let label = file_name
        .replace(['[', ']', '\\'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Media: {}]({scheme}:{id})",
        if label.is_empty() {
            "Attachment"
        } else {
            &label
        }
    )
}

fn inline_attachment_ids(markdown: &str) -> Vec<String> {
    inline_ids_with_scheme(markdown, "loom-attachment")
}

fn inline_media_ids(markdown: &str) -> Vec<String> {
    inline_ids_with_scheme(markdown, "loom-media")
}

fn inline_ids_with_scheme(markdown: &str, scheme: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let marker = format!("({scheme}:");
    let mut rest = markdown;
    while let Some(start) = rest.find(&marker) {
        rest = &rest[start + marker.len()..];
        let Some(end) = rest.find(')') else { break };
        let candidate = &rest[..end];
        if is_sha256(candidate) && !ids.iter().any(|id| id == candidate) {
            ids.push(candidate.to_owned());
        }
        rest = &rest[end + 1..];
    }
    ids
}

fn remove_inline_attachment_markers(markdown: &str, attachment_id: &str) -> String {
    remove_inline_markers_with_scheme(markdown, attachment_id, "loom-attachment")
}

fn remove_inline_media_markers(markdown: &str, attachment_id: &str) -> String {
    remove_inline_markers_with_scheme(markdown, attachment_id, "loom-media")
}

fn remove_inline_markers_with_scheme(markdown: &str, attachment_id: &str, scheme: &str) -> String {
    if !is_sha256(attachment_id) {
        return markdown.to_owned();
    }
    let needle = format!("]({scheme}:{attachment_id})");
    let mut remaining = markdown;
    let mut rendered = String::with_capacity(markdown.len());
    while let Some(marker_end) = remaining.find(&needle) {
        let prefix = &remaining[..marker_end];
        let Some(label_start) = prefix.rfind('[') else {
            rendered.push_str(&remaining[..marker_end + needle.len()]);
            remaining = &remaining[marker_end + needle.len()..];
            continue;
        };
        let mut retained_prefix_end = label_start;
        let mut retained_suffix = &remaining[marker_end + needle.len()..];
        if let Some(suffix) = retained_suffix.strip_prefix("\n\n") {
            retained_suffix = suffix;
        } else if prefix[..label_start].ends_with("\n\n") {
            retained_prefix_end -= 2;
        }
        rendered.push_str(&prefix[..retained_prefix_end]);
        remaining = retained_suffix;
    }
    rendered.push_str(remaining);
    if rendered.trim().is_empty() {
        String::new()
    } else {
        rendered
    }
}

fn attachment_root(project_root: &Path) -> Result<PathBuf, ContextAttachmentError> {
    let loom = project_root.join(".loom");
    let root = loom.join("attachments");
    let objects = root.join("objects");
    let manifests = root.join("manifests");
    for path in [&loom, &root, &objects, &manifests] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(ContextAttachmentError::ContextInvalid),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root)
}

fn install_object(
    project_root: &Path,
    sha256: &str,
    bytes: &[u8],
) -> Result<(), ContextAttachmentError> {
    if !is_sha256(sha256) || format!("{:x}", Sha256::digest(bytes)) != sha256 {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let root = attachment_root(project_root)?;
    let path = root.join("objects").join(sha256);
    install_immutable(&path, bytes)
}

fn read_object(
    project_root: &Path,
    sha256: &str,
    expected_bytes: u64,
) -> Result<Vec<u8>, ContextAttachmentError> {
    if !is_sha256(sha256) || expected_bytes > MAX_ATTACHMENT_BYTES {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let path = attachment_root(project_root)?.join("objects").join(sha256);
    let bytes = fs::read(path)?;
    if bytes.len() as u64 != expected_bytes || format!("{:x}", Sha256::digest(&bytes)) != sha256 {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(bytes)
}

fn write_manifest(
    project_root: &Path,
    manifest: &AttachmentManifest,
) -> Result<(), ContextAttachmentError> {
    let path = attachment_root(project_root)?
        .join("manifests")
        .join(format!("{}.json", manifest.attachment.id));
    let bytes = serde_json::to_vec_pretty(manifest)?;
    install_immutable(&path, &bytes)
}

fn read_manifest(
    project_root: &Path,
    id: &str,
) -> Result<AttachmentManifest, ContextAttachmentError> {
    let manifest = read_manifest_metadata(project_root, id)?;
    let _ = read_canonical_text(project_root, &manifest)?;
    Ok(manifest)
}

fn read_manifest_metadata(
    project_root: &Path,
    id: &str,
) -> Result<AttachmentManifest, ContextAttachmentError> {
    read_manifest_if_present(project_root, id)?.ok_or_else(|| {
        ContextAttachmentError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "attachment manifest does not exist",
        ))
    })
}

fn read_manifest_if_present(
    project_root: &Path,
    id: &str,
) -> Result<Option<AttachmentManifest>, ContextAttachmentError> {
    if !is_sha256(id) {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let path = attachment_root(project_root)?
        .join("manifests")
        .join(format!("{id}.json"));
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let manifest: AttachmentManifest = serde_json::from_slice(&bytes)?;
    let expected_media_kinds = manifest
        .media
        .iter()
        .map(|media| match media.kind {
            MediaKind::Image => "image".to_owned(),
            MediaKind::Audio => "audio".to_owned(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let text_representation_is_valid = match manifest.schema.as_str() {
        MANIFEST_SCHEMA => {
            manifest.canonical_text.is_none()
                && manifest.attachment.text_bytes <= MAX_CANONICAL_TEXT_BYTES as u64
                && match (
                    manifest.attachment.text_bytes,
                    manifest.canonical_text_sha256.as_deref(),
                ) {
                    (0, None) => true,
                    (1.., Some(sha256)) => is_sha256(sha256),
                    _ => false,
                }
        }
        MANIFEST_SCHEMA_V2 => {
            manifest.canonical_text_sha256.is_none()
                && (manifest.canonical_text.as_ref().is_some_and(|text| {
                    text.len() as u64 == manifest.attachment.text_bytes
                        && text.len() <= MAX_CANONICAL_TEXT_BYTES
                }) || (manifest.canonical_text.is_none()
                    && manifest.attachment.text_bytes == 0))
        }
        _ => false,
    };
    let target_fingerprint_is_valid = manifest.preparation_plan.target_fingerprint
        == gemma_target().fingerprint
        || (manifest.schema == MANIFEST_SCHEMA_V2
            && manifest.preparation_plan.target_fingerprint == LEGACY_TARGET_FINGERPRINT);
    if !text_representation_is_valid
        || manifest.attachment.id != id
        || manifest.attachment.media_kinds != expected_media_kinds
        || manifest.attachment.inline_markdown
            != inline_attachment_markdown(id, &manifest.attachment.file_name)
        || manifest.media.iter().any(|media| {
            !is_sha256(&media.sha256)
                || !media_mime_matches_kind(media.kind, &media.mime)
                || media.byte_count == 0
                || media.byte_count > MAX_ATTACHMENT_BYTES
                || media
                    .waveform_peaks
                    .as_ref()
                    .is_some_and(|peaks| media.kind != MediaKind::Audio || peaks.len() > 64)
        })
        || manifest.processing_receipt.root_sha256 != id
        || manifest.processing_receipt.complete_coverage != manifest.attachment.coverage_complete
        || (!manifest.attachment.coverage_complete && !manifest.media.is_empty())
        || manifest.processing_receipt.network_used
        || manifest.processing_receipt.process_used
        || manifest.processing_receipt.model_invoked
        || manifest.preparation_plan.source_job_id != manifest.processing_receipt.job_id
        || manifest.preparation_plan.target_id != gemma_target().target_id
        || !target_fingerprint_is_valid
        || !manifest.preparation_plan.transforms.is_empty()
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(Some(manifest))
}

fn media_mime_matches_kind(kind: MediaKind, mime: &str) -> bool {
    match kind {
        MediaKind::Image => matches!(
            mime,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ),
        MediaKind::Audio => matches!(
            mime,
            "audio/wav" | "audio/flac" | "audio/mpeg" | "audio/ogg" | "audio/mp4"
        ),
    }
}

fn read_canonical_text(
    project_root: &Path,
    manifest: &AttachmentManifest,
) -> Result<String, ContextAttachmentError> {
    match (
        manifest.canonical_text.as_deref(),
        manifest.canonical_text_sha256.as_deref(),
    ) {
        (Some(text), None) if manifest.schema == MANIFEST_SCHEMA_V2 => Ok(text.to_owned()),
        (None, Some(sha256)) if manifest.schema == MANIFEST_SCHEMA => {
            let bytes = read_object(project_root, sha256, manifest.attachment.text_bytes)?;
            String::from_utf8(bytes).map_err(|_| ContextAttachmentError::ContextInvalid)
        }
        (None, None) if manifest.attachment.text_bytes == 0 => Ok(String::new()),
        _ => Err(ContextAttachmentError::ContextInvalid),
    }
}

fn read_contexts(project_root: &Path) -> Result<DocumentContexts, ContextAttachmentError> {
    let path = attachment_root(project_root)?.join("document-context.json");
    match fs::read(path) {
        Ok(bytes) => {
            let contexts: DocumentContexts = serde_json::from_slice(&bytes)?;
            if contexts.schema != CONTEXT_SCHEMA
                && contexts.schema != CONTEXT_SCHEMA_V2
                && contexts.schema != CONTEXT_SCHEMA_V1
            {
                return Err(ContextAttachmentError::ContextInvalid);
            }
            if contexts
                .documents
                .values()
                .any(|ids| ids.len() > MAX_CONTEXT_ATTACHMENTS)
                || contexts
                    .manual_text
                    .values()
                    .any(|text| text.len() > MAX_MANUAL_CONTEXT_BYTES)
                || contexts.text_imports.values().any(|imports| {
                    imports.len() > MAX_TEXT_IMPORT_RECEIPTS
                        || imports.iter().any(|import| {
                            !is_sha256(&import.attachment_id)
                                || !is_sha256(&import.source_sha256)
                                || !is_sha256(&import.inserted_sha256)
                        })
                })
            {
                return Err(ContextAttachmentError::ContextInvalid);
            }
            Ok(DocumentContexts {
                schema: CONTEXT_SCHEMA.to_owned(),
                ..contexts
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentContexts {
            schema: CONTEXT_SCHEMA.to_owned(),
            documents: BTreeMap::new(),
            manual_text: BTreeMap::new(),
            text_imports: BTreeMap::new(),
        }),
        Err(error) => Err(error.into()),
    }
}

fn write_contexts(
    project_root: &Path,
    contexts: &DocumentContexts,
) -> Result<(), ContextAttachmentError> {
    let path = attachment_root(project_root)?.join("document-context.json");
    replace_atomically(&path, &serde_json::to_vec_pretty(contexts)?)
}

fn install_immutable(path: &Path, bytes: &[u8]) -> Result<(), ContextAttachmentError> {
    if let Ok(existing) = fs::read(path) {
        return if existing == bytes {
            Ok(())
        } else {
            Err(ContextAttachmentError::ContextInvalid)
        };
    }
    let temp = temporary_sibling(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    match fs::hard_link(&temp, path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(path)? != bytes {
                let _ = fs::remove_file(&temp);
                return Err(ContextAttachmentError::ContextInvalid);
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    fs::remove_file(temp)?;
    Ok(())
}

fn replace_atomically(path: &Path, bytes: &[u8]) -> Result<(), ContextAttachmentError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let mut file = AtomicWriteFile::options().open(path)?;
    file.write_all(bytes)?;
    file.commit()?;
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = fs::metadata(parent)?;
    }
    Ok(())
}

fn temporary_sibling(path: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("{}.{}.tmp", std::process::id(), sequence))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn inline_references_are_ordered_unique_and_prefix_scoped() {
        let first = "11".repeat(32);
        let second = "22".repeat(32);
        let text = format!(
            "A [📎 first](loom-attachment:{first}) B [same](loom-attachment:{first}) C [second](loom-attachment:{second})"
        );
        assert_eq!(inline_attachment_ids(&text), vec![first, second]);
    }

    #[test]
    fn legacy_document_selection_is_synthesized_then_markers_become_authoritative() {
        let project = tempfile::tempdir().expect("project fixture");
        let path = project.path().join("legacy.md");
        fs::write(&path, "Legacy context body.").expect("write fixture");
        let attachment = import_path(project.path(), &path).expect("import fixture");
        write_contexts(
            project.path(),
            &DocumentContexts {
                schema: CONTEXT_SCHEMA_V1.to_owned(),
                documents: BTreeMap::from([("doc".to_owned(), vec![attachment.id.clone()])]),
                manual_text: BTreeMap::new(),
                text_imports: BTreeMap::new(),
            },
        )
        .expect("write legacy selection");

        assert_eq!(
            document_context_text(project.path(), "doc").expect("synthesize legacy markers"),
            attachment.inline_markdown
        );
        set_document_context_text(project.path(), "doc", "Only this prose remains.".to_owned())
            .expect("replace legacy selection");
        assert!(
            document_context(project.path(), "doc")
                .expect("read authoritative selection")
                .is_empty()
        );
        let resolved =
            resolve_for_generation(project.path(), "doc", "").expect("resolve marker-free context");
        assert!(resolved.attachment_ids.is_empty());
        assert!(
            resolved
                .context_preamble
                .contains("Only this prose remains.")
        );
        assert!(!resolved.context_preamble.contains("Legacy context body."));
    }

    #[test]
    fn add_and_remove_update_context_markdown_and_selection_in_one_write() {
        let project = tempfile::tempdir().expect("project fixture");
        let path = project.path().join("guide.md");
        fs::write(&path, "Attached guide body.").expect("write fixture");
        let attachment = import_path(project.path(), &path).expect("import fixture");
        set_document_context_text(project.path(), "doc", "Opening note.".to_owned())
            .expect("persist note");

        add_document_context(project.path(), "doc", &attachment.id).expect("add attachment");
        let added = document_context_text(project.path(), "doc").expect("read added marker");
        assert!(added.starts_with("Opening note."));
        assert!(added.contains(&attachment.inline_markdown));
        assert_eq!(
            document_context(project.path(), "doc")
                .expect("read added selection")
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![attachment.id.clone()]
        );

        remove_document_context(project.path(), "doc", &attachment.id).expect("remove attachment");
        assert_eq!(
            document_context_text(project.path(), "doc").expect("read removed marker"),
            "Opening note."
        );
        assert!(
            document_context(project.path(), "doc")
                .expect("read removed selection")
                .is_empty()
        );
        let resolved =
            resolve_for_generation(project.path(), "doc", "").expect("resolve after removal");
        assert!(resolved.attachment_ids.is_empty());
        assert!(!resolved.context_preamble.contains("Attached guide body."));
    }

    #[test]
    fn document_context_and_inline_markers_resolve_only_the_selected_scope() {
        let project = tempfile::tempdir().expect("project fixture");
        let first_path = project.path().join("guide.md");
        let second_path = project.path().join("later.md");
        fs::write(&first_path, "# Voice\n\nKeep the cadence spare.").expect("write first fixture");
        fs::write(&second_path, "Only after this marker.").expect("write second fixture");
        let first = import_path(project.path(), &first_path).expect("import first fixture");
        let second = import_path(project.path(), &second_path).expect("import second fixture");

        add_document_context(project.path(), "doc", &first.id).expect("add document context");
        let before = resolve_for_generation(project.path(), "doc", "opening")
            .expect("resolve document context");
        assert_eq!(before.attachment_ids, vec![first.id.clone()]);
        assert!(before.context_preamble.contains("Keep the cadence spare."));
        assert!(!before.context_preamble.contains("Only after this marker."));

        let after = resolve_for_generation(
            project.path(),
            "doc",
            &format!("opening\n\n{}\n\nnext", second.inline_markdown),
        )
        .expect("resolve inline context");
        assert_eq!(after.attachment_ids, vec![first.id, second.id]);
        assert!(after.context_preamble.contains("Only after this marker."));
    }

    #[test]
    fn pasted_context_is_persisted_and_bounded_for_generation() {
        let project = tempfile::tempdir().expect("project fixture");
        let pasted = "Keep the narration close and concrete. ".repeat(2_000);
        set_document_context_text(project.path(), "doc", pasted.clone())
            .expect("persist pasted context");
        assert_eq!(
            document_context_text(project.path(), "doc").expect("read pasted context"),
            pasted
        );

        let resolved = resolve_for_generation(
            project.path(),
            "doc",
            "The concrete room narrowed around the narrator.",
        )
        .expect("resolve pasted context");
        assert!(resolved.context_preamble.len() <= 16 * 1024);
        assert!(
            resolved
                .context_preamble
                .contains("AUTHOR STEERING CONTEXT")
        );
        assert!(resolved.context_preamble.contains("concrete"));
        assert!(resolved.retrieval_evidence.manual_text_sha256.is_some());
    }

    #[test]
    fn long_attachment_uses_relevant_receipted_excerpts_and_a_recent_manuscript_window() {
        let project = tempfile::tempdir().expect("project fixture");
        let path = project.path().join("long-reference.md");
        let text = format!(
            "# Front matter\n\n{}\n\n## Estuary detail\nThe kingfisher returns to the tidal estuary at dusk.\n\n{}",
            "Unrelated catalogue entry.\n".repeat(1_000),
            "Remote appendix line.\n".repeat(1_000)
        );
        fs::write(&path, text).expect("write long reference");
        let attachment = import_path(project.path(), &path).expect("import long reference");
        add_document_context(project.path(), "doc", &attachment.id).expect("add reference");
        let manuscript = format!(
            "{}The narrator waits beside the tidal estuary for the kingfisher.{}Current ending.",
            "Earlier manuscript material. ".repeat(500),
            " Later manuscript material. ".repeat(400)
        );

        let resolved = resolve_for_generation(project.path(), "doc", &manuscript)
            .expect("resolve long reference");
        assert!(resolved.context_preamble.len() <= 8 * 1024);
        assert!(
            resolved.context_preamble.contains("kingfisher returns"),
            "{}",
            resolved.context_preamble
        );
        assert!(resolved.manuscript_prompt.len() <= 32_768 - (4 * 96) - 1_024);
        assert!(
            resolved
                .manuscript_prompt
                .contains("earlier manuscript middle omitted")
        );
        assert!(resolved.manuscript_prompt.starts_with("Earlier manuscript"));
        assert!(resolved.manuscript_prompt.ends_with("Current ending."));
        assert!(
            resolved
                .retrieval_evidence
                .manuscript_omitted_start_byte
                .is_some()
        );
        assert!(
            resolved
                .retrieval_evidence
                .manuscript_omitted_end_byte
                .is_some()
        );
        assert!(!resolved.retrieval_evidence.excerpts.is_empty());
    }

    #[test]
    fn context_markers_and_retrieval_query_are_stable_between_growth_rebalances() {
        let project = tempfile::tempdir().expect("project fixture");
        let path = project.path().join("reference.md");
        fs::write(&path, "A stable attached reference.").expect("write reference");
        let attachment = import_path(project.path(), &path).expect("import reference");
        set_document_context_text(project.path(), "doc", attachment.inline_markdown.clone())
            .expect("persist context document marker");

        let first_prefix = format!("{}tail one", "stable manuscript. ".repeat(1_100));
        let second_prefix = format!("{first_prefix} and a little more");
        let first = resolve_for_generation(project.path(), "doc", &first_prefix)
            .expect("resolve first growth point");
        let second = resolve_for_generation(project.path(), "doc", &second_prefix)
            .expect("resolve second growth point");

        assert_eq!(first.attachment_ids, vec![attachment.id]);
        assert_eq!(
            first.retrieval_evidence.manuscript_query_sha256,
            second.retrieval_evidence.manuscript_query_sha256
        );
        assert_eq!(
            first.retrieval_evidence.retrieval_rebalance_epoch,
            second.retrieval_evidence.retrieval_rebalance_epoch
        );
    }

    #[test]
    fn image_and_audio_are_retained_as_exact_direct_media() {
        let project = tempfile::tempdir().expect("project fixture");
        let png_path = project.path().join("pixel.png");
        let wav_path = project.path().join("tone.wav");

        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encode PNG fixture");
        fs::write(&png_path, png.into_inner()).expect("write PNG fixture");
        fs::write(&wav_path, wav_fixture()).expect("write WAV fixture");

        let image = import_path(project.path(), &png_path).expect("import image");
        let audio = import_path(project.path(), &wav_path).expect("import audio");
        assert_eq!(image.media_kinds, vec!["image"]);
        assert_eq!(audio.media_kinds, vec!["audio"]);
        assert_eq!(image.text_bytes, 0);
        assert_eq!(audio.text_bytes, 0);

        let prefix = format!("{}\n\n{}", image.inline_markdown, audio.inline_markdown);
        let resolved =
            resolve_for_generation(project.path(), "doc", &prefix).expect("resolve native media");
        assert!(resolved.context_preamble.is_empty());
        assert_eq!(resolved.media.len(), 2);
        assert_eq!(
            resolved.media[0].bytes,
            fs::read(png_path).expect("read PNG fixture")
        );
        assert_eq!(
            resolved.media[1].bytes,
            fs::read(wav_path).expect("read WAV fixture")
        );
    }

    #[test]
    fn empty_manuscript_resolves_manual_text_attachment_image_and_audio_context() {
        let project = tempfile::tempdir().expect("project fixture");
        let text_path = project.path().join("voice.md");
        let png_path = project.path().join("pixel.png");
        let wav_path = project.path().join("tone.wav");
        fs::write(&text_path, "Keep the voice exact and quiet.").expect("write text fixture");
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encode PNG fixture");
        fs::write(&png_path, png.into_inner()).expect("write PNG fixture");
        fs::write(&wav_path, wav_fixture()).expect("write WAV fixture");
        let text = import_path(project.path(), &text_path).expect("import text");
        let image = import_path(project.path(), &png_path).expect("import image");
        let audio = import_path(project.path(), &wav_path).expect("import audio");
        let context = format!(
            "Author direction.\n\n{}\n\n{}\n\n{}",
            text.inline_markdown, image.inline_markdown, audio.inline_markdown
        );
        set_document_context_text(project.path(), "doc", context).expect("persist context");

        let resolved =
            resolve_for_generation(project.path(), "doc", "").expect("resolve empty manuscript");
        assert!(resolved.manuscript_prompt.is_empty());
        assert_eq!(resolved.attachment_ids, vec![text.id, image.id, audio.id]);
        assert!(resolved.context_preamble.contains("Author direction."));
        assert!(
            resolved
                .context_preamble
                .contains("Keep the voice exact and quiet.")
        );
        assert_eq!(
            resolved
                .media
                .iter()
                .map(|item| item.kind)
                .collect::<Vec<_>>(),
            vec![MediaKind::Image, MediaKind::Audio]
        );
    }

    #[test]
    fn projected_text_is_editable_idempotent_and_never_hidden_twice() {
        let project = tempfile::tempdir().expect("project fixture");
        let text_path = project.path().join("voice.md");
        let canonical = "Keep the voice exact and quiet.";
        fs::write(&text_path, canonical).expect("write text fixture");
        let attachment = import_path(project.path(), &text_path).expect("import text");

        let first = add_document_context_snapshot(
            project.path(),
            "doc",
            std::slice::from_ref(&attachment.id),
        )
        .expect("project text into editable context");
        assert_eq!(first.markdown, canonical);
        assert!(first.attachments.is_empty());
        assert_eq!(first.text_sources.len(), 1);

        let second = add_document_context_snapshot(
            project.path(),
            "doc",
            std::slice::from_ref(&attachment.id),
        )
        .expect("repeat projection is idempotent");
        assert_eq!(second, first);
        let resolved =
            resolve_for_generation(project.path(), "doc", "").expect("resolve projected text once");
        assert_eq!(resolved.context_preamble.matches(canonical).count(), 1);
        assert_eq!(resolved.attachment_ids, vec![attachment.id.clone()]);
        assert!(
            resolved
                .context_preamble
                .contains("[BEGIN UNTRUSTED ATTACHMENT EXCERPTS")
        );
        assert!(!resolved.context_preamble.contains("AUTHOR STEERING"));

        set_document_context_snapshot(project.path(), "doc", "", &[])
            .expect("delete projected markdown");
        let restored = add_document_context_snapshot(
            project.path(),
            "doc",
            std::slice::from_ref(&attachment.id),
        )
        .expect("re-add deleted projection");
        assert_eq!(restored.markdown, canonical);
        assert_eq!(restored.text_sources.len(), 1);
    }

    #[test]
    fn media_snapshot_hides_authority_marker_and_removal_revokes_bytes() {
        let project = tempfile::tempdir().expect("project fixture");
        let png_path = project.path().join("pixel.png");
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encode PNG fixture");
        let png = png.into_inner();
        fs::write(&png_path, &png).expect("write PNG fixture");
        let attachment = import_path(project.path(), &png_path).expect("import image");

        let snapshot = add_document_context_snapshot(
            project.path(),
            "doc",
            std::slice::from_ref(&attachment.id),
        )
        .expect("select image");
        assert!(snapshot.markdown.is_empty());
        assert_eq!(snapshot.attachments.len(), 1);
        let media = &snapshot.attachments[0].media[0];
        assert_eq!(
            read_context_media(project.path(), &attachment.id, &media.sha256)
                .expect("load selected media")
                .bytes,
            png
        );
        let resolved = resolve_for_generation(project.path(), "doc", "")
            .expect("resolve media without marker text");
        assert!(resolved.context_preamble.is_empty());
        assert_eq!(resolved.media.len(), 1);

        let removed = remove_document_context_snapshot(project.path(), "doc", &attachment.id)
            .expect("remove image authority");
        assert!(removed.attachments.is_empty());
        assert!(
            resolve_for_generation(project.path(), "doc", "")
                .expect("resolve revoked image")
                .media
                .is_empty()
        );
    }

    #[test]
    fn native_media_preflight_rejects_object_per_object_and_aggregate_overflow() {
        let project = tempfile::tempdir().expect("project fixture");
        let png_path = project.path().join("pixel.png");
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encode PNG fixture");
        fs::write(&png_path, png.into_inner()).expect("write PNG fixture");
        let attachment = import_path(project.path(), &png_path).expect("import image");
        let manifest = read_manifest(project.path(), &attachment.id).expect("read manifest");

        let mut too_many = manifest.clone();
        too_many.media = std::iter::repeat_n(too_many.media[0].clone(), 33).collect();
        assert!(matches!(
            preflight_native_media(&[(attachment.id.clone(), too_many)]),
            Err(ContextAttachmentError::Processing(_))
        ));

        let mut too_large = manifest.clone();
        too_large.media[0].byte_count = MAX_NATIVE_MEDIA_OBJECT_BYTES + 1;
        assert!(matches!(
            preflight_native_media(&[(attachment.id.clone(), too_large)]),
            Err(ContextAttachmentError::Processing(_))
        ));

        let mut aggregate = manifest;
        aggregate.media = vec![aggregate.media[0].clone(), aggregate.media[0].clone()];
        aggregate.media[0].byte_count = MAX_NATIVE_MEDIA_TOTAL_BYTES / 2 + 1;
        aggregate.media[1].byte_count = MAX_NATIVE_MEDIA_TOTAL_BYTES / 2;
        assert!(matches!(
            preflight_native_media(&[(attachment.id, aggregate)]),
            Err(ContextAttachmentError::Processing(_))
        ));
    }

    #[test]
    fn waveform_peaks_are_bounded_and_derived_only_from_valid_wav_samples() {
        assert_eq!(wav_waveform_peaks(&wav_fixture()), Some(vec![0]));
        assert_eq!(wav_waveform_peaks(b"not a wave file"), None);
    }

    #[test]
    fn middle_out_handles_tiny_utf8_budgets_without_overclaiming_retention() {
        let text = "αβ🙂tail";
        for budget in 0..=6 {
            let (rendered, omitted) = middle_out_manuscript(text, budget);
            assert!(rendered.len() <= budget);
            assert!(omitted.is_some());
            assert!(text.ends_with(&rendered));
        }

        let manuscript = format!("HEAD{}TAIL", "αβ🙂middle".repeat(2_000));
        let project = tempfile::tempdir().expect("project fixture");
        let resolved =
            resolve_for_generation_with_budget(project.path(), "doc", &manuscript, 2_048, 1, 1)
                .expect("resolve truncated UTF-8 manuscript");
        let evidence = &resolved.retrieval_evidence;
        let omitted_start = usize::try_from(
            evidence
                .manuscript_omitted_start_byte
                .expect("omitted start"),
        )
        .expect("omitted start fits usize");
        let omitted_end =
            usize::try_from(evidence.manuscript_omitted_end_byte.expect("omitted end"))
                .expect("omitted end fits usize");
        assert_eq!(
            evidence.manuscript_retained_head_end_byte,
            omitted_start as u64
        );
        assert_eq!(
            evidence.manuscript_retained_tail_start_byte,
            omitted_end as u64
        );
        assert!(manuscript.is_char_boundary(omitted_start));
        assert!(manuscript.is_char_boundary(omitted_end));
        assert!(
            resolved
                .manuscript_prompt
                .starts_with(&manuscript[..omitted_start])
        );
        assert!(
            resolved
                .manuscript_prompt
                .ends_with(&manuscript[omitted_end..])
        );
        assert_eq!(evidence.manuscript_window_start_byte, 0);
        assert_eq!(evidence.manuscript_window_end_byte, manuscript.len() as u64);
    }

    #[test]
    fn adding_context_files_is_atomic_at_the_document_limit() {
        let project = tempfile::tempdir().expect("project fixture");
        let mut attachments = Vec::new();
        for index in 0..33 {
            let path = project.path().join(format!("context-{index}.md"));
            fs::write(&path, format!("Context item {index}")).expect("write context fixture");
            attachments.push(import_path(project.path(), &path).expect("import context fixture"));
        }
        let initial = attachments[..31]
            .iter()
            .map(|attachment| attachment.id.clone())
            .collect::<Vec<_>>();
        add_document_contexts(project.path(), "doc", &initial).expect("add initial context");
        let overflow = attachments[31..]
            .iter()
            .map(|attachment| attachment.id.clone())
            .collect::<Vec<_>>();
        assert!(matches!(
            add_document_contexts(project.path(), "doc", &overflow),
            Err(ContextAttachmentError::ContextLimit)
        ));
        assert_eq!(
            document_context(project.path(), "doc")
                .expect("read unchanged context")
                .into_iter()
                .map(|attachment| attachment.id)
                .collect::<Vec<_>>(),
            initial
        );
    }

    #[test]
    #[ignore = "allocates and canonicalizes a 65 MiB real-size document"]
    fn oversized_utf8_document_is_retained_as_an_explicit_partial_excerpt() {
        let project = tempfile::tempdir().expect("project fixture");
        let path = project.path().join("oversized.md");
        let text = "Long document line with a stable UTF-8 boundary.\n"
            .repeat((65 * 1024 * 1024) / 48 + 1);
        fs::write(&path, text).expect("write oversized fixture");

        let attachment = import_path(project.path(), &path).expect("import bounded excerpt");
        assert!(!attachment.coverage_complete);
        assert!(attachment.text_bytes <= MAX_CANONICAL_TEXT_BYTES as u64);
        assert!(
            attachment
                .warnings
                .iter()
                .any(|warning| warning.contains("part"))
        );
        add_document_context(project.path(), "doc", &attachment.id).expect("add attachment");
        let resolved = resolve_for_generation(project.path(), "doc", "stable boundary")
            .expect("resolve bounded excerpts");
        assert!(resolved.context_preamble.len() <= 16 * 1024);
    }

    #[derive(Deserialize)]
    struct WildExemplar {
        format: String,
        path: PathBuf,
        representation: String,
    }

    #[test]
    #[ignore = "requires LOOM_ATTACHMENT_EXEMPLAR_MANIFEST with local wild files"]
    fn wild_format_exemplars_produce_safe_real_representations() {
        let manifest_path = std::env::var("LOOM_ATTACHMENT_EXEMPLAR_MANIFEST")
            .expect("set LOOM_ATTACHMENT_EXEMPLAR_MANIFEST");
        let exemplars: Vec<WildExemplar> =
            serde_json::from_slice(&fs::read(manifest_path).expect("read local exemplar manifest"))
                .expect("decode local exemplar manifest");
        let required = BTreeSet::from([
            "docx", "epub", "jpeg", "markdown", "pdf", "png", "wav", "xlsx",
        ]);
        assert_eq!(
            exemplars
                .iter()
                .map(|item| item.format.as_str())
                .collect::<BTreeSet<_>>(),
            required
        );

        let project = tempfile::tempdir().expect("project fixture");
        for exemplar in exemplars {
            let attachment = import_path(project.path(), &exemplar.path)
                .unwrap_or_else(|error| panic!("{} exemplar failed: {error}", exemplar.format));
            assert_eq!(attachment.detected_format, exemplar.format);
            match exemplar.representation.as_str() {
                "text" => assert!(attachment.text_bytes > 0),
                "image" => assert_eq!(attachment.media_kinds, ["image"]),
                "audio" => assert_eq!(attachment.media_kinds, ["audio"]),
                other => panic!("unknown exemplar representation {other}"),
            }
            eprintln!(
                "{} sha256={} bytes={} canonical_text_bytes={} complete={} warnings={:?}",
                exemplar.format,
                attachment.id,
                attachment.byte_count,
                attachment.text_bytes,
                attachment.coverage_complete,
                attachment.warnings
            );
        }
    }

    fn wav_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&38_u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&8_000_u32.to_le_bytes());
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&0_i16.to_le_bytes());
        bytes
    }
}
