#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use loom_types::BuildModelPolicy;

const POLICY_ENV: &str = "LOOM_BUILD_MODEL_POLICY";
const WRITER_MODEL_PATH_ENV: &str = "LOOM_BUILD_WRITER_MODEL_PATH";
const MACOS_DEPLOYMENT_TARGET_ENV: &str = "MACOSX_DEPLOYMENT_TARGET";
const CMAKE_MACOS_DEPLOYMENT_TARGET_ENV: &str = "CMAKE_OSX_DEPLOYMENT_TARGET";
const MINIMUM_MACOS_MAJOR: u32 = 10;
const MINIMUM_MACOS_MINOR: u32 = 15;
const DEFAULT_POLICY: &str = "writer-gemma4-base-v1";
const ALLOWED_POLICIES: [(&str, &str); 2] = [
    ("none-v1", "none-v1.json"),
    ("writer-gemma4-base-v1", "writer-gemma4-base-v1.json"),
];

fn main() {
    if let Err(error) = enforce_native_platform_floor() {
        panic!("invalid Loom native platform floor: {error}");
    }
    if let Err(error) = embed_build_model_policy() {
        panic!("failed to embed Loom build-model policy: {error}");
    }
    tauri_build::build();
}

fn enforce_native_platform_floor() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed={MACOS_DEPLOYMENT_TARGET_ENV}");
    println!("cargo:rerun-if-env-changed={CMAKE_MACOS_DEPLOYMENT_TARGET_ENV}");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return Ok(());
    }
    for variable in [
        MACOS_DEPLOYMENT_TARGET_ENV,
        CMAKE_MACOS_DEPLOYMENT_TARGET_ENV,
    ] {
        let value = env::var(variable)
            .map_err(|_| format!("{variable} must explicitly declare macOS 10.15 or newer"))?;
        let (major, minor) = parse_major_minor(&value)
            .ok_or_else(|| format!("{variable} has invalid version `{value}`"))?;
        if (major, minor) < (MINIMUM_MACOS_MAJOR, MINIMUM_MACOS_MINOR) {
            return Err(format!(
                "{variable}={value} is below the llama.cpp filesystem floor of macOS 10.15"
            ));
        }
    }
    Ok(())
}

fn parse_major_minor(value: &str) -> Option<(u32, u32)> {
    let mut parts = value.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

fn embed_build_model_policy() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed={POLICY_ENV}");
    println!("cargo:rerun-if-env-changed={WRITER_MODEL_PATH_ENV}");
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or_else(|| "Cargo did not provide CARGO_MANIFEST_DIR".to_owned())?,
    );
    let policy_dir = manifest_dir.join("../../../model-policies");
    let requested = match env::var(POLICY_ENV) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => DEFAULT_POLICY.to_owned(),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(format!("{POLICY_ENV} must be valid UTF-8"));
        }
    };
    let selected_file = ALLOWED_POLICIES
        .iter()
        .find_map(|(name, file)| (*name == requested).then_some(*file))
        .ok_or_else(|| {
            format!(
                "unsupported {POLICY_ENV} value `{requested}`; allowed values are none-v1 and writer-gemma4-base-v1"
            )
        })?;

    let mut selected = None;
    for (name, file) in ALLOWED_POLICIES {
        let path = policy_dir.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let policy = read_policy(&path)?;
        if policy.name().as_str() != name {
            return Err(format!(
                "policy file {} declares `{}` instead of `{name}`",
                path.display(),
                policy.name()
            ));
        }
        if file == selected_file {
            selected = Some(policy);
        }
    }
    let selected = selected.ok_or_else(|| "selected policy was not validated".to_owned())?;
    let canonical = selected
        .canonical_json()
        .map_err(|error| format!("could not canonicalize selected policy: {error}"))?;
    let digest = selected
        .canonical_digest()
        .map_err(|error| format!("could not digest selected policy: {error}"))?;
    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR").ok_or_else(|| "Cargo did not provide OUT_DIR".to_owned())?,
    );
    fs::write(out_dir.join("loom-build-model-policy.json"), canonical)
        .map_err(|error| format!("could not write canonical embedded policy: {error}"))?;
    let writer_model_path = embedded_writer_model_path()?;
    fs::write(
        out_dir.join("loom-build-writer-model-path.txt"),
        writer_model_path.as_deref().unwrap_or_default().as_bytes(),
    )
    .map_err(|error| format!("could not write embedded writer model path: {error}"))?;
    println!(
        "cargo:rustc-env=LOOM_BUILD_MODEL_POLICY_NAME={}",
        selected.name()
    );
    println!("cargo:rustc-env=LOOM_BUILD_MODEL_POLICY_SHA256={digest}");
    Ok(())
}

fn embedded_writer_model_path() -> Result<Option<String>, String> {
    let value = match env::var(WRITER_MODEL_PATH_ENV) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(format!("{WRITER_MODEL_PATH_ENV} must be valid UTF-8"));
        }
    };
    if !Path::new(&value).is_absolute() {
        return Err(format!("{WRITER_MODEL_PATH_ENV} must be an absolute path"));
    }
    Ok(Some(value))
}

fn read_policy(path: &Path) -> Result<BuildModelPolicy, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read policy {}: {error}", path.display()))?;
    BuildModelPolicy::from_json_slice(&bytes)
        .map_err(|error| format!("invalid policy {}: {error}", path.display()))
}
