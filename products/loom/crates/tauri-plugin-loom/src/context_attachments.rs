use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Cursor;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use atomic_write_file::AtomicWriteFile;
use attachment_native_host::{AttachmentHost, AttachmentHostConfig, ProvidedAttachment};
use attachment_native_types::{
    AttachmentReceipt, AudioPreparationPolicy, Coverage, DetectedFormat, MediaFamily,
    PreparationPlan, PreparationPolicy, PreparedPart, TargetCapabilities,
};
use image::{ImageDecoder as _, ImageEncoder as _};
use llama_native_types::{MediaInput, MediaKind};
use same_file::Handle as FileIdentityHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const MAX_ATTACHMENT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CONTEXT_ATTACHMENTS: usize = 32;
const MAX_CANONICAL_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_MANUAL_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_NATIVE_MEDIA_OBJECTS: usize = 32;
const MAX_NATIVE_MEDIA_OBJECT_BYTES: u64 = MAX_ATTACHMENT_BYTES;
const MAX_NATIVE_MEDIA_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const CONTEXT_QUERY_BYTES: usize = 16 * 1024;
const EXCERPT_CHUNK_BYTES: usize = 2 * 1024;
const PROMPT_OVERHEAD_TOKENS: u32 = 1_024;
const RETRIEVAL_REBALANCE_BYTES: usize = 16 * 1024;
const PREAMBLE_CACHE_ENTRIES: usize = 8;
const PREAMBLE_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MANIFEST_SCHEMA: &str = "loom.context-attachment.v3";
const CONTEXT_SCHEMA: &str = "loom.document-context.v4";
const RETRIEVAL_SCHEMA: &str = "loom.context-retrieval.v4";
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) editable_markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) media_markdown: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextAttachmentPresentationKind {
    Text,
    File,
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
    pub(crate) source_revision: String,
    pub(crate) excerpt: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DocumentContextSnapshot {
    /// Authored instructions only. Imported prose never enters this field.
    pub(crate) markdown: String,
    pub(crate) attachments: Vec<ContextAttachmentPresentation>,
    pub(crate) materials: Vec<ContextMaterial>,
    pub(crate) revision: String,
}

/// A selected immutable source version, optionally with an authored excerpt.
/// Editing its excerpt never turns source material into author instructions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextMaterial {
    pub(crate) attachment_id: String,
    pub(crate) source_revision: String,
    pub(crate) excerpt: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_conversion: Option<ImageConversion>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ImageConversion {
    source_sha256: String,
    method: String,
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
    documents: BTreeMap<String, DocumentContext>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DocumentContext {
    instructions: String,
    materials: Vec<ContextMaterial>,
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
    context_revision: String,
    materials: Vec<ContextMaterial>,
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
    document_id: String,
    policy: &'static str,
    attachment_ids: Vec<String>,
    attachment_text_fingerprints: Vec<String>,
    manual_text_sha256: Option<String>,
    context_revision: String,
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
    #[error("the attachment exceeds Loom's 128 MB per-file limit")]
    SourceSize,
    #[error("attachment processing failed: {0}")]
    Processing(String),
    #[error("the attachment produced no safe text or media representation")]
    NoRepresentation,
    #[error("the document context already contains Loom's 32-attachment limit")]
    ContextLimit,
    #[error("pasted context exceeds Loom's 256 KB saved-text limit")]
    ManualTextLimit,
    #[error("the attachment context is corrupt or no longer matches its content identity")]
    ContextInvalid,
    #[error(
        "saved context uses an earlier mixed-text format; its unchanged file is .loom/attachments/document-context.json. Move that file aside to retain it, then select materials afresh. Your manuscript is still editable"
    )]
    ContextFormat,
    #[error("attachment storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("attachment metadata failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
pub(crate) fn import_path(
    project_root: &Path,
    source_path: &Path,
) -> Result<StoredAttachment, ContextAttachmentError> {
    prepare_path_bounded(project_root, source_path, MAX_ATTACHMENT_BYTES)?.publish()
}

// Keep grant validation adjacent to bounded source reading and conversion.
pub(crate) fn prepare_path_bounded(
    project_root: &Path,
    source_path: &Path,
    max_bytes: u64,
) -> Result<PreparedAttachment, ContextAttachmentError> {
    let max_bytes = max_bytes.min(MAX_ATTACHMENT_BYTES);
    let file_name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .ok_or(ContextAttachmentError::UnsafeSource)?
        .to_owned();
    let metadata = fs::symlink_metadata(source_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ContextAttachmentError::UnsafeSource);
    }
    if metadata.len() == 0 || metadata.len() > max_bytes {
        return Err(ContextAttachmentError::SourceSize);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut source = options.open(source_path)?;
    if !source.metadata()?.is_file() {
        return Err(ContextAttachmentError::UnsafeSource);
    }
    let identity = FileIdentityHandle::from_file(source.try_clone()?)?;
    let provided =
        ProvidedAttachment::read_bounded(file_name.clone(), None, &mut source, max_bytes)
            .map_err(|error| ContextAttachmentError::Processing(error.safe_message))?;
    let visible = FileIdentityHandle::from_path(source_path)?;
    if visible != identity {
        return Err(ContextAttachmentError::UnsafeSource);
    }

    let attachment = prepare_provided(project_root, provided)?;
    record_import_origin(
        project_root,
        &serde_json::json!({
            "schema": "loom.file-import.v1", "source_path": source_path,
            "source_sha256": attachment.attachment.id, "source_bytes": attachment.attachment.byte_count,
            "network_used": false, "human_reviewed": false,
        }),
    )?;
    Ok(attachment)
}

/// Retains an explicitly captured recording through the same direct-media
/// pipeline as an imported WAV. Recognition is never part of this operation.
pub(crate) fn import_recorded_wav(
    project_root: &Path,
    file_name: String,
    wav: &[u8],
) -> Result<StoredAttachment, ContextAttachmentError> {
    let provided = ProvidedAttachment::read_bounded(
        file_name,
        Some("audio/wav".to_owned()),
        &mut Cursor::new(wav),
        MAX_ATTACHMENT_BYTES,
    )
    .map_err(|error| ContextAttachmentError::Processing(error.safe_message))?;
    import_provided(project_root, provided)
}

// Account downloads, folder imports, recordings, and native file grants share
// one parser and persistence path. Callers provide bytes, never URLs to the parser.
pub(crate) fn import_provided(
    project_root: &Path,
    provided: ProvidedAttachment,
) -> Result<StoredAttachment, ContextAttachmentError> {
    prepare_provided(project_root, provided)?.publish()
}

/// Conversion and immutable object staging happen before publication authority
/// is reacquired. Only the final manifest link makes an attachment selectable.
#[derive(Debug)]
pub(crate) struct PreparedAttachment {
    pub(crate) attachment: StoredAttachment,
    staged_manifest: Option<StagedManifest>,
}

#[derive(Debug)]
struct StagedManifest {
    temporary: PathBuf,
    destination: PathBuf,
}

impl Drop for StagedManifest {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temporary);
    }
}

impl PreparedAttachment {
    pub(crate) fn publish(self) -> Result<StoredAttachment, ContextAttachmentError> {
        if let Some(staged) = &self.staged_manifest {
            // No conversion, serialization, hashing, or payload copying here.
            // A concurrent conflicting publisher must be retried, never replaced.
            fs::hard_link(&staged.temporary, &staged.destination)?;
        }
        Ok(self.attachment)
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn prepare_provided(
    project_root: &Path,
    provided: ProvidedAttachment,
) -> Result<PreparedAttachment, ContextAttachmentError> {
    let mut file_name = provided.display_name.clone();
    let byte_count = provided.bytes.len() as u64;
    if byte_count == 0 || byte_count > MAX_ATTACHMENT_BYTES {
        return Err(ContextAttachmentError::SourceSize);
    }
    let original = std::sync::Arc::clone(&provided.bytes);
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
                if blob.media_type == "image/gif" {
                    warnings.push("The model receives the first frame of GIF images; the original animation is retained.".to_owned());
                }
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
                    image_conversion: None,
                    waveform_peaks: (kind == MediaKind::Audio)
                        .then(|| wav_waveform_peaks(bytes))
                        .flatten(),
                });
            }
            PreparedPart::OpaqueReference { .. } => {}
        }
    }
    let root_format = prepared
        .bundle
        .graph
        .objects
        .iter()
        .find(|object| object.id == prepared.bundle.graph.root)
        .and_then(|object| object.detection.selected);
    if coverage_complete
        && matches!(
            root_format,
            Some(DetectedFormat::Webp | DetectedFormat::Bmp | DetectedFormat::Tiff)
        )
    {
        match image_as_png(&original) {
            Ok(bytes) => {
                let sha256 = format!("{:x}", Sha256::digest(&bytes));
                install_object(project_root, &sha256, &bytes)?;
                media_kinds.insert("image".to_owned());
                stored_media.push(StoredMedia {
                    id: sha256.clone(),
                    kind: MediaKind::Image,
                    mime: "image/png".to_owned(),
                    sha256,
                    byte_count: bytes.len() as u64,
                    waveform_peaks: None,
                    image_conversion: Some(ImageConversion {
                        source_sha256: prepared.bundle.graph.root.0.clone(),
                        method: "bounded-first-frame-png-v1".to_owned(),
                    }),
                });
                warnings.push("A PNG of the first image or page is sent to the model; the exact original file is retained.".to_owned());
            }
            Err(message) => warnings.push(message),
        }
    }
    let canonical_text = canonical_text.join("\n\n");
    if canonical_text.len() > MAX_CANONICAL_TEXT_BYTES {
        return Err(ContextAttachmentError::Processing(
            "canonical attachment text exceeded Loom's 64 MB retained-text limit".to_owned(),
        ));
    }
    let id = prepared.bundle.graph.root.0.clone();
    if prepared.bundle.graph.objects.iter().any(|object| {
        object.id == prepared.bundle.graph.root
            && object.detection.selected == Some(DetectedFormat::Executable)
    }) {
        return Err(ContextAttachmentError::NoRepresentation);
    }
    // Retaining an original does not assert extraction or model support.
    // Identity is the root's content hash, shared with its immutable receipt.
    install_object(project_root, &id, &original)?;
    if canonical_text.is_empty() && stored_media.is_empty() {
        warnings.push(
            "Original file retained. No supported text or native media was extracted; this file is not sent to the model.".to_owned(),
        );
    }

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
    if detected_format == "email"
        && file_name.starts_with("Gmail message ")
        && let Some(subject) = canonical_text
            .lines()
            .find_map(|line| line.strip_prefix("**Subject:** "))
    {
        file_name = format!("{}.eml", subject.chars().take(160).collect::<String>());
    }
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
        byte_count,
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
                            .map(|blocker| &blocker.safe_message),
                    )
                    .cloned(),
            )
            .collect(),
        inline_markdown,
        editable_markdown: None,
        media_markdown: None,
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
        let mut result = existing.attachment;
        result.media_markdown = editor_media_markdown(&result, &existing.media);
        result.editable_markdown = (!canonical_text.is_empty()).then_some(canonical_text);
        return Ok(PreparedAttachment {
            attachment: result,
            staged_manifest: None,
        });
    }
    let destination = attachment_root(project_root)?
        .join("manifests")
        .join(format!("{}.json", manifest.attachment.id));
    let staged = StagedManifest {
        temporary: temporary_sibling(&destination),
        destination,
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged.temporary)?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.sync_all()?;
    let mut result = attachment;
    result.media_markdown = editor_media_markdown(&result, &manifest.media);
    result.editable_markdown = (!canonical_text.is_empty()).then_some(canonical_text);
    Ok(PreparedAttachment {
        attachment: result,
        staged_manifest: Some(staged),
    })
}

#[cfg(test)]
fn document_context(
    project_root: &Path,
    document_id: &str,
) -> Result<Vec<StoredAttachment>, ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    attachments_for_ids(
        project_root,
        &authoritative_context_ids(&contexts, document_id),
    )
}

#[cfg(test)]
fn document_context_text(
    project_root: &Path,
    document_id: &str,
) -> Result<String, ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    Ok(contexts
        .documents
        .get(document_id)
        .map_or_else(String::new, |context| context.instructions.clone()))
}

pub(crate) fn document_context_snapshot(
    project_root: &Path,
    document_id: &str,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    snapshot_from_contexts(project_root, &read_contexts(project_root)?, document_id)
}

pub(crate) fn set_document_context_snapshot(
    project_root: &Path,
    document_id: &str,
    markdown: &str,
    attachment_ids: &[String],
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    set_document_context_snapshot_with_materials(
        project_root,
        document_id,
        markdown,
        attachment_ids,
        None,
    )
}

pub(crate) fn set_document_context_snapshot_with_materials(
    project_root: &Path,
    document_id: &str,
    markdown: &str,
    attachment_ids: &[String],
    materials: Option<&[ContextMaterial]>,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    if markdown.len() > MAX_MANUAL_CONTEXT_BYTES {
        return Err(ContextAttachmentError::ManualTextLimit);
    }
    let ids = ordered_unique_attachment_ids(attachment_ids)?;
    let manifests = manifest_metadata_for_ids(project_root, &ids)?;
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    let previous = contexts
        .documents
        .get(document_id)
        .cloned()
        .unwrap_or_default();
    let supplied = materials.unwrap_or(&previous.materials);
    if materials.is_some()
        && supplied
            .iter()
            .map(|material| &material.attachment_id)
            .collect::<Vec<_>>()
            != ids.iter().collect::<Vec<_>>()
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let mut selected = Vec::with_capacity(manifests.len());
    for (id, manifest) in manifests {
        let material = supplied
            .iter()
            .find(|material| material.attachment_id == id)
            .cloned()
            .unwrap_or(ContextMaterial {
                attachment_id: id,
                source_revision: manifest_revision(&manifest)?,
                excerpt: None,
            });
        validate_material(&material, &manifest)?;
        selected.push(material);
    }
    if selected
        .iter()
        .filter_map(|material| material.excerpt.as_ref())
        .map(String::len)
        .sum::<usize>()
        > MAX_MANUAL_CONTEXT_BYTES
    {
        return Err(ContextAttachmentError::ManualTextLimit);
    }
    contexts.documents.insert(
        document_id.to_owned(),
        DocumentContext {
            instructions: markdown.to_owned(),
            materials: selected,
        },
    );
    write_contexts(project_root, &contexts)?;
    snapshot_from_contexts(project_root, &contexts, document_id)
}

pub(crate) fn add_document_context_snapshot(
    project_root: &Path,
    document_id: &str,
    attachment_ids: &[String],
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    let requested = manifest_metadata_for_ids(
        project_root,
        &ordered_unique_attachment_ids(attachment_ids)?,
    )?;
    let _guard = CONTEXT_WRITE_LOCK
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    let mut contexts = read_contexts(project_root)?;
    let context = contexts
        .documents
        .entry(document_id.to_owned())
        .or_default();
    for (id, manifest) in requested {
        if let Some(existing) = context
            .materials
            .iter()
            .find(|material| material.attachment_id == id)
        {
            validate_material(existing, &manifest)?;
            continue;
        }
        if context.materials.len() >= MAX_CONTEXT_ATTACHMENTS {
            return Err(ContextAttachmentError::ContextLimit);
        }
        context.materials.push(ContextMaterial {
            attachment_id: id,
            source_revision: manifest_revision(&manifest)?,
            excerpt: None,
        });
    }
    write_contexts(project_root, &contexts)?;
    snapshot_from_contexts(project_root, &contexts, document_id)
}

#[cfg(test)]
fn set_material_excerpt(
    project_root: &Path,
    document_id: &str,
    attachment_id: &str,
    source_revision: &str,
    excerpt: Option<String>,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    let snapshot = document_context_snapshot(project_root, document_id)?;
    let mut materials = snapshot.materials;
    let material = materials
        .iter_mut()
        .find(|material| {
            material.attachment_id == attachment_id && material.source_revision == source_revision
        })
        .ok_or(ContextAttachmentError::ContextInvalid)?;
    material.excerpt = excerpt;
    let ids = materials
        .iter()
        .map(|material| material.attachment_id.clone())
        .collect::<Vec<_>>();
    set_document_context_snapshot_with_materials(
        project_root,
        document_id,
        &snapshot.markdown,
        &ids,
        Some(&materials),
    )
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
    if let Some(context) = contexts.documents.get_mut(document_id) {
        context
            .materials
            .retain(|material| material.attachment_id != attachment_id);
    }
    write_contexts(project_root, &contexts)?;
    snapshot_from_contexts(project_root, &contexts, document_id)
}

fn manifest_revision(manifest: &AttachmentManifest) -> Result<String, ContextAttachmentError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(manifest)?)
    ))
}

fn validate_material(
    material: &ContextMaterial,
    manifest: &AttachmentManifest,
) -> Result<(), ContextAttachmentError> {
    if material.attachment_id != manifest.attachment.id
        || material.source_revision != manifest_revision(manifest)?
        || material.excerpt.as_ref().is_some_and(|text| {
            manifest.attachment.text_bytes == 0 || text.len() > MAX_MANUAL_CONTEXT_BYTES
        })
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(())
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
    add_document_context_snapshot(project_root, document_id, attachment_ids)?;
    document_context(project_root, document_id)
}

#[cfg(test)]
fn remove_document_context(
    project_root: &Path,
    document_id: &str,
    attachment_id: &str,
) -> Result<Vec<StoredAttachment>, ContextAttachmentError> {
    remove_document_context_snapshot(project_root, document_id, attachment_id)?;
    document_context(project_root, document_id)
}

fn authoritative_context_ids(contexts: &DocumentContexts, document_id: &str) -> Vec<String> {
    contexts
        .documents
        .get(document_id)
        .map(|context| {
            context
                .materials
                .iter()
                .map(|material| material.attachment_id.clone())
                .collect()
        })
        .unwrap_or_default()
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

fn ordered_unique_attachment_ids(
    attachment_ids: &[String],
) -> Result<Vec<String>, ContextAttachmentError> {
    let mut ids = Vec::new();
    for id in attachment_ids {
        if !is_sha256(id) {
            return Err(ContextAttachmentError::ContextInvalid);
        }
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    if ids.len() > MAX_CONTEXT_ATTACHMENTS {
        return Err(ContextAttachmentError::ContextLimit);
    }
    Ok(ids)
}

fn manifest_metadata_for_ids(
    project_root: &Path,
    ids: &[String],
) -> Result<Vec<(String, AttachmentManifest)>, ContextAttachmentError> {
    ids.iter()
        .map(|id| read_manifest_metadata(project_root, id).map(|manifest| (id.clone(), manifest)))
        .collect()
}

fn snapshot_from_contexts(
    project_root: &Path,
    contexts: &DocumentContexts,
    document_id: &str,
) -> Result<DocumentContextSnapshot, ContextAttachmentError> {
    let context = contexts
        .documents
        .get(document_id)
        .cloned()
        .unwrap_or_default();
    let mut attachments = Vec::with_capacity(context.materials.len());
    for material in &context.materials {
        let manifest = read_manifest_metadata(project_root, &material.attachment_id)?;
        validate_material(material, &manifest)?;
        let mut presentation = attachment_presentation(manifest);
        presentation
            .source_revision
            .clone_from(&material.source_revision);
        presentation.excerpt.clone_from(&material.excerpt);
        attachments.push(presentation);
    }
    Ok(DocumentContextSnapshot {
        revision: context_revision(&context)?,
        markdown: context.instructions,
        attachments,
        materials: context.materials,
    })
}

fn context_revision(context: &DocumentContext) -> Result<String, ContextAttachmentError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(context)?)
    ))
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
        (false, false, false) => ContextAttachmentPresentationKind::File,
        (true, false, false) => ContextAttachmentPresentationKind::Text,
        (false, true, false) => ContextAttachmentPresentationKind::Image,
        (false, false, true) => ContextAttachmentPresentationKind::Audio,
        _ => ContextAttachmentPresentationKind::Mixed,
    };
    ContextAttachmentPresentation {
        source_revision: String::new(),
        excerpt: None,
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

#[allow(clippy::too_many_lines)]
pub(crate) fn resolve_for_generation_with_budget(
    project_root: &Path,
    document_id: &str,
    manuscript_prefix: &str,
    context_window_tokens: u32,
    branch_count: u32,
    max_generated_tokens: u32,
) -> Result<ResolvedContext, ContextAttachmentError> {
    let (context, manifests) = admit_context(project_root, document_id, manuscript_prefix)?;
    let ordered_ids = manifests
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let manual_text = &context.instructions;
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
    let rebalance_epoch = (stable_query_end / RETRIEVAL_REBALANCE_BYTES) as u64;
    let cache_key = PreambleCacheKey {
        project_root: project_root.to_path_buf(),
        document_id: document_id.to_owned(),
        policy: RETRIEVAL_SCHEMA,
        attachment_ids: ordered_ids.clone(),
        attachment_text_fingerprints: manifests
            .iter()
            .map(|(id, manifest)| attachment_text_fingerprint(id, manifest))
            .collect(),
        manual_text_sha256: manual_text_sha256.clone(),
        context_revision: context_revision(&context)?,
        manuscript_query_sha256: query_sha256.clone(),
        retrieval_rebalance_epoch: rebalance_epoch,
        context_budget,
    };
    let (text, excerpts) = if let Some(cached) = cached_preamble(&cache_key) {
        (cached.text, cached.excerpts)
    } else {
        let mut sources = Vec::new();
        for (id, manifest) in &manifests {
            let canonical_text = match context
                .materials
                .iter()
                .find(|material| material.attachment_id == *id)
                .and_then(|material| material.excerpt.as_ref())
            {
                Some(excerpt) => excerpt.clone(),
                None => read_canonical_text(project_root, manifest)?,
            };
            if !canonical_text.is_empty() {
                sources.push((id.clone(), manifest.attachment.clone(), canonical_text));
            }
        }
        let (text, excerpts) =
            assemble_generation_context(manual_text, &sources, query, context_budget);
        cache_preamble(
            cache_key,
            PreambleCacheValue {
                text: text.clone(),
                excerpts: excerpts.clone(),
            },
        );
        (text, excerpts)
    };
    let media = load_admitted_media(project_root, manifests)?;
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
            context_revision: context_revision(&context)?,
            materials: context.materials,
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

/// Admission uses current selected source revisions, never a cached selection.
/// Cached text is an immutable derivative of previously hash-verified bytes:
/// disk changes cannot modify it. A cache miss always verifies source bytes.
fn admit_context(
    project_root: &Path,
    document_id: &str,
    manuscript: &str,
) -> Result<(DocumentContext, Vec<(String, AttachmentManifest)>), ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    let context = contexts
        .documents
        .get(document_id)
        .cloned()
        .unwrap_or_default();
    let mut ids = authoritative_context_ids(&contexts, document_id);
    for id in inline_attachment_ids(manuscript) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let ids = ordered_unique_attachment_ids(&ids)?;
    let manifests = manifest_metadata_for_ids(project_root, &ids)?;
    for (id, manifest) in &manifests {
        if let Some(material) = context
            .materials
            .iter()
            .find(|material| material.attachment_id == *id)
        {
            validate_material(material, manifest)?;
        }
        // Existence and shape checks establish current availability, not content
        // integrity. Only verified retained derivatives may skip payload reads.
        check_object_metadata(project_root, id, manifest.attachment.byte_count)?;
        if let Some(hash) = &manifest.canonical_text_sha256 {
            check_object_metadata(project_root, hash, manifest.attachment.text_bytes)?;
        }
    }
    preflight_native_media(&manifests)?;
    Ok((context, manifests))
}

pub(crate) fn resolve_media_for_document(
    project_root: &Path,
    document_id: &str,
    manuscript: &str,
) -> Result<Vec<MediaInput>, ContextAttachmentError> {
    let (_, manifests) = admit_context(project_root, document_id, manuscript)?;
    load_admitted_media(project_root, manifests)
}

fn load_admitted_media(
    project_root: &Path,
    manifests: Vec<(String, AttachmentManifest)>,
) -> Result<Vec<MediaInput>, ContextAttachmentError> {
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
    Ok(media)
}

fn check_object_metadata(
    project_root: &Path,
    sha256: &str,
    expected_bytes: u64,
) -> Result<(), ContextAttachmentError> {
    if !is_sha256(sha256) || expected_bytes > MAX_ATTACHMENT_BYTES {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let metadata =
        fs::symlink_metadata(attachment_root(project_root)?.join("objects").join(sha256))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != expected_bytes
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct SourceWork {
    read: usize,
    hashed: usize,
    prepared_copied: usize,
}
#[cfg(test)]
thread_local! { static SOURCE_WORK: std::cell::Cell<SourceWork> = const { std::cell::Cell::new(SourceWork { read: 0, hashed: 0, prepared_copied: 0 }) }; }

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
    #[cfg(test)]
    SOURCE_WORK.with(|counter| {
        let mut work = counter.get();
        work.prepared_copied += value.text.len();
        counter.set(work);
    });
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
    #[cfg(test)]
    if source_id != "manual" {
        SOURCE_WORK.with(|counter| {
            let mut work = counter.get();
            work.hashed += text.len();
            counter.set(work);
        });
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
            let mut floor = start + (end - start) / 2;
            while !text.is_char_boundary(floor) {
                floor -= 1;
            }
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

fn image_as_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
    struct Output(Vec<u8>);
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_PNG_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("PNG exceeds 16 MiB"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let format = image::guess_format(bytes).map_err(|error| error.to_string())?;
    if !matches!(
        format,
        image::ImageFormat::WebP | image::ImageFormat::Bmp | image::ImageFormat::Tiff
    ) {
        return Err("No image conversion is configured for this file.".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 16 * 1024 * 1024 {
        return Err("Image conversion exceeds its 16-megapixel limit.".into());
    }
    // Match ImageReader::decode's accounting while keeping the dimension
    // check and conversion on one bounded decoder.
    limits
        .reserve(decoder.total_bytes())
        .map_err(|error| error.to_string())?;
    decoder
        .set_limits(limits)
        .map_err(|error| error.to_string())?;
    let pixels = image::DynamicImage::from_decoder(decoder)
        .map_err(|error| error.to_string())?
        .into_rgba8();
    let mut output = Output(Vec::new());
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            &pixels,
            pixels.width(),
            pixels.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| error.to_string())?;
    Ok(output.0)
}

fn gemma_target() -> TargetCapabilities {
    TargetCapabilities {
        target_id: "google.gemma-4-12b-it".to_owned(),
        fingerprint: "loom:gemma-4-12b-it:native-image-audio:excerpted-text:v2".to_owned(),
        // The pinned mtmd decoder does not support raw WebP or TIFF.
        accepted_media_types: ["image/png", "image/jpeg", "image/gif", "audio/wav"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        accepted_media_families: BTreeSet::new(),
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

fn editor_media_markdown(attachment: &StoredAttachment, media: &[StoredMedia]) -> Option<String> {
    if media.is_empty() {
        return None;
    }
    let name = attachment
        .file_name
        .replace(['[', ']', '\\', '\n', '\r'], " ");
    Some(
        media
            .iter()
            .map(|item| {
                let kind = if item.kind == MediaKind::Audio {
                    "Audio"
                } else {
                    "Image"
                };
                let waveform = item
                    .waveform_peaks
                    .as_ref()
                    .map_or_else(String::new, |peaks| {
                        const DIGITS: &[u8; 16] = b"0123456789abcdef";
                        let mut hex = String::with_capacity(peaks.len() * 2);
                        for peak in peaks {
                            hex.push(char::from(DIGITS[usize::from(peak >> 4)]));
                            hex.push(char::from(DIGITS[usize::from(peak & 15)]));
                        }
                        format!(" \"loom-waveform:{hex}\"")
                    });
                format!(
                    "![{kind}: {name}](loom-attachment:{}/{}{waveform})",
                    attachment.id, item.sha256
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

pub(crate) fn manuscript_selects_attachment(markdown: &str, id: &str) -> bool {
    inline_attachment_ids(markdown)
        .iter()
        .any(|selected| selected == id)
}

fn inline_attachment_ids(markdown: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let marker = "(loom-attachment:";
    let mut rest = markdown;
    while let Some(start) = rest.find(marker) {
        rest = &rest[start + marker.len()..];
        let Some(end) = rest.find(')') else { break };
        let address = rest[..end].split_whitespace().next().unwrap_or_default();
        let candidate = if let Some((id, media)) = address.split_once('/') {
            if !is_sha256(media) {
                rest = &rest[end + 1..];
                continue;
            }
            id
        } else {
            address
        };
        if is_sha256(candidate) && !ids.iter().any(|id| id == candidate) {
            ids.push(candidate.to_owned());
        }
        rest = &rest[end + 1..];
    }
    ids
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
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != expected_bytes
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let file = File::open(&path)?;
    let identity = FileIdentityHandle::from_file(file.try_clone()?)?;
    let mut bytes = Vec::new();
    file.take(expected_bytes + 1).read_to_end(&mut bytes)?;
    if FileIdentityHandle::from_path(&path)? != identity
        || fs::symlink_metadata(&path)?.file_type().is_symlink()
        || bytes.len() as u64 != expected_bytes
        || format!("{:x}", Sha256::digest(&bytes)) != sha256
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(bytes)
}

pub(crate) fn original_path(
    project_root: &Path,
    id: &str,
) -> Result<PathBuf, ContextAttachmentError> {
    let manifest = read_manifest_metadata(project_root, id)?;
    let _ = read_object(project_root, id, manifest.attachment.byte_count)?;
    Ok(attachment_root(project_root)?.join("objects").join(id))
}

/// Acquisition provenance is separate from the offline processing receipt.
/// The receipt is immutable and contains no OAuth credential material.
pub(crate) fn record_import_origin(
    project_root: &Path,
    origin: &impl Serialize,
) -> Result<(), ContextAttachmentError> {
    let bytes = serde_json::to_vec_pretty(origin)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let path = attachment_root(project_root)?
        .join("manifests")
        .join(format!("source-{hash}.json"));
    install_immutable(&path, &bytes)
}

#[cfg(test)]
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
        _ => false,
    };
    let target_fingerprint_is_valid =
        manifest.preparation_plan.target_fingerprint == gemma_target().fingerprint;
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
                || media.image_conversion.as_ref().is_some_and(|conversion| {
                    conversion.source_sha256 != manifest.attachment.id
                        || conversion.method != "bounded-first-frame-png-v1"
                        || media.kind != MediaKind::Image
                        || media.mime != "image/png"
                })
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
        (None, Some(sha256)) if manifest.schema == MANIFEST_SCHEMA => {
            let bytes = read_object(project_root, sha256, manifest.attachment.text_bytes)?;
            #[cfg(test)]
            SOURCE_WORK.with(|counter| {
                let mut work = counter.get();
                work.read += bytes.len();
                work.hashed += bytes.len();
                counter.set(work);
            });
            String::from_utf8(bytes).map_err(|_| ContextAttachmentError::ContextInvalid)
        }
        (None, None) if manifest.attachment.text_bytes == 0 => Ok(String::new()),
        _ => Err(ContextAttachmentError::ContextInvalid),
    }
}

fn read_contexts(project_root: &Path) -> Result<DocumentContexts, ContextAttachmentError> {
    #[derive(Deserialize)]
    struct Header {
        schema: String,
    }
    let path = attachment_root(project_root)?.join("document-context.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DocumentContexts {
                schema: CONTEXT_SCHEMA.to_owned(),
                documents: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    // Mixed-text historical formats cannot prove which bytes were instructions.
    // Reject without rewriting or promoting any existing source material.
    if serde_json::from_slice::<Header>(&bytes)?.schema != CONTEXT_SCHEMA {
        return Err(ContextAttachmentError::ContextFormat);
    }
    let contexts: DocumentContexts = serde_json::from_slice(&bytes)?;
    if contexts.schema != CONTEXT_SCHEMA
        || contexts.documents.values().any(|context| {
            context.instructions.len() > MAX_MANUAL_CONTEXT_BYTES
                || context
                    .materials
                    .iter()
                    .filter_map(|material| material.excerpt.as_ref())
                    .map(String::len)
                    .sum::<usize>()
                    > MAX_MANUAL_CONTEXT_BYTES
                || context.materials.len() > MAX_CONTEXT_ATTACHMENTS
                || context.materials.iter().any(|material| {
                    !is_sha256(&material.attachment_id)
                        || !is_sha256(&material.source_revision)
                        || material
                            .excerpt
                            .as_ref()
                            .is_some_and(|text| text.len() > MAX_MANUAL_CONTEXT_BYTES)
                })
                || context
                    .materials
                    .iter()
                    .map(|material| &material.attachment_id)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != context.materials.len()
        })
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(contexts)
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
    fn explicit_material_edits_preserve_instructions_and_exact_source_identity() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("source.md");
        let shared = "Même phrase 🖋\r\n";
        fs::write(&path, shared).unwrap();
        let attachment = import_path(project.path(), &path).unwrap();
        let instructions = format!("{shared}Keep my instructions unchanged.");
        let initial = set_document_context_snapshot(
            project.path(),
            "doc",
            &instructions,
            std::slice::from_ref(&attachment.id),
        )
        .unwrap();
        let revision = initial.materials[0].source_revision.clone();
        let edited = set_material_excerpt(
            project.path(),
            "doc",
            &attachment.id,
            &revision,
            Some(format!("{shared}Edited source, not instructions.")),
        )
        .unwrap();
        assert_eq!(edited.markdown, instructions);
        assert_ne!(edited.revision, initial.revision);
        let result = resolve_for_generation(project.path(), "doc", "").unwrap();
        assert!(
            result
                .context_preamble
                .contains(&format!("[BEGIN AUTHOR STEERING CONTEXT]\n{instructions}"))
        );
        assert!(
            result
                .context_preamble
                .contains("Edited source, not instructions.")
        );
        assert_eq!(result.retrieval_evidence.materials, edited.materials);
        assert_eq!(result.retrieval_evidence.context_revision, edited.revision);
        assert_eq!(result.attachment_ids, vec![attachment.id.clone()]);
        let removed =
            remove_document_context_snapshot(project.path(), "doc", &attachment.id).unwrap();
        assert_eq!(removed.markdown, instructions);
        assert!(removed.materials.is_empty());
        let result = resolve_for_generation(project.path(), "doc", "").unwrap();
        assert!(!result.context_preamble.contains("Edited source"));
        assert!(
            set_material_excerpt(
                project.path(),
                "doc",
                &attachment.id,
                &revision,
                Some("late edit".into())
            )
            .is_err()
        );
    }

    #[test]
    fn removing_mixed_material_revokes_both_text_and_media() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("image.png");
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        fs::write(&path, bytes.into_inner()).unwrap();
        let attachment = import_path(project.path(), &path).unwrap();
        let mut manifest = read_manifest_metadata(project.path(), &attachment.id).unwrap();
        let text = "Description retained alongside the picture.";
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        install_object(project.path(), &hash, text.as_bytes()).unwrap();
        manifest.canonical_text_sha256 = Some(hash);
        manifest.attachment.text_bytes = text.len() as u64;
        let manifest_path = attachment_root(project.path())
            .unwrap()
            .join("manifests")
            .join(format!("{}.json", attachment.id));
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        add_document_context_snapshot(project.path(), "doc", std::slice::from_ref(&attachment.id))
            .unwrap();
        let before = resolve_for_generation(project.path(), "doc", "").unwrap();
        assert_eq!(before.media.len(), 1);
        assert!(before.context_preamble.contains(text));
        remove_document_context_snapshot(project.path(), "doc", &attachment.id).unwrap();
        let after = resolve_for_generation(project.path(), "doc", "").unwrap();
        assert!(after.media.is_empty());
        assert!(after.context_preamble.is_empty());
    }

    #[test]
    fn warm_context_reads_no_source_payload_and_only_copies_bounded_prepared_text() {
        for size in [4096, 65_536, 1_048_576] {
            let project = tempfile::tempdir().unwrap();
            let path = project.path().join("source.md");
            fs::write(&path, "Source words.\n".repeat(size / 14 + 1)).unwrap();
            let attachment = import_path(project.path(), &path).unwrap();
            add_document_context_snapshot(
                project.path(),
                "doc",
                std::slice::from_ref(&attachment.id),
            )
            .unwrap();
            SOURCE_WORK.with(|counter| counter.set(SourceWork::default()));
            let cold =
                resolve_for_generation_with_budget(project.path(), "doc", "Source", 8192, 1, 128)
                    .unwrap();
            let cold_work = SOURCE_WORK.with(std::cell::Cell::get);
            SOURCE_WORK.with(|counter| counter.set(SourceWork::default()));
            let warm =
                resolve_for_generation_with_budget(project.path(), "doc", "Source", 8192, 1, 128)
                    .unwrap();
            let warm_work = SOURCE_WORK.with(std::cell::Cell::get);
            assert_eq!(cold, warm);
            assert!(cold_work.read >= size);
            assert_eq!(warm_work.read, 0);
            assert_eq!(warm_work.hashed, 0);
            assert_eq!(warm_work.prepared_copied, warm.context_preamble.len());
            assert!(warm_work.prepared_copied <= 8192);
            eprintln!("source={size} cold={cold_work:?} warm={warm_work:?}");
            SOURCE_WORK.with(|counter| counter.set(SourceWork::default()));
            assert!(
                resolve_media_for_document(project.path(), "doc", "")
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(SOURCE_WORK.with(std::cell::Cell::get).read, 0);
        }
    }

    #[test]
    fn warm_context_rejects_replaced_source_metadata_before_cache_reuse() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("source.md");
        fs::write(&path, "Original verified source.").unwrap();
        let attachment = import_path(project.path(), &path).unwrap();
        add_document_context_snapshot(project.path(), "doc", std::slice::from_ref(&attachment.id))
            .unwrap();
        resolve_for_generation(project.path(), "doc", "").unwrap();
        let mut manifest = read_manifest_metadata(project.path(), &attachment.id).unwrap();
        manifest.attachment.file_name = "Changed source.md".into();
        manifest.attachment.inline_markdown =
            inline_attachment_markdown(&attachment.id, &manifest.attachment.file_name);
        let manifest_path = attachment_root(project.path())
            .unwrap()
            .join("manifests")
            .join(format!("{}.json", attachment.id));
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(document_context_snapshot(project.path(), "doc").is_err());
        assert!(resolve_for_generation(project.path(), "doc", "").is_err());
        remove_document_context_snapshot(project.path(), "doc", &attachment.id).unwrap();
        assert!(
            resolve_for_generation(project.path(), "doc", "")
                .unwrap()
                .attachment_ids
                .is_empty()
        );
    }

    #[test]
    fn warm_material_revalidates_selection_and_never_uses_replaced_payload_bytes() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("source.md");
        fs::write(&path, "Verified source text.").unwrap();
        let attachment = import_path(project.path(), &path).unwrap();
        add_document_context_snapshot(project.path(), "doc", std::slice::from_ref(&attachment.id))
            .unwrap();
        let cold = resolve_for_generation(project.path(), "doc", "").unwrap();
        let manifest = read_manifest_metadata(project.path(), &attachment.id).unwrap();
        let object = attachment_root(project.path())
            .unwrap()
            .join("objects")
            .join(manifest.canonical_text_sha256.as_ref().unwrap());
        fs::write(
            &object,
            vec![b'X'; usize::try_from(manifest.attachment.text_bytes).unwrap()],
        )
        .unwrap();
        let warm = resolve_for_generation(project.path(), "doc", "").unwrap();
        assert_eq!(warm.context_preamble, cold.context_preamble); // Already verified immutable derivative.
        assert!(resolve_for_generation(project.path(), "doc", "new query misses cache").is_err());
        fs::remove_file(object).unwrap();
        assert!(resolve_for_generation(project.path(), "doc", "").is_err());
        remove_document_context_snapshot(project.path(), "doc", &attachment.id).unwrap();
        assert!(
            resolve_for_generation(project.path(), "doc", "")
                .unwrap()
                .attachment_ids
                .is_empty()
        );
    }

    #[test]
    fn source_only_files_retain_originals_without_inventing_model_content() {
        let project = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let bytes = b"\x00\xff\xfe\xfd\x80\x81\x82\x83";
        let path = source.path().join("Archive.bin");
        fs::write(&path, bytes).unwrap();
        let attachment = import_path(project.path(), &path).unwrap();
        assert_eq!(attachment.text_bytes, 0);
        assert!(attachment.media_kinds.is_empty());
        assert!(attachment.editable_markdown.is_none());
        assert!(
            attachment
                .warnings
                .iter()
                .any(|warning| warning.contains("not sent to the model"))
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            fs::read(original_path(project.path(), &attachment.id).unwrap()).unwrap(),
            bytes
        );
        let snapshot = add_document_context_snapshot(
            project.path(),
            "doc",
            std::slice::from_ref(&attachment.id),
        )
        .unwrap();
        assert_eq!(
            snapshot.attachments[0].presentation_kind,
            ContextAttachmentPresentationKind::File
        );
        let persisted = set_document_context_snapshot(
            project.path(),
            "doc",
            "",
            std::slice::from_ref(&attachment.id),
        )
        .unwrap();
        assert_eq!(persisted.attachments.len(), 1);
        let resolved =
            resolve_for_generation_with_budget(project.path(), "doc", "Writing", 32768, 4, 128)
                .unwrap();
        assert!(resolved.media.is_empty());
        assert!(resolved.context_preamble.is_empty());
        assert_eq!(resolved.manuscript_prompt, "Writing");
        assert!(
            remove_document_context_snapshot(project.path(), "doc", &attachment.id)
                .unwrap()
                .attachments
                .is_empty()
        );
    }

    #[test]
    fn common_web_images_resolve_exact_native_bytes_and_keep_truncated_sources_opaque() {
        let project = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        for (extension, format) in [
            ("gif", image::ImageFormat::Gif),
            ("webp", image::ImageFormat::WebP),
            ("bmp", image::ImageFormat::Bmp),
            ("tiff", image::ImageFormat::Tiff),
        ] {
            let mut encoded = Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(2, 2)
                .write_to(&mut encoded, format)
                .unwrap();
            let bytes = encoded.into_inner();
            let path = source.path().join(format!("picture.{extension}"));
            fs::write(&path, &bytes).unwrap();
            let attachment = import_path(project.path(), &path).unwrap();
            let markdown = attachment.media_markdown.unwrap();
            let resolved =
                resolve_for_generation_with_budget(project.path(), "doc", &markdown, 32768, 4, 128)
                    .unwrap();
            assert_eq!(resolved.media.len(), 1);
            assert_eq!(resolved.media[0].kind, MediaKind::Image);
            if extension == "gif" {
                assert_eq!(resolved.media[0].bytes, bytes);
            } else {
                assert_eq!(resolved.media[0].mime, "image/png");
                let manifest = read_manifest(project.path(), &attachment.id).unwrap();
                assert_eq!(
                    manifest.media[0]
                        .image_conversion
                        .as_ref()
                        .unwrap()
                        .source_sha256,
                    attachment.id
                );
                let decoded = image::load_from_memory(&resolved.media[0].bytes).unwrap();
                assert_eq!((decoded.width(), decoded.height()), (2, 2));
                assert_eq!(
                    fs::read(original_path(project.path(), &attachment.id).unwrap()).unwrap(),
                    bytes
                );
            }
            if matches!(extension, "bmp" | "tiff") {
                continue;
            }
            fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
            let truncated = import_path(project.path(), &path).unwrap();
            assert!(truncated.media_kinds.is_empty());
            assert!(truncated.media_markdown.is_none());
            assert_eq!(
                fs::read(original_path(project.path(), &truncated.id).unwrap()).unwrap(),
                bytes[..bytes.len() - 1]
            );
        }
    }

    #[test]
    fn inline_media_keeps_native_bytes_and_editable_markdown_preview_identity() {
        let project = tempfile::tempdir().expect("project");
        for (extension, expected_kind) in [("png", MediaKind::Image), ("wav", MediaKind::Audio)] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(format!("native.{extension}"));
            let attachment = import_path(project.path(), &path).expect(extension);
            assert!(attachment.editable_markdown.is_none());
            let markdown = attachment.media_markdown.expect("native preview Markdown");
            assert!(markdown.starts_with("!["));
            assert!(manuscript_selects_attachment(&markdown, &attachment.id));
            let resolved =
                resolve_for_generation_with_budget(project.path(), "doc", &markdown, 32768, 4, 128)
                    .expect("native prompt");
            assert_eq!(resolved.media.len(), 1);
            assert_eq!(resolved.media[0].kind, expected_kind);
            assert_eq!(
                resolved.media[0].bytes,
                fs::read(path).expect("source bytes")
            );
        }
    }

    #[test]
    fn supported_document_files_return_editable_text_on_first_and_cached_import() {
        let project = tempfile::tempdir().expect("project");
        for extension in ["pdf", "docx", "epub", "md", "txt", "csv", "xlsx"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(format!("editable.{extension}"));
            for _ in 0..2 {
                let attachment = import_path(project.path(), &path).expect(extension);
                assert_eq!(
                    fs::read(original_path(project.path(), &attachment.id).unwrap()).unwrap(),
                    fs::read(&path).unwrap()
                );
                let markdown = attachment.editable_markdown.as_deref().expect(extension);
                assert!(
                    markdown.contains("Loom editable fixture"),
                    "{extension}: {markdown}"
                );
                assert!(!markdown.contains("loom-attachment:"));
                let snapshot =
                    add_document_context_snapshot(project.path(), extension, &[attachment.id])
                        .expect(extension);
                assert!(snapshot.markdown.is_empty());
                assert_eq!(snapshot.materials.len(), 1);
                assert_eq!(snapshot.attachments.len(), 1);
                assert!(
                    resolve_for_generation(project.path(), extension, "")
                        .unwrap()
                        .context_preamble
                        .contains("Loom editable fixture")
                );
            }
        }
    }

    #[test]
    fn excerpt_chunks_preserve_every_utf8_boundary() {
        for symbol in ["•", "é", "界", "🖋"] {
            for offset in 0..8 {
                let text = format!("{}{}", "a".repeat(offset), symbol.repeat(3000));
                let ranges = excerpt_chunk_ranges(&text);
                let rebuilt: String = ranges
                    .iter()
                    .map(|&(start, end)| &text[start..end])
                    .collect();
                assert_eq!(rebuilt, text);
                assert!(
                    ranges
                        .iter()
                        .all(|&(start, end)| end > start && end - start <= EXCERPT_CHUNK_BYTES)
                );
            }
        }
    }

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
    fn ambiguous_context_is_rejected_without_rewriting_saved_bytes() {
        let project = tempfile::tempdir().unwrap();
        let path = attachment_root(project.path())
            .unwrap()
            .join("document-context.json");
        let bytes = br#"{"schema":"loom.document-context.v3","documents":{},"manual_text":{"doc":"Author words mixed with imported words"},"text_imports":{}}"#;
        fs::write(&path, bytes).unwrap();
        assert!(document_context_snapshot(project.path(), "doc").is_err());
        assert!(set_document_context_snapshot(project.path(), "doc", "new", &[]).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn add_and_remove_update_context_markdown_and_selection_in_one_write() {
        let project = tempfile::tempdir().expect("project fixture");
        let path = project.path().join("guide.md");
        fs::write(&path, "Attached guide body.").expect("write fixture");
        let attachment = import_path(project.path(), &path).expect("import fixture");
        set_document_context_snapshot(project.path(), "doc", "Opening note.", &[])
            .expect("persist note");

        add_document_context(project.path(), "doc", &attachment.id).expect("add attachment");
        let added = document_context_text(project.path(), "doc").expect("read added marker");
        assert!(added.starts_with("Opening note."));
        assert_eq!(added, "Opening note.");
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
        set_document_context_snapshot(project.path(), "doc", &pasted, &[])
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
    fn selected_material_and_retrieval_query_are_stable_between_growth_rebalances() {
        let project = tempfile::tempdir().expect("project fixture");
        let path = project.path().join("reference.md");
        fs::write(&path, "A stable attached reference.").expect("write reference");
        let attachment = import_path(project.path(), &path).expect("import reference");
        add_document_context_snapshot(project.path(), "doc", std::slice::from_ref(&attachment.id))
            .expect("select context material");

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
        set_document_context_snapshot(
            project.path(),
            "doc",
            "Author direction.",
            &[text.id.clone(), image.id.clone(), audio.id.clone()],
        )
        .expect("persist instructions and selected materials");

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
    fn selected_text_is_separate_idempotent_and_never_hidden_twice() {
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
        assert!(first.markdown.is_empty());
        assert_eq!(first.attachments.len(), 1);
        assert_eq!(first.materials.len(), 1);

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
        assert!(restored.markdown.is_empty());
        assert_eq!(restored.materials.len(), 1);
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

    #[test]
    fn captured_wav_survives_exactly_without_transcription_or_a_source_file() {
        let project = tempfile::tempdir().expect("project fixture");
        let wav = wav_fixture();
        let audio = import_recorded_wav(project.path(), "Recording.wav".to_owned(), &wav)
            .expect("retain capture");
        assert_eq!(audio.media_kinds, ["audio"]);
        assert_eq!(audio.text_bytes, 0);
        let resolved = resolve_for_generation(project.path(), "document", &audio.inline_markdown)
            .expect("direct native audio context");
        assert!(resolved.context_preamble.is_empty());
        assert_eq!(resolved.media.len(), 1);
        assert_eq!(resolved.media[0].bytes, wav);
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
