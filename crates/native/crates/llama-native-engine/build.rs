#![forbid(unsafe_code)]

#[path = "src/build_identity.rs"]
mod build_identity;

use build_identity::{
    PrivateBuildIdentity, reviewed_binding_profile, rustc_digest_directive,
    validate_binding_build_evidence, validate_reviewed_cmake_cache,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LLAMA_CPP_BINDING_VERSION: &str = "0.1.154";
const LLAMA_CPP_BINDING_REV: &str = "01e48b7c1e7de39c3e5e8a67cd9efac498f8da1f";
const LLAMA_CPP_REV: &str = "5f55650a78f92aff4d48d671423e888fac0469ff";

// Fixed names only. Values are fed to a hash-only accumulator and are never
// serialized into build output, constants, binaries, or receipts.
const ALLOWLISTED_ENVIRONMENT: &[(&str, &str)] = &[
    ("BUILD_DEBUG", "environment.build_debug"),
    (
        "CARGO_ENCODED_RUSTFLAGS",
        "environment.cargo_encoded_rustflags",
    ),
    (
        "CARGO_CFG_TARGET_FEATURE",
        "environment.cargo_cfg_target_feature",
    ),
    ("CARGO_CFG_PANIC", "environment.cargo_cfg_panic"),
    ("CMAKE", "environment.cmake"),
    ("CMAKE_BUILD_TYPE", "environment.cmake_build_type"),
    ("CMAKE_GENERATOR", "environment.cmake_generator"),
    ("CMAKE_MAKE_PROGRAM", "environment.cmake_make_program"),
    (
        "CMAKE_OSX_ARCHITECTURES",
        "environment.cmake_osx_architectures",
    ),
    (
        "CMAKE_OSX_DEPLOYMENT_TARGET",
        "environment.cmake_osx_deployment_target",
    ),
    ("CMAKE_OSX_SYSROOT", "environment.cmake_osx_sysroot"),
    ("CMAKE_PREFIX_PATH", "environment.cmake_prefix_path"),
    ("CMAKE_TOOLCHAIN_FILE", "environment.cmake_toolchain_file"),
    ("DEBUG", "environment.debug"),
    ("GGML_CPU_REPACK", "environment.ggml_cpu_repack"),
    ("GGML_NATIVE", "environment.ggml_native"),
    ("LLAMA_LIB_PROFILE", "environment.llama_lib_profile"),
    (
        "LLAMA_BUILD_SHARED_LIBS",
        "environment.llama_build_shared_libs",
    ),
    ("LLAMA_STATIC_CRT", "environment.llama_static_crt"),
    (
        "MACOSX_DEPLOYMENT_TARGET",
        "environment.macosx_deployment_target",
    ),
    (
        "IPHONEOS_DEPLOYMENT_TARGET",
        "environment.iphoneos_deployment_target",
    ),
    (
        "TVOS_DEPLOYMENT_TARGET",
        "environment.tvos_deployment_target",
    ),
    (
        "WATCHOS_DEPLOYMENT_TARGET",
        "environment.watchos_deployment_target",
    ),
    (
        "VISIONOS_DEPLOYMENT_TARGET",
        "environment.visionos_deployment_target",
    ),
    ("ANDROID_API_LEVEL", "environment.android_api_level"),
    ("ANDROID_PLATFORM", "environment.android_platform"),
    ("ANDROID_NDK", "environment.android_ndk"),
    ("ANDROID_NDK_ROOT", "environment.android_ndk_root"),
    ("NDK_ROOT", "environment.ndk_root"),
    ("ANDROID_HOME", "environment.android_home"),
    ("ANDROID_SDK_ROOT", "environment.android_sdk_root"),
    ("SDKROOT", "environment.sdkroot"),
    ("DEVELOPER_DIR", "environment.developer_dir"),
    ("VULKAN_SDK", "environment.vulkan_sdk"),
    ("CUDA_PATH", "environment.cuda_path"),
    ("CUDA_HOME", "environment.cuda_home"),
    ("ROCM_PATH", "environment.rocm_path"),
    ("HIP_PATH", "environment.hip_path"),
    ("OPENCL_INCLUDE_DIR", "environment.opencl_include_dir"),
    ("OPENCL_LIBRARY", "environment.opencl_library"),
    ("MKLROOT", "environment.mklroot"),
    ("RUSTC_BOOTSTRAP", "environment.rustc_bootstrap"),
    ("RUSTFLAGS", "environment.rustflags"),
    ("SOURCE_DATE_EPOCH", "environment.source_date_epoch"),
];

// These affect only scheduling or diagnostics. They are explicitly accepted,
// not fingerprinted, and never broadened to arbitrary CMAKE_* names.
const ALLOWLISTED_NONSEMANTIC_ENVIRONMENT: &[&str] =
    &["CMAKE_BUILD_PARALLEL_LEVEL", "CMAKE_VERBOSE"];

const COMPILER_ENVIRONMENT_BASES: &[(&str, &str)] = &[
    ("AR", "environment.archiver"),
    ("CC", "environment.c_compiler"),
    ("CXX", "environment.cxx_compiler"),
    ("CFLAGS", "environment.cflags"),
    ("CXXFLAGS", "environment.cxxflags"),
    ("CPPFLAGS", "environment.cppflags"),
    ("LDFLAGS", "environment.ldflags"),
];

fn main() {
    if let Err(error) = emit_build_identity() {
        panic!("failed to derive private native build identity: {error}");
    }
}

fn emit_build_identity() -> Result<(), String> {
    reject_unreviewed_forwarded_environment()?;

    let engine_root = PathBuf::from(required_os("CARGO_MANIFEST_DIR")?);
    let workspace_root = engine_root.join("../..");
    let types_root = engine_root.join("../llama-native-types");
    let target = required_safe_identifier("TARGET")?;
    let host = required_safe_identifier("HOST")?;

    let mut identity = PrivateBuildIdentity::new();
    identity.add_bytes("protocol", b"llama-native-engine-private-build-v3")?;
    identity.add_bytes("binding.version", LLAMA_CPP_BINDING_VERSION.as_bytes())?;
    identity.add_bytes("binding.wrapper_revision", LLAMA_CPP_BINDING_REV.as_bytes())?;
    identity.add_bytes("binding.llama_cpp_revision", LLAMA_CPP_REV.as_bytes())?;
    identity.add_bytes(
        "source.engine",
        &source_tree_digest(&engine_root, &["Cargo.toml", "build.rs", "src"])?,
    )?;
    identity.add_bytes(
        "source.types",
        &source_tree_digest(&types_root, &["Cargo.toml", "src"])?,
    )?;
    identity.add_bytes(
        "source.workspace_manifest",
        &private_file_digest(&workspace_root.join("Cargo.toml"), "workspace manifest")?,
    )?;
    identity.add_bytes(
        "source.cargo_lock",
        &private_file_digest(&workspace_root.join("Cargo.lock"), "Cargo lock")?,
    )?;
    identity.add_bytes(
        "source.cargo_config",
        &private_file_digest(
            &workspace_root.join(".cargo/config.toml"),
            "Cargo configuration",
        )?,
    )?;
    identity.add_bytes("cargo.target", target.as_bytes())?;
    identity.add_bytes("cargo.host", host.as_bytes())?;
    identity.add_bytes(
        "cargo.profile",
        required_safe_identifier("PROFILE")?.as_bytes(),
    )?;
    identity.add_bytes(
        "cargo.opt_level",
        required_safe_identifier("OPT_LEVEL")?.as_bytes(),
    )?;

    for (name, field) in ALLOWLISTED_ENVIRONMENT {
        println!("cargo:rerun-if-env-changed={name}");
        identity.add_optional_os(field, env::var_os(name).as_deref())?;
    }
    for name in ALLOWLISTED_NONSEMANTIC_ENVIRONMENT {
        println!("cargo:rerun-if-env-changed={name}");
    }
    for (base, field) in COMPILER_ENVIRONMENT_BASES {
        add_target_environment(&mut identity, field, base, &target, &host)?;
    }
    add_rustc_identity(&mut identity)?;

    let dependency_root = PathBuf::from(required_os("DEP_LLAMA_ROOT")?);
    validate_reviewed_cmake_cache(&private_read(
        &dependency_root.join("build/CMakeCache.txt"),
        "llama.cpp CMake cache",
    )?)?;
    identity.add_bytes(
        "dependency.upstream_build_evidence",
        binding_build_evidence_digest(&target)?.as_bytes(),
    )?;

    for path in [
        "Cargo.toml",
        "build.rs",
        "src",
        "../llama-native-types/Cargo.toml",
        "../llama-native-types/src",
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../../.cargo/config.toml",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=DEP_LLAMA_ROOT");
    println!("{}", rustc_digest_directive(&identity.finish_hex())?);
    Ok(())
}

fn add_rustc_identity(identity: &mut PrivateBuildIdentity) -> Result<(), String> {
    // Cargo directives disclose only fixed variable names. PATH/PATHEXT select
    // a compiler when RUSTC is not absolute, but their values are deliberately
    // absent from the identity; the resolved compiler bytes are what matter.
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=PATHEXT");
    println!("cargo:rerun-if-env-changed=RUSTC");
    let configured = required_os("RUSTC")?;
    let resolved = resolve_executable(&configured)?;
    identity.add_bytes(
        "tool.rustc_binary",
        &private_file_digest(&resolved, "Rust compiler binary")?,
    )?;
    let output = Command::new(&resolved)
        .args(["--version", "--verbose"])
        .output()
        .map_err(|_| "failed to invoke the Rust compiler identity command".to_string())?;
    if !output.status.success() {
        return Err("Rust compiler identity command failed".to_string());
    }
    let mut digest = Sha256::new();
    digest.update(b"llama-native-rustc-version-output-v1\0");
    hash_frame(&mut digest, &output.stdout);
    hash_frame(&mut digest, &output.stderr);
    hash_frame(&mut digest, output.status.to_string().as_bytes());
    identity.add_bytes("tool.rustc_version", &digest.finalize())?;
    add_optional_tool_identity(identity, "tool.rustc_wrapper", "RUSTC_WRAPPER")?;
    add_optional_tool_identity(
        identity,
        "tool.rustc_workspace_wrapper",
        "RUSTC_WORKSPACE_WRAPPER",
    )?;
    Ok(())
}

fn add_optional_tool_identity(
    identity: &mut PrivateBuildIdentity,
    field: &str,
    environment_name: &str,
) -> Result<(), String> {
    println!("cargo:rerun-if-env-changed={environment_name}");
    let Some(configured) = env::var_os(environment_name) else {
        return identity.add_bytes(field, b"absent");
    };
    let resolved = resolve_executable(&configured)?;
    identity.add_bytes(
        field,
        &private_file_digest(&resolved, "Rust compiler wrapper")?,
    )
}

fn resolve_executable(configured: &OsStr) -> Result<PathBuf, String> {
    let configured_path = Path::new(configured);
    if configured_path.is_absolute() || configured_path.components().count() > 1 {
        return canonical_regular_file(configured_path);
    }
    let search_path = env::var_os("PATH")
        .ok_or_else(|| "cannot resolve a configured Rust build tool".to_string())?;
    for directory in env::split_paths(&search_path) {
        let candidate = directory.join(configured_path);
        if candidate.is_file() {
            return canonical_regular_file(&candidate);
        }
        #[cfg(windows)]
        if candidate.extension().is_none() {
            let extensions =
                env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
            for extension in extensions.to_string_lossy().split(';') {
                let candidate = directory.join(format!(
                    "{}{}",
                    configured_path.to_string_lossy(),
                    extension
                ));
                if candidate.is_file() {
                    return canonical_regular_file(&candidate);
                }
            }
        }
    }
    Err("cannot resolve a configured Rust build tool".to_string())
}

fn canonical_regular_file(path: &Path) -> Result<PathBuf, String> {
    let path = path
        .canonicalize()
        .map_err(|_| "cannot resolve a configured Rust build tool".to_string())?;
    if !path.is_file() {
        return Err("configured Rust build tool is not a regular file".to_string());
    }
    Ok(path)
}

fn reject_unreviewed_forwarded_environment() -> Result<(), String> {
    for (name, _) in env::vars_os() {
        let Some(name) = name.to_str() else {
            continue;
        };
        let forwarded = name.starts_with("CMAKE_") || name.starts_with("GGML_");
        if forwarded
            && !ALLOWLISTED_ENVIRONMENT
                .iter()
                .any(|(allowed, _)| *allowed == name)
            && !ALLOWLISTED_NONSEMANTIC_ENVIRONMENT.contains(&name)
        {
            return Err(
                "an unreviewed CMAKE_* or GGML_* environment input is set; exact native builds accept only the fixed allowlist"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn add_target_environment(
    identity: &mut PrivateBuildIdentity,
    field: &str,
    base: &str,
    target: &str,
    host: &str,
) -> Result<(), String> {
    let kind = if target == host { "HOST" } else { "TARGET" };
    let underscored = target.replace(['-', '.'], "_");
    let mut names = BTreeSet::new();
    names.insert(base.to_string());
    names.insert(format!("{base}_{target}"));
    names.insert(format!("{base}_{underscored}"));
    names.insert(format!("{kind}_{base}"));
    for (index, name) in names.into_iter().enumerate() {
        println!("cargo:rerun-if-env-changed={name}");
        identity.add_optional_os(&format!("{field}.{index}"), env::var_os(name).as_deref())?;
    }
    Ok(())
}

fn binding_build_evidence_digest(target: &str) -> Result<String, String> {
    reject_extra_binding_evidence_environment()?;
    let format = required_private_utf8("DEP_LLAMA_BUILD_EVIDENCE_FORMAT")?;
    let features = required_private_utf8("DEP_LLAMA_BUILD_EVIDENCE_FEATURES")?;
    let linkage = required_private_utf8("DEP_LLAMA_BUILD_EVIDENCE_LINKAGE")?;
    let count_text = required_private_utf8("DEP_LLAMA_BUILD_EVIDENCE_ARTIFACT_COUNT")?;
    let count = count_text
        .parse::<usize>()
        .ok()
        .filter(|count| (1..=64).contains(count) && count.to_string() == count_text)
        .ok_or_else(|| "binding build evidence artifact count is malformed".to_string())?;
    let reported_sha256 = required_private_utf8("DEP_LLAMA_BUILD_EVIDENCE_SHA256")?;

    for name in [
        "DEP_LLAMA_BUILD_EVIDENCE_FORMAT",
        "DEP_LLAMA_BUILD_EVIDENCE_FEATURES",
        "DEP_LLAMA_BUILD_EVIDENCE_LINKAGE",
        "DEP_LLAMA_BUILD_EVIDENCE_ARTIFACT_COUNT",
        "DEP_LLAMA_BUILD_EVIDENCE_SHA256",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let mut artifacts = Vec::with_capacity(count);
    for index in 0..64 {
        let name = format!("DEP_LLAMA_BUILD_EVIDENCE_ARTIFACT_{index:02}");
        println!("cargo:rerun-if-env-changed={name}");
        match (index < count, env::var_os(&name)) {
            (true, Some(value)) => {
                artifacts.push(value.into_string().map_err(|_| {
                    "binding build evidence artifact entry is not UTF-8".to_string()
                })?)
            }
            (true, None) => {
                return Err("binding build evidence omits a declared artifact".to_string());
            }
            (false, Some(_)) => {
                return Err("binding build evidence contains an undeclared artifact".to_string());
            }
            (false, None) => {}
        }
    }

    let (expected_features, required_artifact_groups) = reviewed_binding_profile(target);
    validate_binding_build_evidence(
        &format,
        &features,
        &linkage,
        &artifacts,
        &reported_sha256,
        expected_features,
        required_artifact_groups,
    )
}

fn reject_extra_binding_evidence_environment() -> Result<(), String> {
    const PREFIX: &str = "DEP_LLAMA_BUILD_EVIDENCE_";
    let mut allowed = [
        "DEP_LLAMA_BUILD_EVIDENCE_FORMAT".to_string(),
        "DEP_LLAMA_BUILD_EVIDENCE_FEATURES".to_string(),
        "DEP_LLAMA_BUILD_EVIDENCE_LINKAGE".to_string(),
        "DEP_LLAMA_BUILD_EVIDENCE_ARTIFACT_COUNT".to_string(),
        "DEP_LLAMA_BUILD_EVIDENCE_SHA256".to_string(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    allowed.extend((0..64).map(|index| format!("DEP_LLAMA_BUILD_EVIDENCE_ARTIFACT_{index:02}")));
    if env::vars_os().any(|(name, _)| {
        name.to_str()
            .is_some_and(|name| name.starts_with(PREFIX) && !allowed.contains(name))
    }) {
        return Err("binding build evidence contains an unknown field".to_string());
    }
    Ok(())
}

fn required_os(name: &str) -> Result<OsString, String> {
    env::var_os(name).ok_or_else(|| "required private build input is unavailable".to_string())
}

fn required_private_utf8(name: &str) -> Result<String, String> {
    required_os(name)?
        .into_string()
        .map_err(|_| "required private build evidence is not UTF-8".to_string())
}

fn required_safe_identifier(name: &str) -> Result<String, String> {
    let value = env::var(name)
        .map_err(|_| "required structural build input is unavailable or invalid".to_string())?;
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
    {
        return Err("required structural build input is not a safe identifier".to_string());
    }
    Ok(value)
}

fn source_tree_digest(root: &Path, entries: &[&str]) -> Result<Vec<u8>, String> {
    let mut files = Vec::<(String, PathBuf)>::new();
    for entry in entries {
        collect_source_files(root, &root.join(entry), &mut files)?;
    }
    files.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let mut digest = Sha256::new();
    digest.update(b"llama-native-source-tree-v2\0");
    for (relative, path) in files {
        hash_frame(&mut digest, relative.as_bytes());
        hash_frame(&mut digest, &private_read(&path, "reviewed source file")?);
    }
    Ok(digest.finalize().to_vec())
}

fn collect_source_files(
    root: &Path,
    path: &Path,
    output: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "failed to inspect reviewed source tree".to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("reviewed source tree contains an unsupported symlink".to_string());
    }
    if metadata.is_file() {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "failed to relativize reviewed source".to_string())?
            .to_str()
            .ok_or_else(|| "reviewed source name is not valid UTF-8".to_string())?
            .replace('\\', "/");
        output.push((relative, path.to_path_buf()));
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err("reviewed source tree contains an unsupported entry".to_string());
    }
    let mut children = fs::read_dir(path)
        .map_err(|_| "failed to enumerate reviewed source tree".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "failed to enumerate reviewed source tree".to_string())?;
    children.sort_unstable_by_key(|entry| entry.file_name());
    for child in children {
        collect_source_files(root, &child.path(), output)?;
    }
    Ok(())
}

fn private_file_digest(path: &Path, label: &str) -> Result<Vec<u8>, String> {
    Ok(Sha256::digest(private_read(path, label)?).to_vec())
}

fn private_read(path: &Path, label: &str) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|_| format!("failed to read required {label}"))
}

fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}
