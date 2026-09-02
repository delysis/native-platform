use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const MODEL_CONTENT_SHA256: &str =
    "c710ae82b52aa969f89874e7e7b35ad570fec50cc3d943a4fdde0bb874948756";
pub(crate) const MODEL_SOURCE_REVISION: &str = "a61d2818df4659c956b9661a9447f46e98c15126";

const MANIFEST_SCHEMA: &str = "delysis.speech.parakeet-model.v1";
const MANIFEST_JSON: &str =
    include_str!("../assets/parakeet-realtime-eou-120m-v1-onnx.manifest.json");
const COPY_BUFFER_BYTES: usize = 1024 * 1024;
static NEXT_STAGING_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ModelManifest {
    schema: String,
    repository: String,
    revision: String,
    subdirectory: String,
    combined_bytes: u64,
    combined_sha256: String,
    files: Vec<ModelFileManifest>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ModelFileManifest {
    name: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug)]
pub(crate) struct ModelArtifactError(String);

impl fmt::Display for ModelArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ModelArtifactError {}

pub(crate) fn prepare_model_dir(
    explicit_candidate: Option<&Path>,
    explicit_managed_root: Option<&Path>,
) -> Result<Option<PathBuf>, ModelArtifactError> {
    let manifest = production_manifest()?;
    let managed_root = managed_model_root(explicit_managed_root).ok_or_else(|| {
        artifact_error(
            "Parakeet managed model storage is unavailable; configure \
             SPEECH_NATIVE_PARAKEET_MANAGED_ROOT",
        )
    })?;
    let managed_dir = managed_model_dir(&managed_root, &manifest);
    if managed_dir.exists() {
        verify_managed_model_dir(&managed_dir, &manifest)?;
        return Ok(Some(managed_dir));
    }

    let candidate = source_candidate(explicit_candidate, &manifest);
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    copy_verified_candidate(&candidate, &managed_root, &manifest).map(Some)
}

/// Cheap startup-only presence and shape probe.
///
/// This deliberately reads metadata only. It cannot confer model-byte
/// authority: first use still runs the full copy/hash/load/hash admission in
/// [`prepare_model_dir`] and [`verify_production_model_dir`].
pub(crate) fn probe_model_source(
    explicit_candidate: Option<&Path>,
    explicit_managed_root: Option<&Path>,
) -> Result<bool, ModelArtifactError> {
    let manifest = production_manifest()?;
    let managed_root = managed_model_root(explicit_managed_root).ok_or_else(|| {
        artifact_error(
            "Parakeet managed model storage is unavailable; configure \
             SPEECH_NATIVE_PARAKEET_MANAGED_ROOT",
        )
    })?;
    let managed_dir = managed_model_dir(&managed_root, &manifest);
    if managed_dir.exists() {
        probe_model_dir_shape(&managed_dir, &manifest, true)?;
        return Ok(true);
    }
    let candidate = source_candidate(explicit_candidate, &manifest);
    let Some(candidate) = candidate else {
        return Ok(false);
    };
    if !candidate.exists() {
        return Ok(false);
    }
    probe_model_dir_shape(&candidate, &manifest, false)?;
    Ok(true)
}

fn probe_model_dir_shape(
    path: &Path,
    manifest: &ModelManifest,
    managed: bool,
) -> Result<(), ModelArtifactError> {
    let directory = if managed {
        fs::symlink_metadata(path)
    } else {
        fs::metadata(path)
    }
    .map_err(|error| {
        artifact_error(format!(
            "Could not inspect Parakeet model directory {}: {error}",
            path.display()
        ))
    })?;
    if !directory.file_type().is_dir() {
        return Err(artifact_error(format!(
            "Parakeet model path is not a directory: {}",
            path.display()
        )));
    }
    for expected in &manifest.files {
        let artifact_path = path.join(&expected.name);
        let path_metadata = if managed {
            fs::symlink_metadata(&artifact_path)
        } else {
            fs::metadata(&artifact_path)
        }
        .map_err(|error| {
            artifact_error(format!(
                "Could not inspect Parakeet model artifact {}: {error}",
                artifact_path.display()
            ))
        })?;
        if !path_metadata.is_file()
            || path_metadata.len() != expected.bytes
            || (managed && managed_file_has_peer_links(&path_metadata))
        {
            return Err(artifact_error(format!(
                "Parakeet model artifact {} has the wrong type, length, or private-file shape",
                expected.name
            )));
        }
    }
    Ok(())
}

pub(crate) fn verify_production_model_dir(path: &Path) -> Result<(), ModelArtifactError> {
    verify_managed_model_dir(path, &production_manifest()?)
}

pub(crate) fn discover_default_model_dir() -> Option<PathBuf> {
    prepare_model_dir(None, None).ok().flatten()
}

fn production_manifest() -> Result<ModelManifest, ModelArtifactError> {
    let manifest: ModelManifest = serde_json::from_str(MANIFEST_JSON)
        .map_err(|error| artifact_error(format!("Parakeet model manifest is invalid: {error}")))?;
    validate_manifest(&manifest)?;
    if manifest.repository != super::PARAKEET_HF_REPOSITORY
        || manifest.revision != MODEL_SOURCE_REVISION
        || manifest.subdirectory != super::PARAKEET_HF_SUBDIRECTORY
        || manifest.combined_sha256 != MODEL_CONTENT_SHA256
    {
        return Err(artifact_error(
            "Parakeet model manifest identity does not match the compiled backend",
        ));
    }
    Ok(manifest)
}

fn validate_manifest(manifest: &ModelManifest) -> Result<(), ModelArtifactError> {
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(artifact_error(
            "Parakeet model manifest schema is unsupported",
        ));
    }
    if manifest.files.len() != 3 {
        return Err(artifact_error(
            "Parakeet model manifest must bind exactly three files",
        ));
    }
    let expected_names = ["encoder.onnx", "decoder_joint.onnx", "tokenizer.json"];
    if manifest
        .files
        .iter()
        .map(|file| file.name.as_str())
        .ne(expected_names)
    {
        return Err(artifact_error(
            "Parakeet model manifest file order or names are invalid",
        ));
    }
    let total = manifest.files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.bytes)
            .ok_or_else(|| artifact_error("Parakeet model manifest length overflows"))
    })?;
    if total != manifest.combined_bytes
        || !is_lower_hex_sha256(&manifest.combined_sha256)
        || manifest
            .files
            .iter()
            .any(|file| file.bytes == 0 || !is_lower_hex_sha256(&file.sha256))
    {
        return Err(artifact_error(
            "Parakeet model manifest lengths or SHA-256 values are invalid",
        ));
    }
    Ok(())
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn managed_model_root(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = explicit {
        return Some(root.to_path_buf());
    }
    if let Some(root) = std::env::var_os("SPEECH_NATIVE_PARAKEET_MANAGED_ROOT") {
        return Some(PathBuf::from(root));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home).join("Library/Application Support/Delysis/Speech/models"));
    }
    #[cfg(target_os = "windows")]
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        return Some(PathBuf::from(root).join("Delysis/Speech/models"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(root).join("delysis/speech/models"));
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local/share/delysis/speech/models"))
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    None
}

fn managed_model_dir(root: &Path, manifest: &ModelManifest) -> PathBuf {
    root.join(super::PARAKEET_MODEL_ID)
        .join(&manifest.combined_sha256)
}

fn hugging_face_cache_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = std::env::var_os("HUGGINGFACE_HUB_CACHE") {
        roots.push(PathBuf::from(root));
    }
    if let Some(home) = std::env::var_os("HF_HOME") {
        roots.push(PathBuf::from(home).join("hub"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cache/huggingface/hub"));
    }
    roots.sort();
    roots.dedup();
    roots
}

fn discover_exact_hugging_face_candidate(manifest: &ModelManifest) -> Option<PathBuf> {
    hugging_face_cache_roots()
        .into_iter()
        .find_map(|root| exact_hugging_face_candidate_in_root(&root, manifest))
}

fn source_candidate(explicit: Option<&Path>, manifest: &ModelManifest) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("SPEECH_NATIVE_PARAKEET_MODEL_DIR")
                .or_else(|| std::env::var_os("FTE_PARAKEET_MODEL_DIR"))
                .map(PathBuf::from)
        })
        .or_else(|| discover_exact_hugging_face_candidate(manifest))
}

fn exact_hugging_face_candidate_in_root(root: &Path, manifest: &ModelManifest) -> Option<PathBuf> {
    let candidate = root
        .join("models--altunenes--parakeet-rs")
        .join("snapshots")
        .join(&manifest.revision)
        .join(&manifest.subdirectory);
    candidate.is_dir().then_some(candidate)
}

fn copy_verified_candidate(
    candidate: &Path,
    managed_root: &Path,
    manifest: &ModelManifest,
) -> Result<PathBuf, ModelArtifactError> {
    if !candidate.is_dir() {
        return Err(artifact_error(format!(
            "Parakeet model candidate is not a directory: {}",
            candidate.display()
        )));
    }
    let model_root = managed_root.join(super::PARAKEET_MODEL_ID);
    fs::create_dir_all(&model_root).map_err(|error| {
        artifact_error(format!(
            "Could not create Parakeet managed model storage {}: {error}",
            model_root.display()
        ))
    })?;
    set_private_directory(&model_root)?;

    let final_dir = managed_model_dir(managed_root, manifest);
    if final_dir.exists() {
        verify_managed_model_dir(&final_dir, manifest)?;
        return Ok(final_dir);
    }

    let nonce = NEXT_STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
    let staging = model_root.join(format!(
        ".{}.{}.{}.tmp",
        manifest.combined_sha256,
        std::process::id(),
        nonce
    ));
    fs::create_dir(&staging).map_err(|error| {
        artifact_error(format!(
            "Could not create Parakeet model staging directory {}: {error}",
            staging.display()
        ))
    })?;
    set_private_directory(&staging)?;

    let copied = copy_manifest_files(candidate, &staging, manifest)
        .and_then(|()| verify_model_dir(&staging, manifest));
    if let Err(error) = copied {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = make_model_files_read_only(&staging, manifest) {
        let _ = make_model_tree_writable(&staging, manifest);
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    match fs::rename(&staging, &final_dir) {
        Ok(()) => {}
        Err(_error) if final_dir.exists() => {
            make_model_tree_writable(&staging, manifest)?;
            fs::remove_dir_all(&staging).map_err(|cleanup_error| {
                artifact_error(format!(
                    "Parakeet model publish raced and staging cleanup failed: {cleanup_error}"
                ))
            })?;
            verify_managed_model_dir(&final_dir, manifest)?;
            return Ok(final_dir);
        }
        Err(error) => {
            let _ = make_model_tree_writable(&staging, manifest);
            let _ = fs::remove_dir_all(&staging);
            return Err(artifact_error(format!(
                "Could not atomically publish the verified Parakeet model: {error}"
            )));
        }
    }
    if let Err(error) = set_read_only_directory(&final_dir) {
        let _ = make_model_tree_writable(&final_dir, manifest);
        let _ = fs::remove_dir_all(&final_dir);
        return Err(error);
    }
    sync_directory(&model_root)?;
    verify_managed_model_dir(&final_dir, manifest)?;
    Ok(final_dir)
}

fn copy_manifest_files(
    candidate: &Path,
    staging: &Path,
    manifest: &ModelManifest,
) -> Result<(), ModelArtifactError> {
    let mut combined = Sha256::new();
    let mut combined_bytes = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];

    for expected in &manifest.files {
        let source_path = candidate.join(&expected.name);
        let mut source = File::open(&source_path).map_err(|error| {
            artifact_error(format!(
                "Could not open Parakeet model artifact {}: {error}",
                source_path.display()
            ))
        })?;
        let source_metadata = source.metadata().map_err(|error| {
            artifact_error(format!(
                "Could not inspect Parakeet model artifact {}: {error}",
                source_path.display()
            ))
        })?;
        if !source_metadata.is_file() || source_metadata.len() != expected.bytes {
            return Err(artifact_error(format!(
                "Parakeet model artifact {} has the wrong type or length",
                expected.name
            )));
        }

        let destination_path = staging.join(&expected.name);
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination_path)
            .map_err(|error| {
                artifact_error(format!(
                    "Could not create managed Parakeet artifact {}: {error}",
                    destination_path.display()
                ))
            })?;
        let mut individual = Sha256::new();
        let mut file_bytes = 0_u64;
        loop {
            let read = source.read(&mut buffer).map_err(|error| {
                artifact_error(format!(
                    "Could not read Parakeet model artifact {}: {error}",
                    source_path.display()
                ))
            })?;
            if read == 0 {
                break;
            }
            destination.write_all(&buffer[..read]).map_err(|error| {
                artifact_error(format!(
                    "Could not copy Parakeet model artifact {}: {error}",
                    expected.name
                ))
            })?;
            individual.update(&buffer[..read]);
            combined.update(&buffer[..read]);
            let read = u64::try_from(read)
                .map_err(|_| artifact_error("Parakeet model read length overflowed"))?;
            file_bytes = file_bytes
                .checked_add(read)
                .ok_or_else(|| artifact_error("Parakeet model file length overflowed"))?;
            combined_bytes = combined_bytes
                .checked_add(read)
                .ok_or_else(|| artifact_error("Parakeet model bundle length overflowed"))?;
        }
        destination.sync_all().map_err(|error| {
            artifact_error(format!(
                "Could not durably stage Parakeet model artifact {}: {error}",
                expected.name
            ))
        })?;
        if file_bytes != expected.bytes || digest_hex(individual) != expected.sha256 {
            return Err(artifact_error(format!(
                "Parakeet model artifact {} failed exact length/SHA-256 verification",
                expected.name
            )));
        }
    }
    if combined_bytes != manifest.combined_bytes || digest_hex(combined) != manifest.combined_sha256
    {
        return Err(artifact_error(
            "Parakeet model bundle failed exact combined length/SHA-256 verification",
        ));
    }
    sync_directory(staging)
}

fn verify_model_dir(path: &Path, manifest: &ModelManifest) -> Result<(), ModelArtifactError> {
    let directory_metadata = fs::symlink_metadata(path).map_err(|error| {
        artifact_error(format!(
            "Could not inspect Parakeet managed model directory {}: {error}",
            path.display()
        ))
    })?;
    if !directory_metadata.file_type().is_dir() {
        return Err(artifact_error(format!(
            "Parakeet managed model path is not a directory: {}",
            path.display()
        )));
    }
    let mut combined = Sha256::new();
    let mut combined_bytes = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    for expected in &manifest.files {
        let artifact_path = path.join(&expected.name);
        let path_metadata = fs::symlink_metadata(&artifact_path).map_err(|error| {
            artifact_error(format!(
                "Could not inspect Parakeet managed artifact path {}: {error}",
                artifact_path.display()
            ))
        })?;
        if !path_metadata.file_type().is_file() || managed_file_has_peer_links(&path_metadata) {
            return Err(artifact_error(format!(
                "Parakeet managed artifact {} is not one private regular file",
                expected.name
            )));
        }
        let mut artifact = File::open(&artifact_path).map_err(|error| {
            artifact_error(format!(
                "Could not open Parakeet model artifact {}: {error}",
                artifact_path.display()
            ))
        })?;
        let metadata = artifact.metadata().map_err(|error| {
            artifact_error(format!(
                "Could not inspect Parakeet model artifact {}: {error}",
                artifact_path.display()
            ))
        })?;
        if !metadata.is_file() || metadata.len() != expected.bytes {
            return Err(artifact_error(format!(
                "Parakeet model artifact {} has the wrong type or length",
                expected.name
            )));
        }
        let mut individual = Sha256::new();
        let mut file_bytes = 0_u64;
        loop {
            let read = artifact.read(&mut buffer).map_err(|error| {
                artifact_error(format!(
                    "Could not read Parakeet model artifact {}: {error}",
                    artifact_path.display()
                ))
            })?;
            if read == 0 {
                break;
            }
            individual.update(&buffer[..read]);
            combined.update(&buffer[..read]);
            let read = u64::try_from(read)
                .map_err(|_| artifact_error("Parakeet model read length overflowed"))?;
            file_bytes = file_bytes
                .checked_add(read)
                .ok_or_else(|| artifact_error("Parakeet model file length overflowed"))?;
            combined_bytes = combined_bytes
                .checked_add(read)
                .ok_or_else(|| artifact_error("Parakeet model bundle length overflowed"))?;
        }
        if file_bytes != expected.bytes || digest_hex(individual) != expected.sha256 {
            return Err(artifact_error(format!(
                "Parakeet model artifact {} failed exact length/SHA-256 verification",
                expected.name
            )));
        }
    }
    if combined_bytes != manifest.combined_bytes || digest_hex(combined) != manifest.combined_sha256
    {
        return Err(artifact_error(
            "Parakeet model bundle failed exact combined length/SHA-256 verification",
        ));
    }
    Ok(())
}

fn verify_managed_model_dir(
    path: &Path,
    manifest: &ModelManifest,
) -> Result<(), ModelArtifactError> {
    verify_model_dir(path, manifest)?;
    verify_managed_permissions(path, manifest)
}

#[cfg(unix)]
fn verify_managed_permissions(
    path: &Path,
    manifest: &ModelManifest,
) -> Result<(), ModelArtifactError> {
    use std::os::unix::fs::PermissionsExt;
    if fs::symlink_metadata(path)
        .map_err(|error| artifact_error(format!("Could not inspect {}: {error}", path.display())))?
        .permissions()
        .mode()
        & 0o222
        != 0
    {
        return Err(artifact_error(
            "Parakeet managed model directory remains writable",
        ));
    }
    for file in &manifest.files {
        let artifact_path = path.join(&file.name);
        if fs::symlink_metadata(&artifact_path)
            .map_err(|error| {
                artifact_error(format!(
                    "Could not inspect {}: {error}",
                    artifact_path.display()
                ))
            })?
            .permissions()
            .mode()
            & 0o222
            != 0
        {
            return Err(artifact_error(format!(
                "Parakeet managed artifact {} remains writable",
                file.name
            )));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_managed_permissions(
    path: &Path,
    manifest: &ModelManifest,
) -> Result<(), ModelArtifactError> {
    for file in &manifest.files {
        let artifact_path = path.join(&file.name);
        if !fs::metadata(&artifact_path)
            .map_err(|error| {
                artifact_error(format!(
                    "Could not inspect {}: {error}",
                    artifact_path.display()
                ))
            })?
            .permissions()
            .readonly()
        {
            return Err(artifact_error(format!(
                "Parakeet managed artifact {} remains writable",
                file.name
            )));
        }
    }
    Ok(())
}

fn digest_hex(digest: Sha256) -> String {
    format!("{:x}", digest.finalize())
}

#[cfg(unix)]
fn managed_file_has_peer_links(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() != 1
}

#[cfg(not(unix))]
fn managed_file_has_peer_links(_metadata: &fs::Metadata) -> bool {
    false
}

fn make_model_files_read_only(
    path: &Path,
    manifest: &ModelManifest,
) -> Result<(), ModelArtifactError> {
    for file in &manifest.files {
        set_read_only_file(&path.join(&file.name))?;
    }
    Ok(())
}

fn make_model_tree_writable(
    path: &Path,
    manifest: &ModelManifest,
) -> Result<(), ModelArtifactError> {
    set_private_directory(path)?;
    for file in &manifest.files {
        set_private_file(&path.join(&file.name))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), ModelArtifactError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        artifact_error(format!(
            "Could not make Parakeet model directory private {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), ModelArtifactError> {
    Ok(())
}

#[cfg(unix)]
fn set_read_only_file(path: &Path) -> Result<(), ModelArtifactError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o400)).map_err(|error| {
        artifact_error(format!(
            "Could not make managed Parakeet artifact read-only {}: {error}",
            path.display()
        ))
    })
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), ModelArtifactError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        artifact_error(format!(
            "Could not make managed Parakeet artifact private {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_read_only_file(path: &Path) -> Result<(), ModelArtifactError> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| artifact_error(format!("Could not inspect {}: {error}", path.display())))?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).map_err(|error| {
        artifact_error(format!(
            "Could not make managed Parakeet artifact read-only {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_file(path: &Path) -> Result<(), ModelArtifactError> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| artifact_error(format!("Could not inspect {}: {error}", path.display())))?
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).map_err(|error| {
        artifact_error(format!(
            "Could not make managed Parakeet artifact writable {}: {error}",
            path.display()
        ))
    })
}

#[cfg(unix)]
fn set_read_only_directory(path: &Path) -> Result<(), ModelArtifactError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o500)).map_err(|error| {
        artifact_error(format!(
            "Could not make managed Parakeet directory read-only {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_read_only_directory(_path: &Path) -> Result<(), ModelArtifactError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ModelArtifactError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            artifact_error(format!(
                "Could not sync Parakeet model directory {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), ModelArtifactError> {
    Ok(())
}

fn artifact_error(detail: impl Into<String>) -> ModelArtifactError {
    ModelArtifactError(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn temporary_root(label: &str) -> PathBuf {
        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "speech-native-model-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn fixture_manifest(files: &[(&str, &[u8])]) -> ModelManifest {
        let mut combined = Sha256::new();
        let mut combined_bytes = 0_u64;
        let files = files
            .iter()
            .map(|(name, bytes)| {
                combined.update(bytes);
                combined_bytes = combined_bytes
                    .checked_add(u64::try_from(bytes.len()).expect("fixture length fits"))
                    .expect("fixture length remains bounded");
                ModelFileManifest {
                    name: (*name).to_owned(),
                    bytes: u64::try_from(bytes.len()).expect("fixture length fits"),
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                }
            })
            .collect();
        ModelManifest {
            schema: MANIFEST_SCHEMA.to_owned(),
            repository: super::super::PARAKEET_HF_REPOSITORY.to_owned(),
            revision: MODEL_SOURCE_REVISION.to_owned(),
            subdirectory: super::super::PARAKEET_HF_SUBDIRECTORY.to_owned(),
            combined_bytes,
            combined_sha256: digest_hex(combined),
            files,
        }
    }

    #[test]
    fn checked_in_manifest_binds_the_accepted_revision_and_all_bytes() {
        let manifest = production_manifest().expect("checked-in manifest is valid");
        assert_eq!(manifest.revision, MODEL_SOURCE_REVISION);
        assert_eq!(manifest.combined_bytes, 480_708_981);
        assert_eq!(manifest.combined_sha256, MODEL_CONTENT_SHA256);
        assert_eq!(
            manifest
                .files
                .iter()
                .map(|file| (file.name.as_str(), file.bytes, file.sha256.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "encoder.onnx",
                    459_341_289,
                    "d472887cc38a784a5bfc21c2dbe247639edc3b3f9992388d8ceceaec07256b5b"
                ),
                (
                    "decoder_joint.onnx",
                    21_347_639,
                    "9d2553ac043c2fc5f69e970769b0fb8ab9103fbfdeb7d26a1ea9729d4bd2dddd"
                ),
                (
                    "tokenizer.json",
                    20_053,
                    "f6b0ad8690559351fa478116fe0985a203b76f7c040f3a9381f485c99c0325f8"
                ),
            ]
        );
    }

    #[test]
    fn verified_copy_is_independent_from_mutable_candidate_bytes() {
        let root = temporary_root("copy");
        let candidate = root.join("candidate");
        let managed = root.join("managed");
        fs::create_dir_all(&candidate).expect("create candidate");
        let files = [
            ("encoder.onnx", b"encoder".as_slice()),
            ("decoder_joint.onnx", b"decoder".as_slice()),
            ("tokenizer.json", b"tokenizer".as_slice()),
        ];
        for (name, bytes) in files {
            fs::write(candidate.join(name), bytes).expect("write candidate artifact");
        }
        let manifest = fixture_manifest(&files);
        let installed =
            copy_verified_candidate(&candidate, &managed, &manifest).expect("copy exact candidate");
        verify_managed_model_dir(&installed, &manifest).expect("managed bytes verify");

        fs::write(candidate.join("encoder.onnx"), b"tampered").expect("mutate external candidate");
        verify_managed_model_dir(&installed, &manifest)
            .expect("managed copy is independent from external mutation");
        assert_ne!(
            fs::read(candidate.join("encoder.onnx")).expect("read source"),
            fs::read(installed.join("encoder.onnx")).expect("read managed")
        );

        make_model_tree_writable(&installed, &manifest).expect("make fixture removable");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn same_name_tamper_and_truncation_fail_before_publish() {
        let root = temporary_root("tamper");
        let candidate = root.join("candidate");
        let managed = root.join("managed");
        fs::create_dir_all(&candidate).expect("create candidate");
        let files = [
            ("encoder.onnx", b"encoder".as_slice()),
            ("decoder_joint.onnx", b"decoder".as_slice()),
            ("tokenizer.json", b"tokenizer".as_slice()),
        ];
        for (name, bytes) in files {
            fs::write(candidate.join(name), bytes).expect("write candidate artifact");
        }
        let manifest = fixture_manifest(&files);

        fs::write(candidate.join("encoder.onnx"), b"encodex").expect("same-length tamper");
        assert!(copy_verified_candidate(&candidate, &managed, &manifest).is_err());
        assert!(!managed_model_dir(&managed, &manifest).exists());

        fs::write(candidate.join("encoder.onnx"), b"short").expect("truncate artifact");
        assert!(copy_verified_candidate(&candidate, &managed, &manifest).is_err());
        assert!(!managed_model_dir(&managed, &manifest).exists());

        fs::write(candidate.join("encoder.onnx"), b"encoder").expect("restore artifact");
        fs::remove_file(candidate.join("tokenizer.json")).expect("remove artifact");
        assert!(copy_verified_candidate(&candidate, &managed, &manifest).is_err());
        assert!(!managed_model_dir(&managed, &manifest).exists());

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn metadata_probe_never_substitutes_for_first_use_hash_verification() {
        let root = temporary_root("shape-not-authority");
        fs::create_dir_all(&root).expect("create candidate");
        let expected = [
            ("encoder.onnx", b"encoder".as_slice()),
            ("decoder_joint.onnx", b"decoder".as_slice()),
            ("tokenizer.json", b"tokenizer".as_slice()),
        ];
        let manifest = fixture_manifest(&expected);
        for (name, bytes) in [
            ("encoder.onnx", b"encodex".as_slice()),
            ("decoder_joint.onnx", b"decoder".as_slice()),
            ("tokenizer.json", b"tokenizer".as_slice()),
        ] {
            fs::write(root.join(name), bytes).expect("write candidate artifact");
        }

        probe_model_dir_shape(&root, &manifest, false)
            .expect("bounded probe accepts exact names and lengths");
        assert!(
            verify_model_dir(&root, &manifest).is_err(),
            "first-use SHA-256 admission must reject same-length tampering"
        );
        fs::remove_dir_all(root).expect("remove candidate");
    }

    #[test]
    fn mutable_ref_and_wrong_snapshot_are_never_admission_authority() {
        let root = temporary_root("immutable-revision");
        let repository = root.join("models--altunenes--parakeet-rs");
        let manifest = production_manifest().expect("checked-in manifest");
        let wrong = repository
            .join("snapshots/wrong-revision")
            .join(&manifest.subdirectory);
        fs::create_dir_all(&wrong).expect("create wrong snapshot");
        fs::create_dir_all(repository.join("refs")).expect("create refs");
        fs::write(repository.join("refs/main"), "wrong-revision\n").expect("write mutable ref");
        assert_eq!(exact_hugging_face_candidate_in_root(&root, &manifest), None);

        let exact = repository
            .join("snapshots")
            .join(&manifest.revision)
            .join(&manifest.subdirectory);
        fs::create_dir_all(&exact).expect("create exact snapshot directory");
        assert_eq!(
            exact_hugging_face_candidate_in_root(&root, &manifest),
            Some(exact)
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }
}
