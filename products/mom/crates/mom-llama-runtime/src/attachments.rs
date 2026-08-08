use crate::config::resolve_settings;
use crate::conversation_store::{
    Message, MessageRole, active_leaf_id, get_or_create_conversation, upsert_conversation,
};
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::Result;
use llama_native_types::{MediaInput, MediaKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const ATTACHMENTS_FILE: &str = "attachments.json";
const ATTACHMENTS_NAMESPACE: &str = "attachments.v2";
const MAX_TEXT_ATTACHMENT_BYTES: u64 = 512 * 1024;
const MAX_IMAGE_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_AUDIO_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024;
const MAX_PDF_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Text,
    Image,
    Audio,
    Pdf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentRecord {
    pub id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub kind: AttachmentKind,
    pub file_name: String,
    pub source_path: String,
    pub stored_path: String,
    pub mime: String,
    pub bytes: u64,
    pub sha256: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AttachmentDb {
    #[serde(default)]
    pub attachments: Vec<AttachmentRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentImportOutput {
    pub attachment: AttachmentRecord,
    pub multimodal_ready: bool,
    pub multimodal_blocker: Option<Blocker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentPreview {
    pub attachment: AttachmentRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
}

pub fn attachment_import(
    conversation_id: &str,
    path: &Path,
) -> Result<CommandResult<AttachmentImportOutput>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Ok(CommandResult::blocked(
                "mom_llama.attachment_import",
                "stub_blocked",
                Blocker::new(
                    "attachment_not_found",
                    format!("Attachment {} was not found.", path.display()),
                    vec!["Choose an existing local file.".to_string()],
                ),
            ));
        }
    };
    if !metadata.is_file() {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import",
            "stub_blocked",
            Blocker::new(
                "attachment_not_file",
                format!("Attachment {} is not a file.", path.display()),
                vec!["Choose a local file.".to_string()],
            ),
        ));
    }
    let Some((kind, mime, max_bytes)) = classify_attachment(path) else {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import",
            "stub_blocked",
            Blocker::new(
                "attachment_type_unsupported",
                "Supported native attachment types are text, image, audio, and PDF.",
                vec!["Use .txt, .md, .csv, .json, .png, .jpg, .jpeg, .webp, .wav, .mp3, .flac, or .pdf.".to_string()],
            ),
        ));
    };
    if metadata.len() > max_bytes {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import",
            "stub_blocked",
            Blocker::new(
                "attachment_too_large",
                format!(
                    "Attachment is {} bytes; native limit for this type is {} bytes.",
                    metadata.len(),
                    max_bytes
                ),
                vec!["Use a smaller attachment.".to_string()],
            ),
        ));
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment")
        .to_string();
    store_attachment_payload(
        conversation_id,
        file_name,
        path.display().to_string(),
        kind,
        mime,
        fs::read(path)?,
        "mom_llama.attachment_import",
    )
}

pub fn attachment_import_pasted_text(
    conversation_id: &str,
    text: String,
) -> Result<CommandResult<AttachmentImportOutput>> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import_paste",
            "stub_blocked",
            Blocker::new(
                "pasted_text_empty",
                "The pasted text is empty.",
                vec!["Paste non-empty text.".to_string()],
            ),
        ));
    }
    if text.len() as u64 > MAX_TEXT_ATTACHMENT_BYTES {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import_paste",
            "stub_blocked",
            Blocker::new(
                "attachment_too_large",
                format!(
                    "Pasted text is {} bytes; the encrypted attachment limit is {} bytes.",
                    text.len(),
                    MAX_TEXT_ATTACHMENT_BYTES
                ),
                vec!["Paste a smaller text excerpt.".to_string()],
            ),
        ));
    }
    store_attachment_payload(
        conversation_id,
        format!("pasted-text-{}.txt", now_ms()),
        "pasted-text".to_string(),
        AttachmentKind::Text,
        "text/plain",
        text.into_bytes(),
        "mom_llama.attachment_import_paste",
    )
}

fn store_attachment_payload(
    conversation_id: &str,
    file_name: String,
    source_path: String,
    kind: AttachmentKind,
    mime: &str,
    payload: Vec<u8>,
    command: &str,
) -> Result<CommandResult<AttachmentImportOutput>> {
    let (conversation_db, mut conversation) = get_or_create_conversation(conversation_id)?;
    let settings = resolve_settings()?;
    let id = Uuid::new_v4().to_string();
    let store = RuntimeStore::open(&settings.data_dir)?;
    let blob_namespace = format!("attachment.blob.{id}");
    store.put_bytes(&blob_namespace, &payload)?;
    let stored_path = format!("encrypted://{blob_namespace}");
    let sha256 = hash_bytes(&payload);
    let now = now_ms().to_string();
    let message_id = Uuid::new_v4().to_string();
    let record = AttachmentRecord {
        id,
        conversation_id: conversation_id.to_string(),
        message_id: message_id.clone(),
        kind: kind.clone(),
        file_name: file_name.clone(),
        source_path,
        stored_path: stored_path.clone(),
        mime: mime.to_string(),
        bytes: payload.len() as u64,
        sha256,
        created_at: now.clone(),
    };
    let content = attachment_message_content(&record, &payload)?;
    conversation.messages.push(Message {
        id: message_id.clone(),
        conversation_id: conversation_id.to_string(),
        role: MessageRole::User,
        content,
        created_at: now.clone(),
        parent_id: active_leaf_id(&conversation),
        model: None,
        receipt_id: None,
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: None,
    });
    conversation.active_leaf_message_id = Some(message_id);
    conversation.updated_at = now;
    let conversation_path = upsert_conversation(conversation_db, conversation)?;
    let mut attachment_db = load_attachment_db()?;
    attachment_db.attachments.insert(0, record.clone());
    let attachment_db_path = save_attachment_db(&attachment_db)?;
    let (multimodal_ready, multimodal_blocker) = multimodal_readiness(&settings, &record);
    Ok(CommandResult::passed(
        command,
        "contracted",
        AttachmentImportOutput {
            attachment: record,
            multimodal_ready,
            multimodal_blocker,
        },
        vec![
            stored_path,
            conversation_path.display().to_string(),
            attachment_db_path.display().to_string(),
        ],
        Vec::new(),
        false,
        false,
    ))
}

pub fn attachment_list(
    conversation_id: Option<&str>,
) -> Result<CommandResult<Vec<AttachmentRecord>>> {
    let db = load_attachment_db()?;
    let attachments = db
        .attachments
        .into_iter()
        .filter(|attachment| {
            conversation_id
                .map(|id| attachment.conversation_id == id)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.attachment_list",
        "contracted",
        attachments,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn attachment_preview(
    attachment_id: &str,
    include_payload: bool,
) -> Result<CommandResult<AttachmentPreview>> {
    let Some(attachment) = load_attachment_db()?
        .attachments
        .into_iter()
        .find(|attachment| attachment.id == attachment_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_preview",
            "stub_blocked",
            Blocker::new(
                "attachment_not_found",
                format!("Attachment {attachment_id} was not found."),
                vec!["Refresh the conversation and try again.".to_string()],
            ),
        ));
    };
    let bytes = if include_payload {
        Some(
            attachment_bytes(attachment_id)?
                .ok_or_else(|| anyhow::anyhow!("encrypted attachment payload is missing"))?,
        )
    } else {
        None
    };
    Ok(CommandResult::passed(
        "mom_llama.attachment_preview",
        "contracted",
        AttachmentPreview { attachment, bytes },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn multimodal_paths_for_conversation(conversation_id: &str) -> Result<Vec<PathBuf>> {
    let db = load_attachment_db()?;
    Ok(db
        .attachments
        .into_iter()
        .filter(|attachment| {
            attachment.conversation_id == conversation_id
                && matches!(
                    attachment.kind,
                    AttachmentKind::Image | AttachmentKind::Audio
                )
        })
        .map(|attachment| PathBuf::from(attachment.stored_path))
        .collect())
}

pub fn attachment_bytes(attachment_id: &str) -> Result<Option<Vec<u8>>> {
    RuntimeStore::current()?.get_bytes(&format!("attachment.blob.{attachment_id}"))
}

pub fn media_inputs_for_conversation(conversation_id: &str) -> Result<Vec<MediaInput>> {
    let db = load_attachment_db()?;
    let store = RuntimeStore::current()?;
    db.attachments
        .into_iter()
        .filter(|attachment| {
            attachment.conversation_id == conversation_id
                && matches!(
                    attachment.kind,
                    AttachmentKind::Image | AttachmentKind::Audio
                )
        })
        .map(|attachment| {
            let kind = match attachment.kind {
                AttachmentKind::Image => MediaKind::Image,
                AttachmentKind::Audio => MediaKind::Audio,
                AttachmentKind::Text | AttachmentKind::Pdf => {
                    return Err(anyhow::anyhow!(
                        "non-media attachment reached native media input"
                    ));
                }
            };
            let bytes = store
                .get_bytes(&format!("attachment.blob.{}", attachment.id))?
                .ok_or_else(|| anyhow::anyhow!("encrypted attachment payload is missing"))?;
            Ok(MediaInput {
                id: attachment.id,
                kind,
                mime: attachment.mime,
                sha256: attachment.sha256,
                bytes,
            })
        })
        .collect()
}

pub fn load_attachment_db() -> Result<AttachmentDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<AttachmentDb>(
        ATTACHMENTS_NAMESPACE,
        &settings.data_dir.join(ATTACHMENTS_FILE),
    )?;
    Ok(store.get(ATTACHMENTS_NAMESPACE)?.unwrap_or_default())
}

fn save_attachment_db(db: &AttachmentDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(ATTACHMENTS_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

fn classify_attachment(path: &Path) -> Option<(AttachmentKind, &'static str, u64)> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_lowercase();
    match extension.as_str() {
        "txt" => Some((
            AttachmentKind::Text,
            "text/plain",
            MAX_TEXT_ATTACHMENT_BYTES,
        )),
        "md" => Some((
            AttachmentKind::Text,
            "text/markdown",
            MAX_TEXT_ATTACHMENT_BYTES,
        )),
        "csv" => Some((AttachmentKind::Text, "text/csv", MAX_TEXT_ATTACHMENT_BYTES)),
        "json" => Some((
            AttachmentKind::Text,
            "application/json",
            MAX_TEXT_ATTACHMENT_BYTES,
        )),
        "png" => Some((
            AttachmentKind::Image,
            "image/png",
            MAX_IMAGE_ATTACHMENT_BYTES,
        )),
        "jpg" | "jpeg" => Some((
            AttachmentKind::Image,
            "image/jpeg",
            MAX_IMAGE_ATTACHMENT_BYTES,
        )),
        "webp" => Some((
            AttachmentKind::Image,
            "image/webp",
            MAX_IMAGE_ATTACHMENT_BYTES,
        )),
        "wav" => Some((
            AttachmentKind::Audio,
            "audio/wav",
            MAX_AUDIO_ATTACHMENT_BYTES,
        )),
        "mp3" => Some((
            AttachmentKind::Audio,
            "audio/mpeg",
            MAX_AUDIO_ATTACHMENT_BYTES,
        )),
        "flac" => Some((
            AttachmentKind::Audio,
            "audio/flac",
            MAX_AUDIO_ATTACHMENT_BYTES,
        )),
        "pdf" => Some((
            AttachmentKind::Pdf,
            "application/pdf",
            MAX_PDF_ATTACHMENT_BYTES,
        )),
        _ => None,
    }
}

fn attachment_message_content(record: &AttachmentRecord, payload: &[u8]) -> Result<String> {
    if record.kind == AttachmentKind::Text {
        let text = std::str::from_utf8(payload)?;
        return Ok(format!(
            "Attached text file `{}`:\n\n```text\n{text}\n```",
            record.file_name
        ));
    }
    Ok(format!(
        "Attached {} file `{}` ({} bytes, sha256 {}). Stored locally for native multimodal use.",
        match record.kind {
            AttachmentKind::Text => "text",
            AttachmentKind::Image => "image",
            AttachmentKind::Audio => "audio",
            AttachmentKind::Pdf => "PDF",
        },
        record.file_name,
        record.bytes,
        record.sha256
    ))
}

fn multimodal_readiness(
    settings: &crate::config::Settings,
    record: &AttachmentRecord,
) -> (bool, Option<Blocker>) {
    if !matches!(record.kind, AttachmentKind::Image | AttachmentKind::Audio) {
        return (false, None);
    }
    if let Some(mmproj_path) = settings.mmproj_path.as_ref()
        && mmproj_path.is_file()
        && mmproj_path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("gguf"))
    {
        let verified = crate::native_runtime::resident_status()
            .and_then(|status| status.fingerprint)
            .and_then(|fingerprint| fingerprint.multimodal_projector_sha256)
            .is_some();
        if verified {
            return (true, None);
        }
        return (
            false,
            Some(Blocker::new(
                "mmproj_configured_not_verified",
                "The multimodal projector is configured but has not been loaded with the selected model yet.",
                vec!["Run a model check to verify the model and projector pair.".to_string()],
            )),
        );
    }
    (
        false,
        Some(Blocker::new(
            "mmproj_path_missing",
            "Image/audio attachments are stored, but llama.cpp multimodal execution requires an mmproj path.",
            vec![
                "Set `mom-llama settings update --set mmprojPath=/path/to/mmproj.gguf --json`."
                    .to_string(),
            ],
        )),
    )
}

fn hash_bytes(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    format!("{:x}", hasher.finalize())
}
