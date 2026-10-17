//! Pinned first-run model acquisition for Blessenger.
//!
//! This module is deliberately app-owned: the product runtime remains offline
//! and receives only already verified local artifacts. Every URL contains an
//! immutable Hub revision and every installed blob is named by its SHA-256.

use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::{StatusCode, redirect};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;
const MAX_ATTEMPTS: usize = 3;
const PROGRESS_INTERVAL_BYTES: u64 = 8 * 1024 * 1024;
const PERSONA_MANIFEST_SHA256: &str =
    "4c7b786b36c7b3643e7119e52b506d1e81178e3294246dbdd4c108e52deb47e1";

#[derive(Clone, Copy, Debug)]
struct Artifact {
    id: &'static str,
    repository: &'static str,
    revision: &'static str,
    relative: &'static str,
    sha256: &'static str,
    bytes: u64,
}

impl Artifact {
    fn url(self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}?download=true",
            self.repository, self.revision, self.relative
        )
    }
}

const QWEN_REPOSITORY: &str = speech_native_backend_qwen::QWEN_ICL_MODEL_REPOSITORY;
const QWEN_REVISION: &str = speech_native_backend_qwen::QWEN_ICL_MODEL_REVISION;

const REQUIRED_ARTIFACTS: &[Artifact] = &[
    Artifact {
        id: "gemma",
        repository: desktop_model_defaults::GEMMA_REPOSITORY,
        revision: desktop_model_defaults::GEMMA_REVISION,
        relative: desktop_model_defaults::GEMMA_ARTIFACT_NAME,
        sha256: desktop_model_defaults::GEMMA_SHA256,
        bytes: desktop_model_defaults::GEMMA_ARTIFACT_BYTES,
    },
    Artifact {
        id: "projector",
        repository: desktop_model_defaults::GEMMA_REPOSITORY,
        revision: desktop_model_defaults::GEMMA_REVISION,
        relative: desktop_model_defaults::GEMMA_PROJECTOR_NAME,
        sha256: desktop_model_defaults::GEMMA_PROJECTOR_SHA256,
        bytes: desktop_model_defaults::GEMMA_PROJECTOR_BYTES,
    },
    Artifact {
        id: "mtp",
        repository: desktop_model_defaults::GEMMA_MTP_REPOSITORY,
        revision: desktop_model_defaults::GEMMA_MTP_REVISION,
        relative: desktop_model_defaults::GEMMA_MTP_ARTIFACT_NAME,
        sha256: desktop_model_defaults::GEMMA_MTP_SHA256,
        bytes: desktop_model_defaults::GEMMA_MTP_BYTES,
    },
    Artifact {
        id: "qwen-config",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "config.json",
        sha256: "39ffdadc03c1a7c7f8116ee8830d6a577ac87039edcbd88759b4fcc4db272070",
        bytes: 5_317,
    },
    Artifact {
        id: "qwen-generation",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "generation_config.json",
        sha256: "f1b90b4513f3b34c62851049e2492d7b4c5940daf1276f89c82b8ef04127f3aa",
        bytes: 245,
    },
    Artifact {
        id: "qwen-merges",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "merges.txt",
        sha256: "599bab54075088774b1733fde865d5bd747cbcc7a547c5bc12610e874e26f5e3",
        bytes: 1_671_839,
    },
    Artifact {
        id: "qwen-weights",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "model.safetensors",
        sha256: "81fb76175ff74e69be25fef2cc3e54f016df3034f1514c8e1c89da06a3510cff",
        bytes: 3_857_414_009,
    },
    Artifact {
        id: "qwen-index",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "model.safetensors.index.json",
        sha256: "f5bb337c2f77c5046024c7342ed8d6ade28fdfbab862baf9f13269743b920005",
        bytes: 38_742,
    },
    Artifact {
        id: "qwen-preprocessor",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "preprocessor_config.json",
        sha256: "efdde1022ea9d76928bf7a9cd53139138f5ba2e466e837f08f6105ab1af1c119",
        bytes: 127,
    },
    Artifact {
        id: "qwen-codec-config",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "speech_tokenizer/config.json",
        sha256: "ee65bb901c876664ab8707c487157aa1a6ee57c65969b28fb5ec9dc211e68167",
        bytes: 2_336,
    },
    Artifact {
        id: "qwen-codec-configuration",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "speech_tokenizer/configuration.json",
        sha256: "6bc26d64eb5024b4d1dab5a52371958b429256d6c9d59787f1f5294a54e0cebd",
        bytes: 76,
    },
    Artifact {
        id: "qwen-codec-weights",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "speech_tokenizer/model.safetensors",
        sha256: "836b7b357f5ea43e889936a3709af68dfe3751881acefe4ecf0dbd30ba571258",
        bytes: 682_293_092,
    },
    Artifact {
        id: "qwen-codec-preprocessor",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "speech_tokenizer/preprocessor_config.json",
        sha256: "fcb3805e597e786d4067706e602f6688524640f8d3396790e2e09b5942fcbdfb",
        bytes: 234,
    },
    Artifact {
        id: "qwen-tokenizer",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "tokenizer_config.json",
        sha256: "dc3c31c3bdaedd5016382bb3cbe07323026775ad51f5a4fb564505992ae4a670",
        bytes: 7_344,
    },
    Artifact {
        id: "qwen-vocabulary",
        repository: QWEN_REPOSITORY,
        revision: QWEN_REVISION,
        relative: "vocab.json",
        sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
        bytes: 2_776_833,
    },
];

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AcquisitionProgress {
    pub phase: &'static str,
    pub artifact: &'static str,
    pub completed_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct VoiceProgress {
    pub persona_id: String,
    pub persona: String,
    pub voice: String,
    pub completed: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SetupState {
    pub first_run: bool,
    pub phase: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct AcquiredModels {
    pub gemma: PathBuf,
    pub projector: PathBuf,
    pub mtp_assistant: PathBuf,
    pub qwen_root: PathBuf,
    pub verified_model_artifacts: Option<mom_llama_runtime::config::VerifiedModelArtifacts>,
}

struct AcquiredArtifact {
    path: PathBuf,
    verified_identity: Option<String>,
}

fn file_identity(metadata: &fs::Metadata) -> String {
    mom_llama_runtime::engine::model_artifact_file_identity(metadata)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AcquisitionError {
    #[error("Hugging Face cache location is unavailable")]
    MissingCache,
    #[error("invalid pinned model manifest entry {0}")]
    InvalidManifest(&'static str),
    #[error("{artifact} cache path is unsafe: {detail}")]
    UnsafePath {
        artifact: &'static str,
        detail: String,
    },
    #[error("timed out waiting for another {0} download")]
    LockTimeout(&'static str),
    #[error("{artifact} has {actual} bytes; expected {expected}")]
    SizeMismatch {
        artifact: &'static str,
        expected: u64,
        actual: u64,
    },
    #[error("{0} failed SHA-256 verification")]
    DigestMismatch(&'static str),
    #[error("{artifact} download failed after {attempts} attempts: {detail}")]
    Network {
        artifact: &'static str,
        attempts: usize,
        detail: String,
    },
    #[error("could not {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("persona voice archive is invalid: {0}")]
    VoiceArchive(String),
}

#[derive(Deserialize)]
struct VoiceManifest {
    entries: Vec<VoiceEntry>,
}

#[derive(Deserialize)]
struct VoiceEntry {
    persona_id: String,
    speaker: String,
    status: String,
    clip_path: String,
    clip_sha256: String,
}

const EXPECTED_PERSONAS: &[&str] = &[
    "ad_de_jongh",
    "arnold_popky",
    "bessel_van_der_kolk",
    "christine_courtois",
    "dolores_mosquera",
    "francine_shapiro",
    "gabor_mate",
    "janina_fisher",
    "jim_knipe",
    "judith_herman",
    "peter_levine",
    "richard_schwartz",
    "robert_miller",
    "shirley_jean_schmidt",
];

pub(crate) fn validate_persona_voices(
    root: &Path,
    mut progress: impl FnMut(VoiceProgress),
) -> Result<(), AcquisitionError> {
    let manifest_path = root.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path).map_err(|source| AcquisitionError::Io {
        operation: "read persona voice manifest",
        path: manifest_path.clone(),
        source,
    })?;
    if format!("{:x}", Sha256::digest(&manifest_bytes)) != PERSONA_MANIFEST_SHA256 {
        return Err(AcquisitionError::VoiceArchive(
            "the packaged persona manifest failed verification".to_string(),
        ));
    }
    let manifest: VoiceManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| AcquisitionError::VoiceArchive(error.to_string()))?;
    if manifest.entries.len() != EXPECTED_PERSONAS.len() {
        return Err(AcquisitionError::VoiceArchive(format!(
            "found {} voices; expected {}",
            manifest.entries.len(),
            EXPECTED_PERSONAS.len()
        )));
    }
    for (index, expected) in EXPECTED_PERSONAS.iter().enumerate() {
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.persona_id == *expected)
            .ok_or_else(|| AcquisitionError::VoiceArchive(format!("missing {expected}")))?;
        if entry.status != "collected" || !safe_relative(&entry.clip_path) {
            return Err(AcquisitionError::VoiceArchive(format!(
                "{} is not an admitted local voice",
                entry.speaker
            )));
        }
        let clip = root.join(&entry.clip_path);
        let mut file = File::open(&clip).map_err(|source| AcquisitionError::Io {
            operation: "open persona voice",
            path: clip.clone(),
            source,
        })?;
        let length = file
            .metadata()
            .map_err(|source| AcquisitionError::Io {
                operation: "inspect persona voice",
                path: clip.clone(),
                source,
            })?
            .len();
        let digest = format!(
            "{:x}",
            hash_prefix(&mut file, length)
                .map_err(|source| AcquisitionError::Io {
                    operation: "verify persona voice",
                    path: clip,
                    source
                })?
                .finalize()
        );
        if digest != entry.clip_sha256 {
            return Err(AcquisitionError::VoiceArchive(format!(
                "{} voice failed verification",
                entry.speaker
            )));
        }
        progress(VoiceProgress {
            persona_id: entry.persona_id.clone(),
            persona: entry.speaker.clone(),
            voice: format!("{} voice", entry.speaker),
            completed: index + 1,
            total: EXPECTED_PERSONAS.len(),
        });
    }
    Ok(())
}

pub(crate) fn installation_receipt_is_current(app_data_dir: &Path) -> bool {
    fs::read_to_string(app_data_dir.join("setup-v1.receipt"))
        .is_ok_and(|contents| contents == installation_receipt_contents())
}

pub(crate) fn write_installation_receipt(app_data_dir: &Path) -> Result<(), AcquisitionError> {
    real_dir(app_data_dir, "installation-receipt")?;
    let destination = app_data_dir.join("setup-v1.receipt");
    let temporary = app_data_dir.join("setup-v1.receipt.tmp");
    fs::write(&temporary, installation_receipt_contents()).map_err(|source| {
        AcquisitionError::Io {
            operation: "write installation receipt",
            path: temporary.clone(),
            source,
        }
    })?;
    fs::rename(&temporary, &destination).map_err(|source| AcquisitionError::Io {
        operation: "publish installation receipt",
        path: destination,
        source,
    })
}

fn installation_receipt_contents() -> String {
    let mut receipt = String::from("blessenger-voice-setup-v1\n");
    for artifact in REQUIRED_ARTIFACTS {
        receipt.push_str(artifact.id);
        receipt.push(' ');
        receipt.push_str(artifact.sha256);
        receipt.push('\n');
    }
    receipt.push_str("persona-archive ");
    receipt.push_str(PERSONA_MANIFEST_SHA256);
    receipt.push('\n');
    receipt
}

#[derive(Debug)]
struct CachePaths {
    repository: PathBuf,
    blob: PathBuf,
    snapshot: PathBuf,
    partial: PathBuf,
    lock: PathBuf,
}

pub(crate) fn acquire_required_models(
    verify_existing: bool,
    mut progress: impl FnMut(AcquisitionProgress),
) -> Result<AcquiredModels, AcquisitionError> {
    let root = desktop_model_defaults::hugging_face_hub_cache_dir()
        .ok_or(AcquisitionError::MissingCache)?;
    acquire_required_models_in(&root, verify_existing, &mut progress)
}

fn acquire_required_models_in(
    root: &Path,
    verify_existing: bool,
    progress: &mut impl FnMut(AcquisitionProgress),
) -> Result<AcquiredModels, AcquisitionError> {
    let total_bytes = REQUIRED_ARTIFACTS.iter().map(|item| item.bytes).sum();
    let client = client().map_err(|detail| AcquisitionError::Network {
        artifact: "manifest",
        attempts: 0,
        detail,
    })?;
    let mut completed_bytes = 0_u64;
    let mut installed = Vec::with_capacity(REQUIRED_ARTIFACTS.len());
    for artifact in REQUIRED_ARTIFACTS {
        progress(AcquisitionProgress {
            phase: "checking",
            artifact: artifact.id,
            completed_bytes,
            total_bytes,
        });
        let base = completed_bytes;
        let path = acquire_one(root, *artifact, &client, verify_existing, |downloaded| {
            progress(AcquisitionProgress {
                phase: "downloading",
                artifact: artifact.id,
                completed_bytes: base.saturating_add(downloaded.min(artifact.bytes)),
                total_bytes,
            });
        })?;
        completed_bytes = completed_bytes.saturating_add(artifact.bytes);
        progress(AcquisitionProgress {
            phase: "verified",
            artifact: artifact.id,
            completed_bytes,
            total_bytes,
        });
        installed.push(path);
    }
    let qwen_root = cache_paths(root, REQUIRED_ARTIFACTS[3])?
        .snapshot
        .parent()
        .ok_or(AcquisitionError::InvalidManifest("qwen-config"))?
        .to_path_buf();
    Ok(AcquiredModels {
        gemma: installed[0].path.clone(),
        projector: installed[1].path.clone(),
        mtp_assistant: installed[2].path.clone(),
        qwen_root,
        verified_model_artifacts: installed[0]
            .verified_identity
            .as_ref()
            .zip(installed[1].verified_identity.as_ref())
            .map(|(model_identity, projector_identity)| {
                use mom_llama_runtime::config::{VerifiedArtifactDigest, VerifiedModelArtifacts};
                VerifiedModelArtifacts {
                    model: VerifiedArtifactDigest {
                        path: installed[0].path.clone(),
                        sha256: REQUIRED_ARTIFACTS[0].sha256.to_owned(),
                        file_identity: model_identity.clone(),
                    },
                    projector: Some(VerifiedArtifactDigest {
                        path: installed[1].path.clone(),
                        sha256: REQUIRED_ARTIFACTS[1].sha256.to_owned(),
                        file_identity: projector_identity.clone(),
                    }),
                }
            }),
    })
}

fn client() -> Result<Client, String> {
    let redirects = redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https"
            || !attempt.url().username().is_empty()
            || attempt.url().password().is_some()
            || attempt.previous().len() >= MAX_REDIRECTS
        {
            attempt.error("unsafe or excessive model redirect")
        } else {
            attempt.follow()
        }
    });
    Client::builder()
        .redirect(redirects)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())
}

fn acquire_one(
    root: &Path,
    artifact: Artifact,
    client: &Client,
    verify_existing: bool,
    mut progress: impl FnMut(u64),
) -> Result<AcquiredArtifact, AcquisitionError> {
    let paths = cache_paths(root, artifact)?;
    real_dir(paths.blob.parent().expect("blob parent"), artifact.id)?;
    real_dir(
        paths.snapshot.parent().expect("snapshot parent"),
        artifact.id,
    )?;
    real_dir(paths.lock.parent().expect("lock parent"), artifact.id)?;
    let lock = open_lock(&paths.lock, artifact.id)?;
    wait_for_lock(&lock, artifact.id)?;

    match verified_snapshot(&paths, artifact) {
        Ok(true) => {
            let verified_identity = if verify_existing {
                Some(verify_regular_file(&paths.blob, artifact)?)
            } else {
                None
            };
            progress(artifact.bytes);
            return Ok(AcquiredArtifact {
                path: paths.snapshot,
                verified_identity,
            });
        }
        Ok(false) => {}
        Err(AcquisitionError::SizeMismatch { .. }) => {
            let _ = fs::remove_file(&paths.snapshot);
            let _ = fs::remove_file(&paths.blob);
        }
        Err(error) => return Err(error),
    }
    if paths.blob.exists() {
        match verify_regular_file(&paths.blob, artifact) {
            Ok(identity) => {
                publish(&paths, artifact)?;
                progress(artifact.bytes);
                return Ok(AcquiredArtifact {
                    path: paths.snapshot,
                    verified_identity: Some(identity),
                });
            }
            Err(AcquisitionError::SizeMismatch { .. } | AcquisitionError::DigestMismatch(_)) => {
                fs::remove_file(&paths.blob).map_err(|source| AcquisitionError::Io {
                    operation: "remove invalid cached blob",
                    path: paths.blob.clone(),
                    source,
                })?;
            }
            Err(error) => return Err(error),
        }
    }

    let mut last_error = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        match download_attempt(&paths, artifact, client, &artifact.url(), &mut progress) {
            Ok(identity) => {
                publish(&paths, artifact)?;
                return Ok(AcquiredArtifact {
                    path: paths.snapshot,
                    verified_identity: Some(identity),
                });
            }
            Err(error) => {
                last_error = error;
                if attempt < MAX_ATTEMPTS {
                    thread::sleep(Duration::from_secs(u64::try_from(attempt).unwrap_or(1)));
                }
            }
        }
    }
    Err(AcquisitionError::Network {
        artifact: artifact.id,
        attempts: MAX_ATTEMPTS,
        detail: last_error,
    })
}

fn download_attempt(
    paths: &CachePaths,
    artifact: Artifact,
    client: &Client,
    url: &str,
    progress: &mut impl FnMut(u64),
) -> Result<String, String> {
    let mut partial = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&paths.partial)
        .map_err(|error| format!("open partial: {error}"))?;
    let mut offset = partial.metadata().map_err(|error| error.to_string())?.len();
    if offset > artifact.bytes {
        return Err(format!(
            "partial has {offset} bytes, exceeding {}",
            artifact.bytes
        ));
    }
    let mut hasher = hash_prefix(&mut partial, offset).map_err(|error| error.to_string())?;
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
    }
    let mut response = request.send().map_err(|error| error.to_string())?;
    reject_encoded(&response)?;
    if offset > 0 && response.status() == StatusCode::OK {
        partial.set_len(0).map_err(|error| error.to_string())?;
        partial
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        offset = 0;
        hasher = Sha256::new();
    } else if offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT {
        validate_content_range(&response, offset, artifact.bytes)?;
    } else if offset == 0 && response.status() != StatusCode::OK {
        return Err(format!("HTTP {}", response.status().as_u16()));
    } else if offset > 0 {
        return Err(format!(
            "HTTP {} while resuming",
            response.status().as_u16()
        ));
    }
    validate_content_length(&response, artifact.bytes.saturating_sub(offset))?;
    partial
        .seek(SeekFrom::End(0))
        .map_err(|error| error.to_string())?;
    let mut downloaded = offset;
    let mut next_progress = downloaded.saturating_add(PROGRESS_INTERVAL_BYTES);
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let count = response
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        downloaded = downloaded
            .checked_add(u64::try_from(count).map_err(|error| error.to_string())?)
            .ok_or_else(|| "download byte count overflow".to_string())?;
        if downloaded > artifact.bytes {
            return Err(format!("response exceeded {} bytes", artifact.bytes));
        }
        partial
            .write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        hasher.update(&buffer[..count]);
        if downloaded >= next_progress {
            progress(downloaded);
            next_progress = downloaded.saturating_add(PROGRESS_INTERVAL_BYTES);
        }
    }
    if downloaded != artifact.bytes {
        return Err(format!(
            "download ended at {downloaded} bytes; expected {}",
            artifact.bytes
        ));
    }
    partial.sync_all().map_err(|error| error.to_string())?;
    let digest = format!("{:x}", hasher.finalize());
    if digest != artifact.sha256 {
        return Err("SHA-256 mismatch".to_string());
    }
    // Keep the verified download descriptor open across publication. Metadata
    // equality binds the receipt to the exact published file without rereading it.
    fs::rename(&paths.partial, &paths.blob).map_err(|error| error.to_string())?;
    let identity = file_identity(&partial.metadata().map_err(|error| error.to_string())?);
    let published = file_identity(&fs::metadata(&paths.blob).map_err(|error| error.to_string())?);
    if identity != published {
        return Err("published model file changed".into());
    }
    progress(downloaded);
    Ok(identity)
}

fn reject_encoded(response: &Response) -> Result<(), String> {
    if response
        .headers()
        .get(CONTENT_ENCODING)
        .is_some_and(|value| {
            value
                .to_str()
                .map_or(true, |value| !value.eq_ignore_ascii_case("identity"))
        })
    {
        return Err("server returned encoded model bytes".to_string());
    }
    Ok(())
}

fn validate_content_length(response: &Response, expected: u64) -> Result<(), String> {
    if let Some(value) = response.headers().get(CONTENT_LENGTH) {
        let actual = value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| "invalid Content-Length".to_string())?;
        if actual != expected {
            return Err(format!("remote length {actual}; expected {expected}"));
        }
    }
    Ok(())
}

fn validate_content_range(response: &Response, offset: u64, expected: u64) -> Result<(), String> {
    let range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "resume response omitted Content-Range".to_string())?;
    let expected_prefix = format!("bytes {offset}-");
    if !range.starts_with(&expected_prefix) || !range.ends_with(&format!("/{expected}")) {
        return Err(format!("unexpected Content-Range {range}"));
    }
    Ok(())
}

fn cache_paths(root: &Path, artifact: Artifact) -> Result<CachePaths, AcquisitionError> {
    if artifact.repository.split('/').count() != 2
        || artifact.revision.len() != 40
        || !artifact
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !safe_relative(artifact.relative)
        || artifact.bytes == 0
    {
        return Err(AcquisitionError::InvalidManifest(artifact.id));
    }
    let repo_name = format!("models--{}", artifact.repository.replace('/', "--"));
    let repository = root.join(&repo_name);
    let blob = repository.join("blobs").join(artifact.sha256);
    Ok(CachePaths {
        partial: repository
            .join("blobs")
            .join(format!("{}.part", artifact.sha256)),
        snapshot: repository
            .join("snapshots")
            .join(artifact.revision)
            .join(artifact.relative),
        lock: root
            .join(".locks")
            .join(repo_name)
            .join(format!("{}.lock", artifact.sha256)),
        repository,
        blob,
    })
}

fn safe_relative(relative: &str) -> bool {
    !relative.is_empty()
        && Path::new(relative).components().all(|component| {
            matches!(component, std::path::Component::Normal(value) if !value.is_empty())
        })
}

fn real_dir(path: &Path, artifact: &'static str) -> Result<(), AcquisitionError> {
    fs::create_dir_all(path).map_err(|source| AcquisitionError::Io {
        operation: "create cache directory",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| AcquisitionError::Io {
        operation: "inspect cache directory",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AcquisitionError::UnsafePath {
            artifact,
            detail: format!("{} is not a real directory", path.display()),
        });
    }
    Ok(())
}

fn open_lock(path: &Path, artifact: &'static str) -> Result<File, AcquisitionError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(AcquisitionError::UnsafePath {
            artifact,
            detail: "cache lock is a symlink".to_string(),
        });
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| AcquisitionError::Io {
            operation: "open cache lock",
            path: path.to_path_buf(),
            source,
        })
}

fn wait_for_lock(file: &File, artifact: &'static str) -> Result<(), AcquisitionError> {
    let started = Instant::now();
    loop {
        match <File as fs4::FileExt>::try_lock(file) {
            Ok(()) => return Ok(()),
            Err(fs4::TryLockError::WouldBlock) => {
                if started.elapsed() >= LOCK_TIMEOUT {
                    return Err(AcquisitionError::LockTimeout(artifact));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(fs4::TryLockError::Error(source)) => {
                return Err(AcquisitionError::Io {
                    operation: "lock cache artifact",
                    path: PathBuf::from(artifact),
                    source,
                });
            }
        }
    }
}

fn verified_snapshot(paths: &CachePaths, artifact: Artifact) -> Result<bool, AcquisitionError> {
    let metadata = match fs::symlink_metadata(&paths.snapshot) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(AcquisitionError::Io {
                operation: "inspect cached snapshot",
                path: paths.snapshot.clone(),
                source,
            });
        }
    };
    if !metadata.is_file() && !metadata.file_type().is_symlink() {
        return Err(AcquisitionError::UnsafePath {
            artifact: artifact.id,
            detail: "snapshot is not a file".to_string(),
        });
    }
    let resolved = fs::canonicalize(&paths.snapshot).map_err(|source| AcquisitionError::Io {
        operation: "resolve cached snapshot",
        path: paths.snapshot.clone(),
        source,
    })?;
    let expected = fs::canonicalize(&paths.blob).unwrap_or_else(|_| paths.blob.clone());
    if resolved != expected {
        return Err(AcquisitionError::UnsafePath {
            artifact: artifact.id,
            detail: "snapshot does not resolve to its pinned content-addressed blob".to_string(),
        });
    }
    let actual = fs::metadata(resolved)
        .map_err(|source| AcquisitionError::Io {
            operation: "inspect cached blob",
            path: paths.blob.clone(),
            source,
        })?
        .len();
    if actual != artifact.bytes {
        return Err(AcquisitionError::SizeMismatch {
            artifact: artifact.id,
            expected: artifact.bytes,
            actual,
        });
    }
    Ok(true)
}

fn verify_regular_file(path: &Path, artifact: Artifact) -> Result<String, AcquisitionError> {
    let mut file = File::open(path).map_err(|source| AcquisitionError::Io {
        operation: "open cached blob",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| AcquisitionError::Io {
        operation: "inspect cached blob",
        path: path.to_path_buf(),
        source,
    })?;
    let actual = metadata.len();
    if !metadata.is_file() || actual != artifact.bytes {
        return Err(AcquisitionError::SizeMismatch {
            artifact: artifact.id,
            expected: artifact.bytes,
            actual,
        });
    }
    let before = file_identity(&metadata);
    let digest = hash_prefix(&mut file, actual).map_err(|source| AcquisitionError::Io {
        operation: "hash cached blob",
        path: path.to_path_buf(),
        source,
    })?;
    if format!("{:x}", digest.finalize()) != artifact.sha256 {
        return Err(AcquisitionError::DigestMismatch(artifact.id));
    }
    let after = file.metadata().map_err(|source| AcquisitionError::Io {
        operation: "reinspect verified blob",
        path: path.to_path_buf(),
        source,
    })?;
    let path_after = fs::metadata(path).map_err(|source| AcquisitionError::Io {
        operation: "reinspect verified path",
        path: path.to_path_buf(),
        source,
    })?;
    if before != file_identity(&after) || before != file_identity(&path_after) {
        return Err(AcquisitionError::UnsafePath {
            artifact: artifact.id,
            detail: "file changed during verification".into(),
        });
    }
    Ok(before)
}

fn hash_prefix(file: &mut File, bytes: u64) -> io::Result<Sha256> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = bytes;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = file.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short partial file",
            ));
        }
        hasher.update(&buffer[..count]);
        remaining -= u64::try_from(count).unwrap_or(0);
    }
    Ok(hasher)
}

fn publish(paths: &CachePaths, artifact: Artifact) -> Result<(), AcquisitionError> {
    if verified_snapshot(paths, artifact)? {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let parent = paths.snapshot.parent().expect("snapshot parent");
        let depth = parent
            .strip_prefix(&paths.repository)
            .map_err(|_| AcquisitionError::InvalidManifest(artifact.id))?
            .components()
            .count();
        let mut relative = PathBuf::new();
        for _ in 0..depth {
            relative.push("..");
        }
        relative.push("blobs");
        relative.push(artifact.sha256);
        std::os::unix::fs::symlink(relative, &paths.snapshot).map_err(|source| {
            AcquisitionError::Io {
                operation: "publish cache snapshot",
                path: paths.snapshot.clone(),
                source,
            }
        })?;
    }
    #[cfg(not(unix))]
    fs::hard_link(&paths.blob, &paths.snapshot).map_err(|source| AcquisitionError::Io {
        operation: "publish cache snapshot",
        path: paths.snapshot.clone(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const BYTES: &[u8] = b"pinned fixture";
    const FIXTURE: Artifact = Artifact {
        id: "fixture",
        repository: "owner/repo",
        revision: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        relative: "nested/model.bin",
        sha256: "20899fac2e03608f209e7a72eb3fc133b543111a21b15a4bd98ceee8701cd8e5",
        bytes: 14,
    };

    #[test]
    fn pinned_cache_layout_is_hugging_face_compatible() {
        let paths = cache_paths(Path::new("/cache/hub"), FIXTURE).expect("paths");
        assert_eq!(
            paths.snapshot,
            Path::new("/cache/hub/models--owner--repo/snapshots")
                .join(FIXTURE.revision)
                .join(FIXTURE.relative)
        );
        assert_eq!(
            paths.blob,
            Path::new("/cache/hub/models--owner--repo/blobs").join(FIXTURE.sha256)
        );
    }

    #[test]
    fn interrupted_download_resumes_from_local_fixture_server() {
        let temporary = tempfile::tempdir().expect("temporary cache");
        let paths = cache_paths(temporary.path(), FIXTURE).expect("paths");
        real_dir(paths.blob.parent().expect("blob parent"), FIXTURE.id).expect("blob dir");
        fs::write(&paths.partial, &BYTES[..7]).expect("partial");

        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture server");
        let address = listener.local_addr().expect("fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixture request");
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.to_ascii_lowercase().contains("range: bytes=7-"));
            stream
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 7\r\nContent-Range: bytes 7-13/14\r\nConnection: close\r\n\r\n",
                )
                .expect("headers");
            stream.write_all(&BYTES[7..]).expect("body");
        });

        let mut observed = Vec::new();
        let identity = download_attempt(
            &paths,
            FIXTURE,
            &client().expect("client"),
            &format!("http://{address}/fixture"),
            &mut |bytes| observed.push(bytes),
        )
        .expect("resumed download");
        server.join().expect("fixture server completed");
        assert_eq!(fs::read(&paths.blob).expect("blob"), BYTES);
        assert_eq!(observed.last(), Some(&FIXTURE.bytes));
        assert_eq!(
            identity,
            file_identity(&fs::metadata(&paths.blob).expect("blob metadata"))
        );
    }

    #[test]
    fn verified_blob_publishes_once_and_reopens_without_network() {
        let root = tempfile::tempdir().expect("temp");
        let paths = cache_paths(root.path(), FIXTURE).expect("paths");
        real_dir(paths.blob.parent().expect("blob parent"), FIXTURE.id).expect("blob dir");
        real_dir(
            paths.snapshot.parent().expect("snapshot parent"),
            FIXTURE.id,
        )
        .expect("snapshot dir");
        fs::write(&paths.blob, BYTES).expect("blob");
        verify_regular_file(&paths.blob, FIXTURE).expect("digest");
        publish(&paths, FIXTURE).expect("publish");
        assert!(verified_snapshot(&paths, FIXTURE).expect("cached"));
        publish(&paths, FIXTURE).expect("idempotent publish");
    }

    #[test]
    fn verified_existing_artifact_carries_file_identity_but_presence_probe_does_not() {
        let root = tempfile::tempdir().expect("temporary cache");
        let paths = cache_paths(root.path(), FIXTURE).expect("cache paths");
        real_dir(paths.blob.parent().expect("blob parent"), FIXTURE.id)
            .expect("create blob directory");
        real_dir(
            paths.snapshot.parent().expect("snapshot parent"),
            FIXTURE.id,
        )
        .expect("create snapshot directory");
        fs::write(&paths.blob, BYTES).expect("write verified fixture bytes");
        publish(&paths, FIXTURE).expect("publish fixture");
        let acquired = acquire_one(
            root.path(),
            FIXTURE,
            &client().expect("HTTP client"),
            true,
            |_| {},
        )
        .expect("verified acquisition");
        let identity = acquired
            .verified_identity
            .expect("verified acquisition carries file identity");
        assert_eq!(
            identity,
            file_identity(&fs::metadata(&paths.blob).expect("blob metadata"))
        );
        let probe = acquire_one(
            root.path(),
            FIXTURE,
            &client().expect("HTTP client"),
            false,
            |_| {},
        )
        .expect("presence-only probe");
        assert!(
            probe.verified_identity.is_none(),
            "metadata-only discovery cannot mint verification"
        );
        fs::write(&paths.blob, b"wrong fixture!").expect("replace fixture with wrong bytes");
        assert_ne!(
            identity,
            file_identity(&fs::metadata(&paths.blob).expect("replaced blob metadata"))
        );
        assert!(
            acquire_one(
                root.path(),
                FIXTURE,
                &client().expect("HTTP client"),
                true,
                |_| {}
            )
            .is_err()
        );
    }

    #[test]
    fn wrong_bytes_are_never_admitted_under_a_digest_name() {
        let root = tempfile::tempdir().expect("temp");
        let paths = cache_paths(root.path(), FIXTURE).expect("paths");
        real_dir(paths.blob.parent().expect("blob parent"), FIXTURE.id).expect("blob dir");
        fs::write(&paths.blob, b"wrong fixture!").expect("blob");
        assert!(matches!(
            verify_regular_file(&paths.blob, FIXTURE),
            Err(AcquisitionError::DigestMismatch("fixture"))
        ));
    }

    #[test]
    fn packaged_persona_archive_contains_every_exact_verified_voice() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/speech-voices/qwen-icl");
        let mut checked = Vec::new();
        validate_persona_voices(&root, |progress| checked.push(progress.voice))
            .expect("packaged voices");
        assert_eq!(checked.len(), EXPECTED_PERSONAS.len());
        assert!(checked.iter().any(|voice| voice == "Gabor Maté voice"));
    }

    #[test]
    fn installation_receipt_is_exact_and_durable() {
        let root = tempfile::tempdir().expect("receipt directory");
        assert!(!installation_receipt_is_current(root.path()));
        write_installation_receipt(root.path()).expect("receipt");
        assert!(installation_receipt_is_current(root.path()));
        fs::write(root.path().join("setup-v1.receipt"), "stale").expect("stale receipt");
        assert!(!installation_receipt_is_current(root.path()));
    }
}
