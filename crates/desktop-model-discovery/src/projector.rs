use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::has_gguf_extension;

#[derive(Debug, Error)]
pub enum ProjectorDiscoveryError {
    #[error("the model folder exceeds the {0}-entry projector discovery bound")]
    EntryLimit(usize),
    #[error("the model has {0} possible projector files; choose its exact projector")]
    Ambiguous(usize),
    #[error("the catalog artifact must be one local filename beside the selected model")]
    InvalidArtifactName,
    #[error("the selected model has no parent directory")]
    MissingParent,
    #[error("the model's sibling projector candidates could not be inspected: {0}")]
    Io(#[from] std::io::Error),
}

/// A named catalog artifact stays beside the selected alias, never the
/// canonical blob target. This builds a path; it does not prove file identity.
pub fn sibling_artifact_path(
    selected_model: &Path,
    name: &str,
) -> Result<PathBuf, ProjectorDiscoveryError> {
    let mut components = Path::new(name).components();
    if name.contains(['/', '\\'])
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(ProjectorDiscoveryError::InvalidArtifactName);
    }
    selected_model
        .parent()
        .map(|parent| parent.join(name))
        .ok_or(ProjectorDiscoveryError::MissingParent)
}

/// Finds zero or one candidate in one bounded sibling directory. Ambiguity and
/// partial inspection fail closed. A filename match never proves that the
/// projector belongs to the model; native inspection still decides compatibility.
pub fn discover_unique_projector(
    selected_model: &Path,
    max_entries: usize,
) -> Result<Option<PathBuf>, ProjectorDiscoveryError> {
    if selected_model.as_os_str().is_empty() {
        return Ok(None);
    }
    let Some(parent) = selected_model.parent() else {
        return Ok(None);
    };
    let entries = std::fs::read_dir(parent)?;
    let mut found = None;
    let mut count = 0;
    for (index, entry) in entries.enumerate() {
        if index >= max_entries {
            return Err(ProjectorDiscoveryError::EntryLimit(max_entries));
        }
        let entry = entry?;
        let path = entry.path();
        if !has_gguf_extension(&path)
            || !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().contains("mmproj"))
        {
            continue;
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.is_file() {
            count += 1;
            found = Some(path);
        }
    }
    if count > 1 {
        return Err(ProjectorDiscoveryError::Ambiguous(count));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_pairing_preserves_alias_and_rejects_path_traversal() {
        let selected = Path::new("cache/snapshots/revision/model.gguf");
        assert_eq!(
            sibling_artifact_path(selected, "mmproj.gguf").expect("sibling"),
            Path::new("cache/snapshots/revision/mmproj.gguf")
        );
        for name in [
            "../mmproj.gguf",
            "/mmproj.gguf",
            "a/b.gguf",
            "a\\b.gguf",
            "",
            ".",
        ] {
            assert!(sibling_artifact_path(selected, name).is_err());
        }
    }

    #[test]
    fn no_choice_is_made_from_an_ambiguous_or_truncated_directory() {
        let dir = tempfile::tempdir().expect("directory");
        let model = dir.path().join("model.gguf");
        std::fs::write(&model, b"GGUF").expect("model");
        assert!(
            discover_unique_projector(&model, 4)
                .expect("none")
                .is_none()
        );
        let projector = dir.path().join("mmproj.gguf");
        std::fs::write(&projector, b"GGUF").expect("projector");
        assert_eq!(
            discover_unique_projector(&model, 4).expect("one"),
            Some(projector)
        );
        assert!(matches!(
            discover_unique_projector(&model, 1),
            Err(ProjectorDiscoveryError::EntryLimit(1))
        ));
        std::fs::write(dir.path().join("mmproj-other.gguf"), b"GGUF").expect("other");
        assert!(matches!(
            discover_unique_projector(&model, 4),
            Err(ProjectorDiscoveryError::Ambiguous(2))
        ));
    }
}
