use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_MAX_DISCOVERY_ENTRIES: usize = 100_000;
pub const DEFAULT_MAX_DISCOVERY_DEPTH: usize = 16;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelDiscoveryOptions {
    pub hugging_face_cache_roots: Vec<PathBuf>,
    pub user_paths: Vec<PathBuf>,
    pub max_entries: usize,
    pub max_depth: usize,
}

impl Default for ModelDiscoveryOptions {
    fn default() -> Self {
        Self {
            hugging_face_cache_roots: default_hugging_face_cache_roots(),
            user_paths: Vec::new(),
            max_entries: DEFAULT_MAX_DISCOVERY_ENTRIES,
            max_depth: DEFAULT_MAX_DISCOVERY_DEPTH,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDiscoverySource {
    HuggingFaceCache,
    UserSelected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GgufHeaderStatus {
    Verified,
    Invalid { observed_hex: String },
    Unreadable { reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiscoveredGguf {
    /// User-visible cache or selected path, which may itself be a file symlink.
    pub selected_path: PathBuf,
    /// Canonical target used to deduplicate one GGUF reached through many paths.
    pub resolved_path: PathBuf,
    pub source: ModelDiscoverySource,
    pub file_bytes: u64,
    pub header: GgufHeaderStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiscoveryWarning {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelDiscoveryReport {
    pub models: Vec<DiscoveredGguf>,
    pub warnings: Vec<DiscoveryWarning>,
    /// Entries admitted to the scan, including unreadable directory entries.
    /// Queued paths consume the budget before metadata/header inspection.
    pub visited_entries: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum DiscoveryError {
    #[error("model discovery max_entries must be positive")]
    ZeroEntryLimit,
}

#[must_use]
pub fn default_hugging_face_cache_roots() -> Vec<PathBuf> {
    desktop_model_defaults::hugging_face_hub_cache_dir()
        .into_iter()
        .collect()
}

pub fn discover_gguf_models(
    options: &ModelDiscoveryOptions,
) -> Result<ModelDiscoveryReport, DiscoveryError> {
    if options.max_entries == 0 {
        return Err(DiscoveryError::ZeroEntryLimit);
    }

    let mut report = ModelDiscoveryReport::default();
    let mut user_pending = VecDeque::new();
    let mut cache_pending = VecDeque::new();
    // Explicit user choices win deduplication over cache aliases.
    enqueue_roots(
        &mut user_pending,
        &options.user_paths,
        ModelDiscoverySource::UserSelected,
        options.max_entries,
        &mut report.truncated,
    );
    enqueue_roots(
        &mut cache_pending,
        &options.hugging_face_cache_roots,
        ModelDiscoverySource::HuggingFaceCache,
        options.max_entries.saturating_sub(user_pending.len()),
        &mut report.truncated,
    );
    report.visited_entries = user_pending.len() + cache_pending.len();
    let mut resolved_models = BTreeSet::new();
    let mut visited_directories = BTreeSet::new();

    loop {
        let next = user_pending
            .pop_front()
            .or_else(|| cache_pending.pop_front());
        let Some((path, source, depth)) = next else {
            break;
        };
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                report.warnings.push(DiscoveryWarning {
                    path,
                    message: format!("cannot inspect path: {error}"),
                });
                continue;
            }
        };

        if metadata.file_type().is_symlink() {
            inspect_symlink(&path, source, &mut report, &mut resolved_models);
            continue;
        }
        if metadata.is_file() {
            inspect_file(&path, source, &metadata, &mut report, &mut resolved_models);
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        if depth >= options.max_depth {
            report.truncated = true;
            report.warnings.push(DiscoveryWarning {
                path,
                message: format!("directory depth limit {} reached", options.max_depth),
            });
            continue;
        }
        let resolved_directory = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !visited_directories.insert(resolved_directory) {
            continue;
        }
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                report.warnings.push(DiscoveryWarning {
                    path,
                    message: format!("cannot read directory: {error}"),
                });
                continue;
            }
        };
        // Count queued entries against the same inspection budget before
        // reading another directory. One sentinel read detects truncation;
        // neither directory materialization nor the queue can exceed the cap.
        let remaining = options.max_entries.saturating_sub(report.visited_entries);
        let mut children = Vec::new();
        for (index, entry) in entries.take(remaining.saturating_add(1)).enumerate() {
            if index == remaining {
                report.truncated = true;
                break;
            }
            report.visited_entries += 1;
            match entry {
                Ok(entry) => children.push(entry.path()),
                Err(error) => report.warnings.push(DiscoveryWarning {
                    path: path.clone(),
                    message: format!("cannot read directory entry: {error}"),
                }),
            }
        }
        children.sort();
        let pending = match source {
            ModelDiscoverySource::UserSelected => &mut user_pending,
            ModelDiscoverySource::HuggingFaceCache => &mut cache_pending,
        };
        pending.extend(children.into_iter().map(|child| (child, source, depth + 1)));
    }

    report
        .models
        .sort_by(|left, right| left.selected_path.cmp(&right.selected_path));
    report
        .warnings
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(report)
}

fn enqueue_roots(
    pending: &mut VecDeque<(PathBuf, ModelDiscoverySource, usize)>,
    roots: &[PathBuf],
    source: ModelDiscoverySource,
    remaining: usize,
    truncated: &mut bool,
) {
    *truncated |= roots.len() > remaining;
    let mut ordered = roots.iter().take(remaining).cloned().collect::<Vec<_>>();
    ordered.sort();
    ordered.dedup();
    pending.extend(ordered.into_iter().map(|path| (path, source, 0)));
}

fn inspect_symlink(
    path: &Path,
    source: ModelDiscoverySource,
    report: &mut ModelDiscoveryReport,
    resolved_models: &mut BTreeSet<PathBuf>,
) {
    let target_metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            report.warnings.push(DiscoveryWarning {
                path: path.to_path_buf(),
                message: format!("cannot resolve symlink: {error}"),
            });
            return;
        }
    };
    if target_metadata.is_dir() {
        report.warnings.push(DiscoveryWarning {
            path: path.to_path_buf(),
            message: "directory symlink skipped during bounded discovery".to_string(),
        });
        return;
    }
    if target_metadata.is_file() {
        inspect_file(path, source, &target_metadata, report, resolved_models);
    }
}

fn inspect_file(
    path: &Path,
    source: ModelDiscoverySource,
    metadata: &std::fs::Metadata,
    report: &mut ModelDiscoveryReport,
    resolved_models: &mut BTreeSet<PathBuf>,
) {
    if !has_gguf_extension(path) {
        return;
    }
    let resolved_path = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => {
            report.warnings.push(DiscoveryWarning {
                path: path.to_path_buf(),
                message: format!("cannot canonicalize GGUF path: {error}"),
            });
            path.to_path_buf()
        }
    };
    if !resolved_models.insert(resolved_path.clone()) {
        return;
    }
    report.models.push(DiscoveredGguf {
        selected_path: path.to_path_buf(),
        resolved_path,
        source,
        file_bytes: metadata.len(),
        header: inspect_gguf_header(path),
    });
}

pub fn has_gguf_extension(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
}

/// Candidate classification only; native inspection determines architecture.
#[must_use]
pub fn is_model_gguf(path: &Path) -> bool {
    has_gguf_extension(path)
        && path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                !name.contains("mmproj") && !name.contains("-mtp.")
            })
}

fn inspect_gguf_header(path: &Path) -> GgufHeaderStatus {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            return GgufHeaderStatus::Unreadable {
                reason: error.to_string(),
            };
        }
    };
    let mut magic = [0_u8; 4];
    if let Err(error) = file.read_exact(&mut magic) {
        return GgufHeaderStatus::Unreadable {
            reason: error.to_string(),
        };
    }
    if magic == *b"GGUF" {
        GgufHeaderStatus::Verified
    } else {
        let observed_hex = magic.iter().fold(
            String::with_capacity(magic.len() * 2),
            |mut output, byte| {
                let _ = write!(output, "{byte:02x}");
                output
            },
        );
        GgufHeaderStatus::Invalid { observed_hex }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn discovers_verified_gguf_and_reports_invalid_header() {
        let directory = tempfile::tempdir().expect("temp directory");
        let valid = directory.path().join("model.gguf");
        let invalid = directory.path().join("broken.GGUF");
        File::create(&valid)
            .and_then(|mut file| file.write_all(b"GGUFpayload"))
            .expect("write valid fixture");
        File::create(&invalid)
            .and_then(|mut file| file.write_all(b"nope"))
            .expect("write invalid fixture");

        let report = discover_gguf_models(&ModelDiscoveryOptions {
            hugging_face_cache_roots: Vec::new(),
            user_paths: vec![directory.path().to_path_buf()],
            max_entries: 16,
            max_depth: 4,
        })
        .expect("discover models");

        assert_eq!(report.models.len(), 2);
        assert!(report.models.iter().any(|model| {
            model.selected_path == valid && model.header == GgufHeaderStatus::Verified
        }));
        assert!(report.models.iter().any(|model| {
            model.selected_path == invalid
                && matches!(model.header, GgufHeaderStatus::Invalid { .. })
        }));
        assert!(!report.truncated);
    }

    #[test]
    fn traversal_is_bounded_and_never_claims_completeness() {
        let directory = tempfile::tempdir().expect("temp directory");
        for index in 0..4 {
            File::create(directory.path().join(format!("{index}.gguf")))
                .and_then(|mut file| file.write_all(b"GGUF"))
                .expect("write fixture");
        }
        let report = discover_gguf_models(&ModelDiscoveryOptions {
            hugging_face_cache_roots: Vec::new(),
            user_paths: vec![directory.path().to_path_buf()],
            max_entries: 2,
            max_depth: 4,
        })
        .expect("discover models");
        assert!(report.truncated);
        assert!(report.models.len() < 4);
    }

    #[test]
    fn depth_limited_traversal_is_explicitly_partial() {
        let directory = tempfile::tempdir().expect("directory");
        std::fs::write(directory.path().join("model.gguf"), b"GGUF").expect("model");
        let report = discover_gguf_models(&ModelDiscoveryOptions {
            hugging_face_cache_roots: Vec::new(),
            user_paths: vec![directory.path().into()],
            max_entries: 16,
            max_depth: 0,
        })
        .expect("scan");
        assert!(report.truncated);
        assert!(report.models.is_empty());
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn wide_non_model_directory_consumes_the_same_finite_budget() {
        let directory = tempfile::tempdir().expect("directory");
        for index in 0..1_000 {
            std::fs::write(directory.path().join(format!("note-{index}")), b"x").expect("entry");
        }
        let report = discover_gguf_models(&ModelDiscoveryOptions {
            hugging_face_cache_roots: Vec::new(),
            user_paths: vec![directory.path().into()],
            max_entries: 8,
            max_depth: 2,
        })
        .expect("bounded scan");
        assert!(report.truncated);
        assert_eq!(report.visited_entries, 8);
        assert!(report.models.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn file_alias_is_retained_but_directory_symlinks_are_not_traversed() {
        let directory = tempfile::tempdir().expect("directory");
        let cache = directory.path().join("cache");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&cache).expect("cache");
        std::fs::create_dir(&outside).expect("outside");
        let blob = outside.join("blob");
        std::fs::write(&blob, b"GGUF").expect("blob");
        std::fs::write(outside.join("hidden.gguf"), b"GGUF").expect("hidden");
        let alias = cache.join("model.gguf");
        std::os::unix::fs::symlink(&blob, &alias).expect("alias");
        std::os::unix::fs::symlink(&outside, cache.join("linked-directory"))
            .expect("directory alias");
        let report = discover_gguf_models(&ModelDiscoveryOptions {
            hugging_face_cache_roots: vec![cache],
            user_paths: vec![alias.clone()],
            max_entries: 16,
            max_depth: 4,
        })
        .expect("scan");
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.models[0].selected_path, alias);
        assert_eq!(
            report.models[0].resolved_path,
            blob.canonicalize().expect("canonical")
        );
        assert_eq!(report.models[0].source, ModelDiscoverySource::UserSelected);
        assert!(!report.warnings.is_empty());
    }

    #[test]
    fn projector_and_mtp_names_are_not_primary_model_choices() {
        for path in ["mmproj-model.gguf", "model.mmproj.GGUF", "model-MTP.gguf"] {
            assert!(!is_model_gguf(Path::new(path)));
        }
        assert!(is_model_gguf(Path::new("model.GGUF")));
    }

    /// Developer acceptance check for Loom's quiet first-run writer discovery.
    ///
    /// This intentionally depends on the official artifact being present in the
    /// current user's Hugging Face cache, so it stays ignored in hermetic CI.
    #[test]
    #[ignore = "requires the official Gemma 4 12B QAT Q4_0 GGUF in the local Hugging Face cache"]
    fn real_default_cache_discovers_official_gemma_4_12b_qat() {
        const FILE_NAME: &str = "gemma-4-12b-it-qat-q4_0.gguf";
        const FILE_BYTES: u64 = 6_975_879_296;

        let mut options = ModelDiscoveryOptions::default();
        options.max_entries = options.max_entries.min(20_000);
        options.max_depth = options.max_depth.min(12);
        let report = discover_gguf_models(&options).expect("discover default desktop models");

        assert!(
            !report.truncated,
            "the bounded desktop scan truncated after {} visited entries",
            report.visited_entries
        );
        let model = report
            .models
            .iter()
            .find(|model| {
                model
                    .selected_path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(FILE_NAME))
                    && model.file_bytes == FILE_BYTES
            })
            .expect("official Gemma 4 12B QAT Q4_0 artifact");

        assert_eq!(model.source, ModelDiscoverySource::HuggingFaceCache);
        assert_eq!(model.header, GgufHeaderStatus::Verified);
        assert!(model.resolved_path.is_file());
    }
}
