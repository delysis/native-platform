use std::fs;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt as _;

use crate::{Result, StoreError};

pub(crate) fn normalize_document_path(path: &Path) -> Result<String> {
    if path.is_absolute() {
        return Err(StoreError::UnsafeRelativePath(path.display().to_string()));
    }
    let encoded = path
        .as_os_str()
        .to_str()
        .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))?;
    // Reject the foreign separator before `Path::components` can normalize it
    // into an ordinary Windows component boundary. Stored paths have one
    // platform-independent spelling and are never caller-selected OS paths.
    if encoded.contains('\\') {
        return Err(StoreError::UnsafeRelativePath(path.display().to_string()));
    }

    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(StoreError::UnsafeRelativePath(path.display().to_string()));
        };
        let component = component
            .to_str()
            .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))?;
        if component == ".loom" {
            return Err(StoreError::UnsafeRelativePath(path.display().to_string()));
        }
        components.push(component.to_owned());
    }

    if components.len() < 2 || components.first().map(String::as_str) != Some("manuscript") {
        return Err(StoreError::UnsafeRelativePath(path.display().to_string()));
    }
    Ok(components.join("/"))
}

pub(crate) fn ensure_directory(path: &Path) -> Result<()> {
    ensure_directory_with_policy(path, DirectoryPolicy::Ordinary)
}

pub(crate) fn ensure_private_directory(path: &Path) -> Result<()> {
    ensure_directory_with_policy(path, DirectoryPolicy::Private)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectoryPolicy {
    Ordinary,
    Private,
}

fn ensure_directory_with_policy(path: &Path, policy: DirectoryPolicy) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(StoreError::SymbolicLink(path.to_path_buf()))
        }
        Ok(metadata) if !metadata.is_dir() => Err(StoreError::NotDirectory(path.to_path_buf())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_directory(path, policy)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn create_directory(path: &Path, policy: DirectoryPolicy) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    if policy == DirectoryPolicy::Private {
        builder.mode(0o700);
    }
    #[cfg(not(unix))]
    let _ = policy;
    builder.create(path)
}

pub(crate) fn ensure_document_parent(root: &Path, relative_path: &str) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    let path = Path::new(relative_path);
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::UnsafeRelativePath(relative_path.to_owned()))?;

    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(StoreError::UnsafeRelativePath(relative_path.to_owned()));
        };
        current.push(component);
        ensure_directory(&current)?;
    }

    let target = root.join(path);
    reject_symlink_target(&target)?;
    Ok(target)
}

pub(crate) fn inspect_document_path(root: &Path, relative_path: &str) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    let path = Path::new(relative_path);
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::UnsafeRelativePath(relative_path.to_owned()))?;
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(StoreError::UnsafeRelativePath(relative_path.to_owned()));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(StoreError::SymbolicLink(current));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(StoreError::NotDirectory(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    let target = root.join(path);
    reject_symlink_target(&target)?;
    Ok(target)
}

pub(crate) fn reject_symlink_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(StoreError::SymbolicLink(path.to_path_buf()))
        }
        Ok(metadata) if !metadata.is_file() => Err(StoreError::NotRegularFile(path.to_path_buf())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_paths_are_confined_to_manuscript() {
        assert_eq!(
            normalize_document_path(Path::new("manuscript/poems/one.txt")).expect("valid path"),
            "manuscript/poems/one.txt"
        );
        assert!(normalize_document_path(Path::new("../secret")).is_err());
        assert!(normalize_document_path(Path::new(".loom/project.json")).is_err());
        assert!(normalize_document_path(Path::new("assets/image.png")).is_err());
        assert!(normalize_document_path(Path::new("manuscript\\escape.md")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_directory_creation_is_owner_only_and_existing_modes_are_preserved() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().expect("temporary directory");
        let private = root.path().join(".loom");
        ensure_private_directory(&private).expect("create private directory");
        assert_eq!(
            fs::metadata(&private)
                .expect("private metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        fs::set_permissions(&private, fs::Permissions::from_mode(0o750))
            .expect("set deliberate existing permissions");
        ensure_private_directory(&private).expect("accept existing directory");
        assert_eq!(
            fs::metadata(&private)
                .expect("existing metadata")
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }
}
