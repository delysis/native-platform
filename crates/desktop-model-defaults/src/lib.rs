//! Desktop product policy. Discovery reads only the pinned local cache entry;
//! it never downloads, loads a model, or replaces an explicit model selection.

use std::path::{Path, PathBuf};

pub const GEMMA_REPOSITORY: &str = "google/gemma-4-12B-it-qat-q4_0-gguf";
pub const GEMMA_REVISION: &str = "29d097773436b69ff9feafd636ab4cf873786537";
pub const GEMMA_ARTIFACT_NAME: &str = "gemma-4-12b-it-qat-q4_0.gguf";
pub const GEMMA_DOWNLOAD_URL: &str = "https://huggingface.co/google/gemma-4-12B-it-qat-q4_0-gguf/resolve/29d097773436b69ff9feafd636ab4cf873786537/gemma-4-12b-it-qat-q4_0.gguf?download=true";
pub const GEMMA_SHA256: &str = "93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b";
pub const GEMMA_ARTIFACT_BYTES: u64 = 6_975_879_296;
pub const GEMMA_PROJECTOR_NAME: &str = "mmproj-gemma-4-12b-it-qat-q4_0.gguf";
pub const GEMMA_PROJECTOR_DOWNLOAD_URL: &str = "https://huggingface.co/google/gemma-4-12B-it-qat-q4_0-gguf/resolve/29d097773436b69ff9feafd636ab4cf873786537/mmproj-gemma-4-12b-it-qat-q4_0.gguf?download=true";
pub const GEMMA_PROJECTOR_SHA256: &str =
    "cb018338a7538a9814d994bfe54644c71eb7ed54e31eae2f721e45fd3c260da7";
pub const GEMMA_PROJECTOR_BYTES: u64 = 175_115_616;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedDefaultModel {
    /// Keep the snapshot alias so filename and projector pairing survive the
    /// cache's content-addressed blob symlink.
    pub model: PathBuf,
    pub projector: Option<PathBuf>,
}

#[must_use]
pub fn hugging_face_hub_cache_dir() -> Option<PathBuf> {
    cache_dir_from_env(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
}

fn cache_dir_from_env(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(path) = env("HF_HUB_CACHE").or_else(|| env("HUGGINGFACE_HUB_CACHE")) {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = env("HF_HOME") {
        return Some(PathBuf::from(path).join("hub"));
    }
    if let Some(path) = env("XDG_CACHE_HOME") {
        return Some(PathBuf::from(path).join("huggingface/hub"));
    }
    let home = env("HOME").or_else(|| env("USERPROFILE")).or_else(|| {
        let mut drive = env("HOMEDRIVE")?;
        drive.push(env("HOMEPATH")?);
        Some(drive)
    })?;
    Some(PathBuf::from(home).join(".cache/huggingface/hub"))
}

/// This is a local candidate, not an authenticity or inference proof. The
/// normal product validation and native inspection still apply before use.
#[must_use]
pub fn cached_default_model(cache: &Path) -> Option<CachedDefaultModel> {
    let snapshot = cache
        .join(format!("models--{}", GEMMA_REPOSITORY.replace('/', "--")))
        .join("snapshots")
        .join(GEMMA_REVISION);
    let model = snapshot.join(GEMMA_ARTIFACT_NAME);
    if !model.is_file() {
        return None;
    }
    let projector = snapshot.join(GEMMA_PROJECTOR_NAME);
    Some(CachedDefaultModel {
        model,
        projector: projector.is_file().then_some(projector),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_environment_precedence_uses_the_selected_cache_only() {
        let values = [
            ("HOME", "/home/person"),
            ("XDG_CACHE_HOME", "/xdg"),
            ("HF_HOME", "/hf"),
            ("HUGGINGFACE_HUB_CACHE", "/old-hub"),
            ("HF_HUB_CACHE", "/hub"),
        ];
        for (count, expected) in [
            (1, "/home/person/.cache/huggingface/hub"),
            (2, "/xdg/huggingface/hub"),
            (3, "/hf/hub"),
            (4, "/old-hub"),
            (5, "/hub"),
        ] {
            let path = cache_dir_from_env(|key| {
                values[..count]
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| std::ffi::OsString::from(*value))
            });
            assert_eq!(path, Some(PathBuf::from(expected)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn pinned_snapshot_accepts_blob_symlinks_and_never_substitutes_another_model() {
        let temporary = std::env::temp_dir().join(format!(
            "desktop-default-model-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let repository = temporary.join(format!("models--{}", GEMMA_REPOSITORY.replace('/', "--")));
        let snapshot = repository.join("snapshots").join(GEMMA_REVISION);
        std::fs::create_dir_all(&snapshot).expect("snapshot");
        std::fs::write(snapshot.join("qwen.gguf"), b"GGUF").expect("unrelated model");
        assert_eq!(cached_default_model(&temporary), None);
        let blob = repository.join("blob");
        std::fs::write(&blob, b"GGUF").expect("blob");
        let model = snapshot.join(GEMMA_ARTIFACT_NAME);
        std::os::unix::fs::symlink(&blob, &model).expect("cache alias");
        assert_eq!(
            cached_default_model(&temporary),
            Some(CachedDefaultModel {
                model: model.clone(),
                projector: None
            })
        );
        let projector = snapshot.join(GEMMA_PROJECTOR_NAME);
        std::fs::write(&projector, b"GGUF").expect("projector");
        assert_eq!(
            cached_default_model(&temporary),
            Some(CachedDefaultModel {
                model,
                projector: Some(projector)
            })
        );
        std::fs::remove_file(blob).expect("remove blob");
        assert_eq!(cached_default_model(&temporary), None);
        std::fs::remove_dir_all(temporary).expect("remove fixture");
    }
}
