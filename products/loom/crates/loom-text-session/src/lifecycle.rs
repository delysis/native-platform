//! Shared document creation policy for all Loom views.
use loom_document::DocumentContent;
use loom_store::ProjectStore;

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error(transparent)]
    Store(#[from] loom_store::StoreError),
    #[error("the candidate manuscript path could not be inspected: {0}")]
    Inspect(std::io::Error),
    #[error("Loom could not allocate another Untitled manuscript name")]
    Capacity,
}

pub fn create_untitled(store: &mut ProjectStore) -> Result<String, CreateError> {
    for ordinal in 1..=10_000 {
        let path = if ordinal == 1 {
            "Untitled.md".into()
        } else {
            format!("Untitled-{ordinal}.md")
        };
        if store.document_path_is_reserved(&path)? {
            continue;
        }
        match std::fs::symlink_metadata(store.root().join(&path)) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CreateError::Inspect(error)),
        }
        store.create_document_if_absent(
            &path,
            DocumentContent::Prose(String::new()),
            "new document",
        )?;
        return Ok(path);
    }
    Err(CreateError::Capacity)
}
