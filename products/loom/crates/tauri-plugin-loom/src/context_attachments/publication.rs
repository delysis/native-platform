//! Export only the visible scratch text and explicitly selected file cards.
//! Text-import receipts and their unselected original files stay private.
use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileReview {
    pub id: String,
    pub name: String,
    pub byte_count: u64,
    pub markdown: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Material {
    pub markdown: String,
    pub files: Vec<FileReview>,
}

pub(crate) fn material(root: &Path, document_id: &str) -> Result<Material, ContextAttachmentError> {
    let snapshot = document_context_snapshot(root, document_id)?;
    let ids = snapshot
        .attachments
        .iter()
        .map(|file| file.id.clone())
        .collect::<Vec<_>>();
    let manifests = card_manifest_metadata_for_ids(root, &ids)?;
    let mut total = 0_u64;
    let mut files = Vec::new();
    for (id, manifest) in manifests {
        loom_cabal::AssetDescriptor {
            sha256: id.clone(),
            name: manifest.attachment.file_name.clone(),
            byte_count: manifest.attachment.byte_count,
        }
        .validate()
        .map_err(|error| ContextAttachmentError::Processing(error.to_string()))?;
        total = total
            .checked_add(manifest.attachment.byte_count)
            .ok_or(ContextAttachmentError::SourceSize)?;
        if total > MAX_ATTACHMENT_BYTES {
            return Err(ContextAttachmentError::SourceSize);
        }
        files.push(FileReview {
            id,
            name: manifest.attachment.file_name,
            byte_count: manifest.attachment.byte_count,
            markdown: manifest
                .attachment
                .media_markdown
                .unwrap_or(manifest.attachment.inline_markdown),
        });
    }
    let material = Material {
        markdown: snapshot.markdown,
        files,
    };
    if document(&material).len() > MAX_MANUAL_CONTEXT_BYTES {
        return Err(ContextAttachmentError::ManualTextLimit);
    }
    Ok(material)
}

pub(crate) fn document(material: &Material) -> String {
    let mut text = material.markdown.clone();
    for file in &material.files {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&file.markdown);
    }
    text
}

pub(crate) fn file_bytes(
    root: &Path,
    file: &FileReview,
) -> Result<Vec<u8>, ContextAttachmentError> {
    read_object(root, &file.id, file.byte_count)
}
