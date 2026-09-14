use std::collections::BTreeMap;

use llama_native_types::{NativeError, SamplerKind, SamplingConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Task defaults, never a prompt/chat-template choice or a model-quality claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationTask {
    Chat,
    AutomaticProse,
    AutomaticVerse,
    Loompad,
    ManualWriting,
}

impl GenerationTask {
    #[must_use]
    pub fn defaults(self) -> SamplingConfig {
        let mut sampling = SamplingConfig::default();
        match self {
            Self::Chat => sampling.max_tokens = 512,
            Self::AutomaticProse | Self::Loompad => {
                sampling.min_p = 0.05;
                sampling.dry_allowed_length = 4;
                sampling.dry_penalty_last_n = 256;
            }
            Self::AutomaticVerse | Self::ManualWriting => {}
        }
        // Both applications keep prompt-history penalties neutral. In a
        // rendered chat prompt, DRY also sees instructions and control tokens;
        // enabling it globally previously distorted Loom's first prose token.
        sampling
    }
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("unknown sampler `{0}`")]
    UnknownSampler(String),
    #[error("sampler `{0}` appears more than once")]
    DuplicateSampler(String),
    #[error("invalid sampling settings: {0}")]
    InvalidSettings(String),
    #[error(transparent)]
    InvalidSampling(#[from] NativeError),
}

/// Empty text means the native default order. Unknown names and duplicates
/// fail the whole request; a partially applied chain is never substituted.
pub fn parse_sampler_order(raw: &str) -> Result<Vec<SamplerKind>, ProfileError> {
    if raw.trim().is_empty() {
        return Ok(SamplingConfig::default().sampler_order);
    }
    let mut result = Vec::new();
    for name in raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        if name.is_empty() {
            continue;
        }
        let kind = match name.to_ascii_lowercase().as_str() {
            "penalties" => SamplerKind::Penalties,
            "dry" => SamplerKind::Dry,
            "top_k" | "top-k" => SamplerKind::TopK,
            "typ_p" | "typical" | "typical_p" => SamplerKind::TypicalP,
            "top_p" | "top-p" => SamplerKind::TopP,
            "min_p" | "min-p" => SamplerKind::MinP,
            "xtc" => SamplerKind::Xtc,
            "temperature" | "temp" => SamplerKind::Temperature,
            _ => return Err(ProfileError::UnknownSampler(name.to_owned())),
        };
        if result.contains(&kind) {
            return Err(ProfileError::DuplicateSampler(name.to_owned()));
        }
        result.push(kind);
    }
    if result.is_empty() {
        return Err(ProfileError::InvalidSettings(
            "sampler order has no names".into(),
        ));
    }
    Ok(result)
}

macro_rules! sampling_overrides {
    ($($(#[$meta:meta])* $field:ident: $type:ty),* $(,)?) => {
        /// A partial request. `None` inherits; supplied values are never clamped.
        #[derive(Clone, Debug, Default, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct SamplingOverrides {
            $($(#[$meta])* pub $field: Option<$type>,)*
        }

        impl SamplingOverrides {
            fn apply(&self, sampling: &mut SamplingConfig) {
                $(if let Some(value) = &self.$field {
                    sampling.$field.clone_from(value);
                })*
            }
        }
    };
}

sampling_overrides! {
    seed: u32,
    temperature: f32,
    #[serde(alias = "dynatemp_range")]
    dynamic_temperature_range: f32,
    #[serde(alias = "dynatemp_exponent")]
    dynamic_temperature_exponent: f32,
    top_k: i32,
    top_p: f32,
    min_p: f32,
    #[serde(alias = "typ_p")]
    typical_p: f32,
    xtc_probability: f32,
    xtc_threshold: f32,
    repeat_last_n: i32,
    repeat_penalty: f32,
    frequency_penalty: f32,
    presence_penalty: f32,
    dry_multiplier: f32,
    dry_base: f32,
    dry_allowed_length: i32,
    dry_penalty_last_n: i32,
    sampler_order: Vec<SamplerKind>,
    max_tokens: u32,
    stop: Vec<String>,
}

/// Public UI names accepted by the inherited settings adapter. Product
/// allowlists remain narrower and own which controls are writable.
pub const SAMPLING_SETTING_KEYS: &[&str] = &[
    "seed",
    "temperature",
    "dynatemp_range",
    "dynatemp_exponent",
    "top_k",
    "top_p",
    "min_p",
    "typ_p",
    "xtc_probability",
    "xtc_threshold",
    "repeat_last_n",
    "repeat_penalty",
    "frequency_penalty",
    "presence_penalty",
    "dry_multiplier",
    "dry_base",
    "dry_allowed_length",
    "dry_penalty_last_n",
    "samplers",
    "max_tokens",
];

impl SamplingOverrides {
    /// Parse only the sampling portion of an application's mixed settings map.
    /// Numeric text is accepted for HTML inputs; malformed supplied values are
    /// errors rather than a request to inherit a default.
    pub fn from_settings(values: &BTreeMap<String, Value>) -> Result<Self, ProfileError> {
        let mut sampling = serde_json::Map::new();
        for key in SAMPLING_SETTING_KEYS {
            let Some(value) = values.get(*key).filter(|value| !value.is_null()) else {
                continue;
            };
            if *key == "samplers" {
                let raw = value
                    .as_str()
                    .ok_or_else(|| ProfileError::InvalidSettings("samplers must be text".into()))?;
                sampling.insert(
                    "sampler_order".into(),
                    serde_json::to_value(parse_sampler_order(raw)?)
                        .map_err(|error| ProfileError::InvalidSettings(error.to_string()))?,
                );
            } else {
                let value = if let Some(raw) = value.as_str() {
                    serde_json::from_str(raw.trim()).map_err(|_| {
                        ProfileError::InvalidSettings(format!("{key} must contain a number"))
                    })?
                } else {
                    value.clone()
                };
                if !value.is_number() {
                    return Err(ProfileError::InvalidSettings(format!(
                        "{key} must contain a number"
                    )));
                }
                sampling.insert((*key).into(), value);
            }
        }
        serde_json::from_value(Value::Object(sampling))
            .map_err(|error| ProfileError::InvalidSettings(error.to_string()))
    }

    /// Strict typed override object. The product must additionally authorize
    /// keys before calling this; this parser cannot grant new control authority.
    pub fn from_json(raw: &str) -> Result<Self, ProfileError> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(raw).map_err(|error| ProfileError::InvalidSettings(error.to_string()))
    }
}

/// Apply layers in order and validate each layer, including the baseline.
/// An invalid value cannot be hidden by a later override. This resolves the
/// requested configuration; native receipts remain the applied-runtime authority.
pub fn resolve_sampling(
    task: GenerationTask,
    layers: &[SamplingOverrides],
) -> Result<SamplingConfig, ProfileError> {
    let mut sampling = task.defaults();
    sampling.validate()?;
    for layer in layers {
        layer.apply(&mut sampling);
        sampling.validate()?;
    }
    Ok(sampling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_or_duplicate_stage_rejects_entire_order() {
        assert!(matches!(
            parse_sampler_order("top_k;typo;temperature"),
            Err(ProfileError::UnknownSampler(_))
        ));
        assert!(matches!(
            parse_sampler_order("top_p top-p"),
            Err(ProfileError::DuplicateSampler(_))
        ));
        assert!(parse_sampler_order(" , ; ").is_err());
        assert_eq!(
            parse_sampler_order("TOP-K\ntemp").expect("aliases"),
            vec![SamplerKind::TopK, SamplerKind::Temperature]
        );
    }

    #[test]
    fn supplied_bad_values_never_fall_back_or_hide_behind_an_override() {
        for value in [
            json!("NaN"),
            json!("null"),
            json!("1e100"),
            json!(-1),
            json!(1.5),
        ] {
            let parsed =
                SamplingOverrides::from_settings(&BTreeMap::from([("max_tokens".into(), value)]));
            assert!(parsed.is_err());
        }
        let invalid = SamplingOverrides {
            top_p: Some(1.5),
            ..Default::default()
        };
        let valid = SamplingOverrides {
            top_p: Some(0.8),
            ..Default::default()
        };
        assert!(resolve_sampling(GenerationTask::Chat, &[invalid, valid]).is_err());
    }

    #[test]
    fn task_variation_is_explicit_and_inherits_neutral_penalties() {
        for task in [
            GenerationTask::Chat,
            GenerationTask::AutomaticProse,
            GenerationTask::AutomaticVerse,
            GenerationTask::Loompad,
            GenerationTask::ManualWriting,
        ] {
            let sampling = resolve_sampling(task, &[]).expect("task defaults");
            assert_eq!(
                sampling.min_p,
                if matches!(
                    task,
                    GenerationTask::AutomaticProse | GenerationTask::Loompad
                ) {
                    0.05
                } else {
                    0.0
                }
            );
            assert_eq!(sampling.dry_multiplier, 0.0);
            assert_eq!(sampling.repeat_penalty, 1.0);
        }
    }

    #[test]
    fn loompad_retains_main_prose_sampling_with_its_128_token_default() {
        let sampling = resolve_sampling(GenerationTask::Loompad, &[]).expect("Loompad defaults");
        assert_eq!(sampling.max_tokens, 128);
        assert_eq!(sampling.min_p, 0.05);
        assert_eq!(sampling.dry_allowed_length, 4);
        assert_eq!(sampling.dry_penalty_last_n, 256);
        assert_eq!(
            sampling.fingerprint(),
            GenerationTask::AutomaticProse.defaults().fingerprint()
        );
    }

    #[test]
    fn custom_layer_has_last_precedence_and_rejects_unknown_fields() {
        let inherited = SamplingOverrides::from_settings(&BTreeMap::from([
            ("temperature".into(), json!("0.8")),
            ("theme".into(), json!("dark")),
        ]))
        .expect("UI input");
        let custom =
            SamplingOverrides::from_json(r#"{"temperature":0.25,"top_k":7,"max_tokens":33}"#)
                .expect("custom");
        let sampling =
            resolve_sampling(GenerationTask::Chat, &[inherited, custom]).expect("resolved");
        assert_eq!(sampling.temperature, 0.25);
        assert_eq!(sampling.max_tokens, 33);
        assert!(SamplingOverrides::from_json(r#"{"shell":"no"}"#).is_err());
    }
}
