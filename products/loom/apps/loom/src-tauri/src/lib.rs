#![forbid(unsafe_code)]

use loom_types::BuildModelPolicy;
use std::path::PathBuf;

const EMBEDDED_BUILD_MODEL_POLICY: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/loom-build-model-policy.json"));
const EMBEDDED_BUILD_MODEL_POLICY_NAME: &str = env!("LOOM_BUILD_MODEL_POLICY_NAME");
const EMBEDDED_BUILD_MODEL_POLICY_SHA256: &str = env!("LOOM_BUILD_MODEL_POLICY_SHA256");
const EMBEDDED_BUILD_WRITER_MODEL_PATH: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/loom-build-writer-model-path.txt"
));

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let Ok(build_model_policy) = embedded_build_model_policy() else {
        eprintln!("Loom's embedded model policy failed its integrity check");
        return;
    };
    let Ok(build_writer_model_path) = embedded_build_writer_model_path() else {
        eprintln!("Loom's embedded writer model path failed its integrity check");
        return;
    };
    let mut loom_plugin =
        tauri_plugin_loom::Builder::new().with_build_model_policy(build_model_policy);
    if let Some(model_path) = build_writer_model_path {
        loom_plugin = loom_plugin.with_additional_policy_model_path(model_path);
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(loom_plugin.build())
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| eprintln!("Loom could not start: {error}"));
}

fn embedded_build_writer_model_path() -> Result<Option<PathBuf>, String> {
    if EMBEDDED_BUILD_WRITER_MODEL_PATH.is_empty() {
        return Ok(None);
    }
    let value =
        std::str::from_utf8(EMBEDDED_BUILD_WRITER_MODEL_PATH).map_err(|error| error.to_string())?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("embedded writer model path is not absolute".to_owned());
    }
    Ok(Some(path))
}

fn embedded_build_model_policy() -> Result<BuildModelPolicy, String> {
    let policy = BuildModelPolicy::from_json_slice(EMBEDDED_BUILD_MODEL_POLICY)
        .map_err(|error| error.to_string())?;
    if policy.name().as_str() != EMBEDDED_BUILD_MODEL_POLICY_NAME {
        return Err("embedded policy name does not match its build identity".to_owned());
    }
    let digest = policy
        .canonical_digest()
        .map_err(|error| error.to_string())?;
    if digest.to_string() != EMBEDDED_BUILD_MODEL_POLICY_SHA256 {
        return Err("embedded policy digest does not match its build identity".to_owned());
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_policy_is_canonical_and_bound_to_its_build_identity() {
        let policy = embedded_build_model_policy().expect("valid embedded model policy");
        assert_eq!(policy.name().as_str(), EMBEDDED_BUILD_MODEL_POLICY_NAME);
        assert_eq!(
            policy.canonical_json().expect("canonical policy"),
            EMBEDDED_BUILD_MODEL_POLICY
        );
    }

    #[test]
    fn optional_embedded_writer_path_is_absent_or_absolute() {
        assert!(
            embedded_build_writer_model_path()
                .expect("valid embedded writer model path")
                .is_none_or(|path| path.is_absolute())
        );
    }
}
