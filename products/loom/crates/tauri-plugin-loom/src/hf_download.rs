//! Shared weight storage, independent of whether Hugging Face tooling is installed.
use loom_backend_llama::{
    DownloadCancellation, DownloadControl, DownloadError, DownloadProgress, GgufDownloadRequest,
    GgufDownloadResult, Sha256Digest, download_gguf,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
struct CachePaths {
    repository: PathBuf,
    blob: PathBuf,
    snapshot: PathBuf,
    lock: PathBuf,
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> DownloadError {
    DownloadError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

fn paths(
    root: &Path,
    url: &str,
    sha: Sha256Digest,
    name: &str,
) -> Result<CachePaths, DownloadError> {
    let url = url::Url::parse(url).map_err(|_| DownloadError::InvalidUrl)?;
    let parts: Vec<_> = url
        .path_segments()
        .ok_or(DownloadError::InvalidUrl)?
        .collect();
    let pinned = url.host_str() == Some("huggingface.co")
        && parts.len() >= 5
        && parts[2] == "resolve"
        && parts[3].len() == 40
        && parts[3].bytes().all(|c| c.is_ascii_hexdigit())
        && parts.iter().all(|p| component(p));
    let (repo_name, snapshot_suffix) = if pinned {
        let repo = format!("models--{}--{}", parts[0], parts[1]);
        let suffix: PathBuf = parts[3..].iter().collect();
        (repo, suffix)
    } else {
        // A checksum identifies bytes, not a Hub repository or upstream revision.
        ("local".into(), PathBuf::from(sha.to_string()).join(name))
    };
    let repository = root.join(&repo_name);
    Ok(CachePaths {
        blob: repository.join("blobs").join(sha.to_string()),
        snapshot: repository.join("snapshots").join(snapshot_suffix),
        lock: root
            .join(".locks")
            .join(repo_name)
            .join(format!("{sha}.lock")),
        repository,
    })
}

fn root() -> Result<PathBuf, DownloadError> {
    desktop_model_defaults::hugging_face_hub_cache_dir().ok_or(DownloadError::MissingTargetParent)
}

fn real_directory(path: &Path) -> Result<(), DownloadError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !path.exists()
    {
        real_directory(parent)?;
    }
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .map_err(|e| io_error("inspect cache directory", path, e))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(io_error(
                    "use cache directory",
                    path,
                    io::Error::other("not a real directory"),
                ))
            }
        }
        Err(e) => Err(io_error("create cache directory", path, e)),
    }
}

pub(crate) fn target(url: &str, name: &str, sha: Sha256Digest) -> Result<PathBuf, DownloadError> {
    let paths = paths(&root()?, url, sha, name)?;
    real_directory(
        paths
            .snapshot
            .parent()
            .ok_or(DownloadError::MissingTargetParent)?,
    )?;
    Ok(paths.snapshot)
}

fn existing_target(paths: &CachePaths) -> Result<Option<PathBuf>, DownloadError> {
    match fs::symlink_metadata(&paths.snapshot) {
        Ok(metadata) => {
            let canonical = fs::canonicalize(&paths.snapshot)
                .map_err(|e| io_error("resolve cache snapshot", &paths.snapshot, e))?;
            let repository = fs::canonicalize(&paths.repository)
                .map_err(|e| io_error("resolve cache repository", &paths.repository, e))?;
            if !canonical.starts_with(&repository)
                || (!metadata.is_file() && !metadata.file_type().is_symlink())
            {
                return Err(io_error(
                    "use cache snapshot",
                    &paths.snapshot,
                    io::Error::other("snapshot escapes repository or is not a file"),
                ));
            }
            Ok(Some(canonical))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error("inspect cache snapshot", &paths.snapshot, e)),
    }
}

fn publish(paths: &CachePaths) -> Result<(), DownloadError> {
    if let Some(existing) = existing_target(paths)? {
        if existing
            == fs::canonicalize(&paths.blob)
                .map_err(|e| io_error("resolve cache blob", &paths.blob, e))?
        {
            return Ok(());
        }
        return Err(DownloadError::TargetRaceMismatch);
    }
    #[cfg(unix)]
    let result = {
        let parent = paths
            .snapshot
            .parent()
            .ok_or(DownloadError::MissingTargetParent)?;
        let suffix = parent
            .strip_prefix(&paths.repository)
            .map_err(|_| DownloadError::MissingTargetParent)?;
        let mut relative = PathBuf::new();
        for _ in suffix.components() {
            relative.push("..");
        }
        relative.push("blobs");
        relative.push(paths.blob.file_name().ok_or(DownloadError::TargetNotGguf)?);
        std::os::unix::fs::symlink(relative, &paths.snapshot)
    };
    #[cfg(not(unix))]
    let result = fs::hard_link(&paths.blob, &paths.snapshot);
    result.map_err(|e| io_error("publish cache snapshot", &paths.snapshot, e))
}

pub(crate) async fn download<F>(
    request: &GgufDownloadRequest,
    cancellation: &DownloadCancellation,
    progress: F,
) -> Result<GgufDownloadResult, DownloadError>
where
    F: FnMut(DownloadProgress) -> DownloadControl,
{
    let name = request
        .target_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(DownloadError::TargetNotGguf)?;
    let paths = paths(&root()?, &request.url, request.expected_sha256, name)?;
    if paths.snapshot != request.target_path {
        return Err(DownloadError::TargetRaceMismatch);
    }
    download_in_cache(request, cancellation, progress, &paths).await
}

async fn download_in_cache<F>(
    request: &GgufDownloadRequest,
    cancellation: &DownloadCancellation,
    progress: F,
    paths: &CachePaths,
) -> Result<GgufDownloadResult, DownloadError>
where
    F: FnMut(DownloadProgress) -> DownloadControl,
{
    real_directory(
        paths
            .blob
            .parent()
            .ok_or(DownloadError::MissingTargetParent)?,
    )?;
    real_directory(
        paths
            .snapshot
            .parent()
            .ok_or(DownloadError::MissingTargetParent)?,
    )?;
    real_directory(
        paths
            .lock
            .parent()
            .ok_or(DownloadError::MissingTargetParent)?,
    )?;
    if fs::symlink_metadata(&paths.lock).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err(io_error(
            "use cache lock",
            &paths.lock,
            io::Error::other("not a regular lock file"),
        ));
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&paths.lock)
        .map_err(|e| io_error("open cache lock", &paths.lock, e))?;
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        match fs4::FileExt::try_lock(&lock) {
            Ok(()) => break,
            Err(fs4::TryLockError::WouldBlock) => {}
            Err(fs4::TryLockError::Error(e)) => {
                return Err(io_error("lock cache blob", &paths.lock, e));
            }
        }
        if started.elapsed() >= Duration::from_mins(1) {
            return Err(DownloadError::PartialFileBusy);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut blob_request = request.clone();
    let existing = existing_target(paths)?;
    blob_request.target_path = existing.clone().unwrap_or_else(|| paths.blob.clone());
    let mut result = download_gguf(&blob_request, cancellation, progress).await?;
    if cancellation.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    if existing.is_none() {
        publish(paths)?;
    }
    result.target_path.clone_from(&paths.snapshot);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn pinned_hub_layout_and_local_sources_do_not_fabricate_provenance() {
        let sha = Sha256Digest::from_hex(&"a".repeat(64)).expect("shared HF fixture");
        let revision = "b".repeat(40);
        let root = Path::new("/cache/hub");
        let pinned = paths(
            root,
            &format!("https://huggingface.co/owner/repo/resolve/{revision}/sub/model.gguf"),
            sha,
            "ignored.gguf",
        )
        .expect("shared HF fixture");
        assert_eq!(
            pinned.snapshot,
            root.join(format!(
                "models--owner--repo/snapshots/{revision}/sub/model.gguf"
            ))
        );
        assert_eq!(
            pinned.blob,
            root.join(format!("models--owner--repo/blobs/{}", "a".repeat(64)))
        );
        for source in [
            "https://weights.test/model.gguf",
            "https://huggingface.co/owner/repo/resolve/main/model.gguf",
            "https://huggingface.co/owner/repo/resolve/%2e%2e/model.gguf",
        ] {
            let local = paths(root, source, sha, "model.gguf").expect("shared HF fixture");
            assert_eq!(local.repository, root.join("local"));
        }
    }
    #[tokio::test]
    async fn two_app_downloads_reuse_blob_offline_and_never_overwrite_corruption() {
        let root = tempfile::tempdir().expect("shared HF fixture");
        let bytes = b"GGUFshared-fixture";
        let sha = Sha256Digest::from_hex(&format!("{:x}", Sha256::digest(bytes)))
            .expect("shared HF fixture");
        let url = format!(
            "https://huggingface.co/owner/repo/resolve/{}/model.gguf",
            "b".repeat(40)
        );
        let paths = paths(root.path(), &url, sha, "model.gguf").expect("shared HF fixture");
        real_directory(paths.blob.parent().expect("shared HF fixture")).expect("shared HF fixture");
        fs::write(&paths.blob, bytes).expect("shared HF fixture");
        let request = GgufDownloadRequest::new(&url, &paths.snapshot, sha, 1024);
        for _ in 0..2 {
            let result = download_in_cache(
                &request,
                &DownloadCancellation::default(),
                |_| DownloadControl::Continue,
                &paths,
            )
            .await
            .expect("shared HF fixture");
            assert_eq!(result.target_path, paths.snapshot);
            assert_eq!(
                fs::canonicalize(&paths.snapshot).expect("shared HF fixture"),
                fs::canonicalize(&paths.blob).expect("shared HF fixture")
            );
        }
        fs::write(&paths.blob, b"GGUFcorrupt-fixture").expect("shared HF fixture");
        assert!(
            download_in_cache(
                &request,
                &DownloadCancellation::default(),
                |_| DownloadControl::Continue,
                &paths
            )
            .await
            .is_err()
        );
        assert_eq!(
            fs::read(&paths.blob).expect("shared HF fixture"),
            b"GGUFcorrupt-fixture"
        );
    }
    #[tokio::test]
    async fn cache_lock_wait_is_cancellable() {
        let root = tempfile::tempdir().expect("shared HF fixture");
        let sha = Sha256Digest::from_hex(&"a".repeat(64)).expect("shared HF fixture");
        let paths = paths(
            root.path(),
            "https://weights.test/model.gguf",
            sha,
            "model.gguf",
        )
        .expect("shared HF fixture");
        real_directory(paths.lock.parent().expect("shared HF fixture")).expect("shared HF fixture");
        let lock = fs::File::create(&paths.lock).expect("shared HF fixture");
        fs4::FileExt::lock(&lock).expect("shared HF fixture");
        let cancellation = DownloadCancellation::default();
        let pending = cancellation.clone();
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            pending.cancel();
        });
        let request = GgufDownloadRequest::new(
            "https://weights.test/model.gguf",
            &paths.snapshot,
            sha,
            1024,
        );
        assert!(matches!(
            download_in_cache(
                &request,
                &cancellation,
                |_| DownloadControl::Continue,
                &paths
            )
            .await,
            Err(DownloadError::Cancelled)
        ));
        cancel.join().expect("cancellation worker");
    }
    #[cfg(unix)]
    #[test]
    fn snapshot_outside_repository_and_existing_directory_are_rejected() {
        let root = tempfile::tempdir().expect("shared HF fixture");
        let sha = Sha256Digest::from_hex(&"a".repeat(64)).expect("shared HF fixture");
        let paths = paths(
            root.path(),
            "https://weights.test/model.gguf",
            sha,
            "model.gguf",
        )
        .expect("shared HF fixture");
        real_directory(paths.snapshot.parent().expect("shared HF fixture"))
            .expect("shared HF fixture");
        let outside = root.path().join("outside");
        fs::write(&outside, b"GGUF").expect("shared HF fixture");
        std::os::unix::fs::symlink(&outside, &paths.snapshot).expect("shared HF fixture");
        assert!(existing_target(&paths).is_err());
        fs::remove_file(&paths.snapshot).expect("shared HF fixture");
        fs::create_dir(&paths.snapshot).expect("shared HF fixture");
        assert!(existing_target(&paths).is_err());
    }
}
