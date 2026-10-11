use std::path::{Path, PathBuf};

use llama_native_types::{MAX_PARALLEL_SEQUENCES, NativeDevice};
use serde::{Deserialize, Serialize};

/// Requested settings for the next explicit model load. Reading this value
/// never opens model assets, discovers siblings, or grants loading authority.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub path: Option<PathBuf>,
    pub projector_path: Option<PathBuf>,
    pub expected_model_sha256: Option<String>,
    pub expected_projector_sha256: Option<String>,
    pub device: Option<NativeDevice>,
    pub context_tokens: Option<u32>,
    pub batch_tokens: Option<u32>,
    pub max_sequences: Option<u32>,
    pub gpu_layers: Option<i32>,
}

impl ModelConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (name, path) in [
            ("path", self.path.as_deref()),
            ("projector_path", self.projector_path.as_deref()),
        ] {
            if let Some(path) = path {
                let text = path
                    .to_str()
                    .ok_or_else(|| format!("model.{name} must be UTF-8"))?;
                if text.is_empty() || text.len() > 4_096 || text.chars().any(char::is_control) {
                    return Err(format!(
                        "model.{name} must contain 1 to 4096 bytes without control characters"
                    ));
                }
            }
        }
        for (name, digest) in [
            (
                "expected_model_sha256",
                self.expected_model_sha256.as_deref(),
            ),
            (
                "expected_projector_sha256",
                self.expected_projector_sha256.as_deref(),
            ),
        ] {
            if let Some(digest) = digest
                && (digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(format!(
                    "model.{name} must contain exactly 64 hexadecimal digits"
                ));
            }
        }
        if self.expected_projector_sha256.is_some() && self.projector_path.is_none() {
            return Err("model.expected_projector_sha256 requires model.projector_path".to_owned());
        }
        if self.context_tokens.is_some_and(|value| value < 512) {
            return Err("model.context_tokens must be at least 512".to_owned());
        }
        if self.batch_tokens == Some(0) {
            return Err("model.batch_tokens must be positive".to_owned());
        }
        if self
            .max_sequences
            .is_some_and(|value| !(1..=MAX_PARALLEL_SEQUENCES).contains(&value))
        {
            return Err(format!(
                "model.max_sequences must be between 1 and {MAX_PARALLEL_SEQUENCES}"
            ));
        }
        if self.gpu_layers.is_some_and(|value| value < -1) {
            return Err("model.gpu_layers must be -1 (all layers) or nonnegative".to_owned());
        }
        match (self.device, self.gpu_layers) {
            (Some(NativeDevice::Cpu), Some(layers)) if layers != 0 => {
                return Err("model.device = cpu requires gpu_layers = 0 or omission".to_owned());
            }
            (Some(NativeDevice::Metal), Some(0)) => {
                return Err("model.device = metal conflicts with gpu_layers = 0".to_owned());
            }
            _ => {}
        }
        Ok(())
    }

    pub fn model_path(&self, project_root: &Path) -> Option<PathBuf> {
        self.path.as_ref().map(|path| project_root.join(path))
    }

    pub fn projector_path(&self, project_root: &Path) -> Option<PathBuf> {
        self.projector_path
            .as_ref()
            .map(|path| project_root.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unapplied_keys_and_contradictory_hardware_requests() {
        assert!(toml::from_str::<ModelConfig>("threads = 8").is_err());
        for source in [
            "context_tokens = 511",
            "batch_tokens = 0",
            "max_sequences = 5",
            "gpu_layers = -2",
            "device = 'cpu'\ngpu_layers = -1",
            "device = 'metal'\ngpu_layers = 0",
            "path = ''",
            "expected_model_sha256 = 'not-a-digest'",
        ] {
            let config: ModelConfig = toml::from_str(source).expect("typed settings");
            assert!(config.validate().is_err(), "{source}");
        }
    }

    #[test]
    fn path_resolution_is_lexical_and_requires_no_existing_asset() {
        let config: ModelConfig = toml::from_str("path = '../models/writer.gguf'\nprojector_path = '/models/projector.gguf'\ndevice = 'cpu'").expect("typed settings");
        config
            .validate()
            .expect("CPU defaults to zero offloaded layers");
        assert_eq!(
            config.model_path(Path::new("/project")),
            Some(PathBuf::from("/project/../models/writer.gguf"))
        );
        assert_eq!(
            config.projector_path(Path::new("/project")),
            Some(PathBuf::from("/models/projector.gguf"))
        );
    }
}
