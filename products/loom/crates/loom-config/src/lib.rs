#![forbid(unsafe_code)]

//! Project-owned Mine settings. Reading settings grants no tools, model loading,
//! network access, or document mutation authority.

mod model;
mod workspace;
pub use model::ModelConfig;
pub use workspace::*;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Component, Path};

use desktop_generation_policy::{
    GenerationTask, ProfileError, SamplingOverrides, resolve_sampling,
};
use llama_native_types::SamplingConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub const CONFIG_FILE: &str = ".mine.toml";
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_PROFILES: usize = 64;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GenerationDefaults {
    pub chat: Option<String>,
    pub automatic_prose: Option<String>,
    pub automatic_verse: Option<String>,
    pub manual_writing: Option<String>,
}

impl GenerationDefaults {
    pub fn profile(&self, task: GenerationTask) -> Option<&str> {
        match task {
            GenerationTask::Chat => &self.chat,
            GenerationTask::AutomaticProse => &self.automatic_prose,
            GenerationTask::AutomaticVerse => &self.automatic_verse,
            GenerationTask::ManualWriting => &self.manual_writing,
        }
        .as_deref()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NamedProfile {
    /// One explicitly named UTF-8 file in `.mine/personas/`, copied exactly.
    pub context_file: Option<String>,
    pub sampling: SamplingOverrides,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssistanceConfig {
    pub suggestions: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MineConfig {
    pub version: u32,
    pub model: ModelConfig,
    pub assistance: AssistanceConfig,
    pub workspace: WorkspaceOverrides,
    pub generation: GenerationDefaults,
    pub profiles: BTreeMap<String, NamedProfile>,
    #[serde(skip)]
    source_sha256: Option<String>,
}

impl Default for MineConfig {
    fn default() -> Self {
        Self {
            version: 1,
            model: ModelConfig::default(),
            assistance: AssistanceConfig::default(),
            workspace: WorkspaceOverrides::default(),
            generation: GenerationDefaults::default(),
            profiles: BTreeMap::new(),
            source_sha256: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Mine settings exceed 64 KiB")]
    TooLarge,
    #[error("unsupported Mine settings version {0}")]
    Version(u32),
    #[error("Mine settings: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("workspace settings: {0}")]
    Workspace(String),
    #[error("model settings: {0}")]
    Model(String),
    #[error("the frozen generation profile is invalid")]
    Frozen,
    #[error("profile names use 1 to 64 letters, digits, hyphens, or underscores")]
    ProfileName,
    #[error("Mine settings support at most 64 named profiles")]
    ProfileLimit,
    #[error("the selected profile `{0}` is not defined")]
    MissingProfile(String),
    #[error("profile context must name one Markdown file under .mine/personas/")]
    ContextPath,
    #[error("settings and persona sources must be regular files in real project directories")]
    FileKind,
    #[error("the selected source exceeds its {0} byte limit")]
    SourceLimit(usize),
    #[error("settings and persona sources must contain valid UTF-8")]
    Utf8,
    #[error("settings source could not be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("generation profile: {0}")]
    Sampling(#[from] ProfileError),
    #[error("generation profile encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Frozen request input, separately versioned from any document revision.
/// Context bytes and the requested sampling configuration are evidence; the
/// native generation receipt remains authoritative for applied settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenGenerationProfile {
    pub schema_version: u32,
    pub task: GenerationTask,
    pub config_sha256: Option<String>,
    pub profile_name: Option<String>,
    pub profile_sha256: Option<String>,
    pub sampling: SamplingOverrides,
    pub context: Option<AuthoredContext>,
    /// An explicitly applied co-writer copied this context into the document's
    /// editable context. Retain its source evidence without injecting it twice.
    #[serde(default)]
    pub context_in_document: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredContext {
    pub relative_path: String,
    pub sha256: String,
    pub text: String,
}

impl FrozenGenerationProfile {
    /// Explicit profile values override request defaults; the caller separately
    /// checks task and resident-model admission. Never silently clamp a request.
    pub fn resolve(&self, request: SamplingOverrides) -> Result<SamplingConfig, ConfigError> {
        self.validate()?;
        Ok(resolve_sampling(
            self.task,
            &[request, self.sampling.clone()],
        )?)
    }

    pub fn context_text(&self) -> &str {
        self.context
            .as_ref()
            .map_or("", |context| context.text.as_str())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != 1
            || (self.context_in_document && self.context.is_none())
            || self
                .config_sha256
                .as_ref()
                .is_some_and(|value| !is_digest(value))
            || self
                .profile_name
                .as_ref()
                .is_some_and(|name| !valid_profile_name(name))
            || self.profile_name.is_some() != self.profile_sha256.is_some()
        {
            return Err(ConfigError::Frozen);
        }
        if let Some(context) = &self.context {
            validate_context_path(&context.relative_path)?;
            if context.text.len() > MAX_CONTEXT_BYTES
                || digest(context.text.as_bytes()) != context.sha256
            {
                return Err(ConfigError::Frozen);
            }
        }
        if let Some(expected) = &self.profile_sha256 {
            let definition = NamedProfile {
                context_file: self
                    .context
                    .as_ref()
                    .map(|context| context.relative_path.clone()),
                sampling: self.sampling.clone(),
            };
            if digest(&serde_json::to_vec(&definition)?) != *expected {
                return Err(ConfigError::Frozen);
            }
        } else if self.context.is_some() {
            return Err(ConfigError::Frozen);
        }
        resolve_sampling(self.task, std::slice::from_ref(&self.sampling))?;
        Ok(())
    }
}

impl MineConfig {
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        if source.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge);
        }
        let mut config: Self = toml::from_str(source)?;
        config.validate()?;
        config.source_sha256 = Some(digest(source.as_bytes()));
        Ok(config)
    }

    /// A missing dotfile inherits defaults. This never creates directories or
    /// writes settings; invalid existing content stays available for repair.
    pub fn read(project_root: &Path) -> Result<Self, ConfigError> {
        match read_project_file(project_root, Path::new(CONFIG_FILE), MAX_CONFIG_BYTES) {
            Ok(source) => Self::parse(&source),
            Err(ConfigError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            Err(error) => Err(error),
        }
    }

    pub fn source_sha256(&self) -> Option<&str> {
        self.source_sha256.as_deref()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != 1 {
            return Err(ConfigError::Version(self.version));
        }
        self.workspace.resolve().map_err(ConfigError::Workspace)?;
        self.model.validate().map_err(ConfigError::Model)?;
        if self.profiles.len() > MAX_PROFILES {
            return Err(ConfigError::ProfileLimit);
        }
        for (name, profile) in &self.profiles {
            if !valid_profile_name(name) {
                return Err(ConfigError::ProfileName);
            }
            if let Some(path) = &profile.context_file {
                validate_context_path(path)?;
            }
            resolve_sampling(
                GenerationTask::ManualWriting,
                std::slice::from_ref(&profile.sampling),
            )?;
        }
        for task in [
            GenerationTask::Chat,
            GenerationTask::AutomaticProse,
            GenerationTask::AutomaticVerse,
            GenerationTask::ManualWriting,
        ] {
            if let Some(name) = self.generation.profile(task)
                && !self.profiles.contains_key(name)
            {
                return Err(ConfigError::MissingProfile(name.into()));
            }
        }
        Ok(())
    }

    pub fn freeze(
        &self,
        project_root: &Path,
        task: GenerationTask,
    ) -> Result<FrozenGenerationProfile, ConfigError> {
        self.freeze_named(project_root, task, self.generation.profile(task))
    }

    pub fn freeze_named(
        &self,
        project_root: &Path,
        task: GenerationTask,
        name: Option<&str>,
    ) -> Result<FrozenGenerationProfile, ConfigError> {
        self.validate()?;
        let profile = name
            .map(|name| {
                self.profiles
                    .get(name)
                    .ok_or_else(|| ConfigError::MissingProfile(name.into()))
            })
            .transpose()?;
        let context = profile
            .and_then(|profile| profile.context_file.as_ref())
            .map(|relative_path| {
                let text =
                    read_project_file(project_root, Path::new(relative_path), MAX_CONTEXT_BYTES)?;
                Ok::<_, ConfigError>(AuthoredContext {
                    relative_path: relative_path.clone(),
                    sha256: digest(text.as_bytes()),
                    text,
                })
            })
            .transpose()?;
        Ok(FrozenGenerationProfile {
            schema_version: 1,
            task,
            config_sha256: self.source_sha256.clone(),
            profile_name: name.map(str::to_owned),
            profile_sha256: profile
                .map(|profile| serde_json::to_vec(profile).map(|bytes| digest(&bytes)))
                .transpose()?,
            sampling: profile.map_or_else(SamplingOverrides::default, |profile| {
                profile.sampling.clone()
            }),
            context,
            context_in_document: false,
        })
    }
}

pub fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_context_path(value: &str) -> Result<(), ConfigError> {
    let components = value.split('/').collect::<Vec<_>>();
    if components.len() != 3 || components[0] != ".mine" || components[1] != "personas" {
        return Err(ConfigError::ContextPath);
    }
    let stem = components[2]
        .strip_suffix(".md")
        .ok_or(ConfigError::ContextPath)?;
    if !valid_profile_name(stem) {
        return Err(ConfigError::ContextPath);
    }
    Ok(())
}

fn read_project_file(root: &Path, relative: &Path, limit: usize) -> Result<String, ConfigError> {
    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(ConfigError::FileKind);
        };
        path.push(name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || (components.peek().is_some() && !metadata.is_dir())
            || (components.peek().is_none() && !metadata.is_file())
        {
            return Err(ConfigError::FileKind);
        }
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(ConfigError::FileKind);
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(ConfigError::SourceLimit(limit));
    }
    String::from_utf8(bytes).map_err(|_| ConfigError::Utf8)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_settings_do_not_write_and_invalid_settings_never_inherit() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            MineConfig::read(root.path())
                .unwrap()
                .source_sha256()
                .is_none()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        for source in [
            "version=2",
            "unknown=true",
            "[generation]\nmanual_writing='missing'",
            "[profiles.voice.sampling]\ntop_p=1.5",
            "[profiles.voice]\ncontext_file='../voice.md'",
            "[profiles.voice]\ntools=['shell']",
            "[profiles.voice.sampling]\nsampler_order=['temperature','bogus']",
        ] {
            fs::write(root.path().join(CONFIG_FILE), source).unwrap();
            assert!(MineConfig::read(root.path()).is_err(), "{source}");
            assert_eq!(
                fs::read_to_string(root.path().join(CONFIG_FILE)).unwrap(),
                source
            );
        }
    }

    #[test]
    fn profile_freezes_exact_context_separately_from_sampling_and_source_config() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".mine/personas")).unwrap();
        let context = "  Quiet voice.\r\n\n尾  ";
        fs::write(root.path().join(".mine/personas/voice.md"), context).unwrap();
        let config = MineConfig::parse("[generation]\nmanual_writing='voice'\n[profiles.voice]\ncontext_file='.mine/personas/voice.md'\n[profiles.voice.sampling]\ntemperature=0.25\ntop_k=7").unwrap();
        let frozen = config
            .freeze(root.path(), GenerationTask::ManualWriting)
            .unwrap();
        assert_eq!(frozen.context_text(), context);
        let sampling = frozen
            .resolve(SamplingOverrides {
                temperature: Some(0.8),
                seed: Some(42),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(sampling.temperature.to_bits(), 0.25_f32.to_bits());
        assert_eq!(sampling.seed, 42);
        assert_eq!(sampling.top_k, 7);
        fs::write(root.path().join(".mine/personas/voice.md"), "Edited later").unwrap();
        let newer = config
            .freeze(root.path(), GenerationTask::ManualWriting)
            .unwrap();
        assert_eq!(frozen.profile_sha256, newer.profile_sha256);
        assert_eq!(frozen.context_text(), context);
        assert_ne!(
            frozen.context.unwrap().sha256,
            newer.context.unwrap().sha256
        );
    }

    #[test]
    fn file_reads_are_bounded_and_reject_directory_and_symlink_sources() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(CONFIG_FILE),
            " ".repeat(MAX_CONFIG_BYTES + 1),
        )
        .unwrap();
        assert!(matches!(
            MineConfig::read(root.path()),
            Err(ConfigError::SourceLimit(_))
        ));
        fs::remove_file(root.path().join(CONFIG_FILE)).unwrap();
        fs::create_dir(root.path().join(CONFIG_FILE)).unwrap();
        assert!(matches!(
            MineConfig::read(root.path()),
            Err(ConfigError::FileKind)
        ));
        #[cfg(unix)]
        {
            fs::remove_dir(root.path().join(CONFIG_FILE)).unwrap();
            fs::write(root.path().join("source"), "").unwrap();
            std::os::unix::fs::symlink(root.path().join("source"), root.path().join(CONFIG_FILE))
                .unwrap();
            assert!(matches!(
                MineConfig::read(root.path()),
                Err(ConfigError::FileKind)
            ));
        }
    }

    #[test]
    fn serialized_profile_rejects_changed_definition_or_frozen_source_bytes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".mine/personas")).unwrap();
        fs::write(root.path().join(".mine/personas/voice.md"), "Exact.").unwrap();
        let config = MineConfig::parse(
            "[generation]\nchat='voice'\n[profiles.voice]\ncontext_file='.mine/personas/voice.md'",
        )
        .unwrap();
        let frozen = config.freeze(root.path(), GenerationTask::Chat).unwrap();
        let mut replay: FrozenGenerationProfile =
            serde_json::from_slice(&serde_json::to_vec(&frozen).unwrap()).unwrap();
        replay.validate().unwrap();
        replay.context.as_mut().unwrap().text.push('x');
        assert!(matches!(replay.validate(), Err(ConfigError::Frozen)));
        let mut replay = frozen;
        replay.sampling.temperature = Some(0.25);
        assert!(matches!(replay.validate(), Err(ConfigError::Frozen)));
    }
}
