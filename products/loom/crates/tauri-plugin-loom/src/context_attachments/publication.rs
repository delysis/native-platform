//! Export authored instructions, quoted material excerpts and reviewed files.
//! Text-only originals and private acquisition receipts are never published.
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
    let manifests = manifest_metadata_for_ids(root, &ids)?;
    let mut total = 0_u64;
    let mut markdown = snapshot.markdown;
    let mut files = Vec::new();
    for (id, manifest) in manifests {
        let selected = snapshot
            .materials
            .iter()
            .find(|item| item.attachment_id == id)
            .ok_or(ContextAttachmentError::ContextInvalid)?;
        validate_material(selected, &manifest)?;
        if manifest.attachment.text_bytes > 0 {
            let excerpt = match &selected.excerpt {
                Some(text) => text.clone(),
                None => read_canonical_text(root, &manifest)?,
            };
            if !markdown.is_empty() {
                markdown.push_str("\n\n");
            }
            markdown.push_str("> Material: ");
            markdown.push_str(&manifest.attachment.file_name.replace('\n', "\n> "));
            markdown.push_str("\n>\n> ");
            markdown.push_str(&excerpt.replace('\n', "\n> "));
            if markdown.len() > MAX_MANUAL_CONTEXT_BYTES {
                return Err(ContextAttachmentError::ManualTextLimit);
            }
            if manifest.media.is_empty() {
                continue;
            }
        }
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
        let file_markdown = editor_media_markdown(&manifest.attachment, &manifest.media)
            .unwrap_or_else(|| manifest.attachment.inline_markdown.clone());
        files.push(FileReview {
            id,
            name: manifest.attachment.file_name,
            byte_count: manifest.attachment.byte_count,
            markdown: file_markdown,
        });
    }
    let material = Material { markdown, files };
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
