//! Freeze project settings at explicit load admission. The registry and native
//! host retain ownership of loading, exact inspection, rollback, and teardown.

use std::path::PathBuf;

use loom_backend_llama::{LocalDevicePreference, LocalModelProfile, VerifiedModelDescriptor};
use loom_config::{MineConfig, ModelConfig};

use super::{IpcFailure, PluginState, lock_session};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ModelLoadSettings {
    root: PathBuf,
    model: ModelConfig,
}

impl ModelLoadSettings {
    pub(super) fn read(state: &PluginState) -> Result<Self, IpcFailure> {
        let root = lock_session(state)?
            .store
            .as_ref()
            .map(|store| store.root().to_path_buf());
        let Some(root) = root else {
            return Ok(Self {
                root: PathBuf::new(),
                model: ModelConfig::default(),
            });
        };
        let config =
            MineConfig::read(&root).map_err(|error| invalid_settings(error.to_string()))?;
        Ok(Self {
            root,
            model: config.model,
        })
    }

    pub(super) fn configured_path(&self) -> Option<PathBuf> {
        self.model.model_path(&self.root)
    }

    pub(super) fn snapshot(&self) -> Self {
        self.clone()
    }

    pub(super) fn matches(&self, previous: &Self) -> bool {
        self == previous
    }

    /// Keep a configured snapshot alias: its sibling projector can be named
    /// while the canonical GGUF target is an extensionless cache blob.
    pub(super) fn selected_path(&self, requested: &str) -> Result<PathBuf, IpcFailure> {
        let requested = PathBuf::from(requested);
        let Some(configured) = self.configured_path() else {
            return Ok(requested);
        };
        let configured_identity = configured.canonicalize().map_err(|error| {
            invalid_settings(format!("the configured model cannot be opened: {error}"))
        })?;
        let requested_identity = requested.canonicalize().map_err(|error| {
            invalid_settings(format!("the requested model cannot be opened: {error}"))
        })?;
        if requested_identity != configured_identity {
            return Err(invalid_settings(
                "the requested model differs from model.path in .mine.toml; edit the settings to choose another model",
            ));
        }
        Ok(configured)
    }

    pub(super) fn projector_path(&self) -> Result<Option<PathBuf>, IpcFailure> {
        self.model
            .projector_path(&self.root)
            .map(|path| {
                let canonical = path.canonicalize().map_err(|error| {
                    invalid_settings(format!(
                        "the configured projector cannot be opened: {error}"
                    ))
                })?;
                if !canonical.is_file() {
                    return Err(invalid_settings(
                        "model.projector_path must name a regular file",
                    ));
                }
                Ok(canonical)
            })
            .transpose()
    }

    pub(super) fn maximum_context_tokens(&self, model_maximum: Option<u32>) -> Option<u32> {
        match (self.model.context_tokens, model_maximum) {
            (Some(requested), Some(maximum)) => Some(requested.min(maximum)),
            (requested, maximum) => requested.or(maximum),
        }
    }

    pub(super) fn apply(&self, profile: &mut LocalModelProfile) -> Result<(), IpcFailure> {
        self.model.validate().map_err(invalid_settings)?;
        apply_digest_assertion(
            &mut profile.expected_model_sha256,
            self.model.expected_model_sha256.as_deref(),
            "model",
        )?;
        apply_digest_assertion(
            &mut profile.expected_mmproj_sha256,
            self.model.expected_projector_sha256.as_deref(),
            "projector",
        )?;
        if let Some(device) = self.model.device {
            profile.device = device;
            if device == LocalDevicePreference::Cpu {
                profile.gpu_layers = 0;
            }
        }
        if let Some(value) = self.model.context_tokens {
            profile.context_tokens = value;
        }
        if let Some(value) = self.model.batch_tokens {
            profile.batch_tokens = value;
        }
        if let Some(value) = self.model.max_sequences {
            profile.max_parallel_cases = value;
        }
        if let Some(value) = self.model.gpu_layers {
            profile.gpu_layers = value;
        }
        profile
            .as_native_config()
            .map_err(|error| invalid_settings(error.to_string()))?;
        Ok(())
    }
}

fn apply_digest_assertion(
    pinned: &mut Option<String>,
    requested: Option<&str>,
    name: &str,
) -> Result<(), IpcFailure> {
    if let Some(requested) = requested {
        if pinned
            .as_deref()
            .is_some_and(|pinned| !pinned.eq_ignore_ascii_case(requested))
        {
            return Err(invalid_settings(format!(
                "the configured {name} digest conflicts with the pinned catalog or policy identity"
            )));
        }
        if pinned.is_none() {
            *pinned = Some(requested.to_ascii_lowercase());
        }
    }
    Ok(())
}

fn invalid_settings(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("model_settings_invalid", message, false)
}

/// A digest-only change cannot stage the same worker and then retire it as
/// the previous resident. Check new assertions against its inspected bytes.
pub(super) fn validate_resident_assertions(
    requested: &LocalModelProfile,
    descriptor: &VerifiedModelDescriptor,
) -> Result<(), IpcFailure> {
    for (name, expected, observed) in [
        (
            "model",
            requested.expected_model_sha256.as_deref(),
            Some(descriptor.model_sha256.as_str()),
        ),
        (
            "projector",
            requested.expected_mmproj_sha256.as_deref(),
            descriptor.projector_sha256.as_deref(),
        ),
    ] {
        if let Some(expected) = expected
            && !observed.is_some_and(|observed| expected.eq_ignore_ascii_case(observed))
        {
            return Err(invalid_settings(format!(
                "the resident {name} does not match its configured digest"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn settings(root: &Path, source: &str) -> ModelLoadSettings {
        ModelLoadSettings {
            root: root.to_path_buf(),
            model: MineConfig::parse(source).expect("valid settings").model,
        }
    }

    #[test]
    fn configured_missing_or_different_model_never_falls_back() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let other = directory.path().join("other.gguf");
        std::fs::write(&other, b"GGUF").expect("other model fixture");
        let settings = settings(directory.path(), "[model]\npath = 'selected.gguf'");
        assert!(
            settings
                .selected_path(other.to_str().expect("UTF-8"))
                .is_err()
        );
        let selected = directory.path().join("selected.gguf");
        std::fs::write(&selected, b"GGUF").expect("selected model fixture");
        assert!(
            settings
                .selected_path(other.to_str().expect("UTF-8"))
                .is_err()
        );
        assert_eq!(
            settings
                .selected_path(selected.to_str().expect("UTF-8"))
                .expect("exact configured path"),
            selected
        );
    }

    #[test]
    fn runtime_settings_reach_native_configuration_and_preserve_pinned_identity() {
        let digest = "ab".repeat(32);
        let settings = settings(
            Path::new("/project"),
            &format!(
                "[model]\ndevice = 'cpu'\ncontext_tokens = 4096\nbatch_tokens = 128\nmax_sequences = 2\nexpected_model_sha256 = '{digest}'"
            ),
        );
        let mut profile = LocalModelProfile::for_gguf("fixture.gguf");
        settings.apply(&mut profile).expect("apply settings");
        let native = profile.as_native_config().expect("native configuration");
        assert_eq!(native.device, LocalDevicePreference::Cpu);
        assert_eq!(native.gpu_layers, 0);
        assert_eq!(
            (
                native.context_tokens,
                native.batch_tokens,
                native.max_sequences
            ),
            (4096, 128, 2)
        );
        assert_eq!(
            native.expected_model_sha256.as_deref(),
            Some(digest.as_str())
        );
        profile.expected_model_sha256 = Some("cd".repeat(32));
        let error = settings
            .apply(&mut profile)
            .expect_err("pinned identity wins");
        assert_eq!(error.code, "model_settings_invalid");
        assert_eq!(profile.expected_model_sha256, Some("cd".repeat(32)));
    }
}
