#![forbid(unsafe_code)]

//! One-time, test-only translator for the preserved Python autoresearch lab.
//!
//! This test deliberately does not insert anything into Loom. Historical
//! reconstruction proves only that the preserved source bytes still explain
//! the old manuscript. It cannot mint a live inference envelope or an
//! assembly admission.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use loom_research_types::NonEmptyByteRange;
use loom_types::BlobId;
use serde::Deserialize;
use serde_json::Value;

const AUDIT_PATH: &str = "04_review_governance/provenance/v4_assembly_invalidation.v1.json";
const EXPECTED_AUDIT_HASH: &str =
    "679110aea8e2bdc7bfc6559819c55994ae07a8310e65c66185b2da0a050706d0";
const MAX_AUDIT_BYTES: u64 = 1024 * 1024;
const MAX_ASSEMBLY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RAW_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FINDINGS: usize = 1024;
const MAX_SPANS: usize = 256;
const MAX_FIELD_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyAudit {
    audit_hash: String,
    audited_at: String,
    counts: BTreeMap<String, usize>,
    findings: Vec<LegacyFinding>,
    policy: BTreeMap<String, String>,
    project_root: String,
    record_type: String,
    total: usize,
    version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyFinding {
    artifact_id: String,
    assembly_path: String,
    assembly_sha256: String,
    manuscript_hash: String,
    reconstruction_error: String,
    release_eligible: bool,
    replay_error: String,
    status: String,
    strict_error: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyAssembly {
    artifact_id: String,
    manuscript_hash: String,
    record_type: String,
    separators: Vec<String>,
    spans: Vec<LegacySpan>,
    version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySpan {
    call_id: String,
    #[serde(default)]
    call_ledger_path: String,
    #[serde(default)]
    call_record_hash: String,
    raw_char_end: usize,
    raw_char_start: usize,
    raw_hash: String,
    raw_path: String,
    role: String,
    span_id: String,
    text_hash: String,
}

#[derive(Debug)]
enum TranslationError {
    EmptySpan { span_id: String },
    Invalid(String),
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySpan { span_id } => write!(formatter, "legacy span is empty: {span_id}"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl Error for TranslationError {}

#[derive(Debug)]
struct HistoricalReconstruction {
    artifact_id: String,
    manuscript_hash: String,
    span_count: usize,
}

#[test]
fn python_character_offsets_translate_to_utf8_byte_offsets() {
    let text = "aé🜁z";
    assert_eq!(python_char_to_byte(text, 0), Some(0));
    assert_eq!(python_char_to_byte(text, 1), Some(1));
    assert_eq!(python_char_to_byte(text, 2), Some(3));
    assert_eq!(python_char_to_byte(text, 3), Some(7));
    assert_eq!(python_char_to_byte(text, 4), Some(8));
    assert_eq!(python_char_to_byte(text, 5), None);
}

#[test]
#[ignore = "requires LOOM_LEGACY_FIXTURE_ROOT pointing at the preserved Python lab"]
fn preserved_python_assemblies_reconstruct_or_fail_on_empty_spans() -> Result<(), Box<dyn Error>> {
    let root = std::env::var_os("LOOM_LEGACY_FIXTURE_ROOT")
        .ok_or("LOOM_LEGACY_FIXTURE_ROOT is not set")?;
    let root = PathBuf::from(root).canonicalize()?;
    if !root.is_dir() {
        return Err("legacy fixture root is not a directory".into());
    }

    let audit_path = confined_file(&root, AUDIT_PATH)?;
    let audit_bytes = read_bounded(&audit_path, MAX_AUDIT_BYTES)?;
    let mut audit_value: Value = serde_json::from_slice(&audit_bytes)?;
    let audit: LegacyAudit = serde_json::from_slice(&audit_bytes)?;

    assert_eq!(audit.record_type, "V4AssemblyInvalidationLedger");
    assert_eq!(audit.version, "v4-assembly-audit.v1");
    assert_eq!(audit.audit_hash, EXPECTED_AUDIT_HASH);
    assert!(!audit.audited_at.is_empty());
    assert!(!audit.project_root.is_empty());
    assert_eq!(audit.policy.len(), 4);
    assert_eq!(audit.total, 128);
    assert_eq!(audit.findings.len(), audit.total);
    assert!(audit.findings.len() <= MAX_FINDINGS);
    assert_eq!(audit.counts.get("historical_unbound"), Some(&111));
    assert_eq!(audit.counts.get("invalid_reconstruction"), Some(&17));
    verify_audit_hash(&mut audit_value, &audit.audit_hash)?;

    let mut reconstructed = 0_usize;
    let mut rejected_empty = 0_usize;
    for finding in &audit.findings {
        assert!(!finding.release_eligible);
        assert!(finding.replay_error.is_empty());
        check_field("assembly path", &finding.assembly_path)?;
        check_digest("assembly sha256", &finding.assembly_sha256)?;

        let assembly_path = confined_file(&root, &finding.assembly_path)?;
        let assembly_bytes = read_bounded(&assembly_path, MAX_ASSEMBLY_BYTES)?;
        if digest(&assembly_bytes) != finding.assembly_sha256 {
            return Err(format!("assembly bytes changed: {}", finding.assembly_path).into());
        }
        let assembly: LegacyAssembly = serde_json::from_slice(&assembly_bytes)?;
        match translate_historical_assembly(&root, assembly) {
            Ok(value) => {
                if finding.status != "historical_unbound"
                    || finding.artifact_id != value.artifact_id
                    || finding.manuscript_hash != value.manuscript_hash
                    || !finding.reconstruction_error.is_empty()
                    || finding.strict_error.is_empty()
                    || value.span_count == 0
                {
                    return Err(format!(
                        "audit classification disagrees with reconstruction: {}",
                        finding.assembly_path
                    )
                    .into());
                }
                reconstructed += 1;
            }
            Err(TranslationError::EmptySpan { .. }) => {
                if finding.status != "invalid_reconstruction"
                    || finding.reconstruction_error.is_empty()
                    || !finding.artifact_id.is_empty()
                    || !finding.manuscript_hash.is_empty()
                    || !finding.strict_error.is_empty()
                {
                    return Err(format!(
                        "empty-span rejection disagrees with audit: {}",
                        finding.assembly_path
                    )
                    .into());
                }
                rejected_empty += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }

    assert_eq!(reconstructed, 111);
    assert_eq!(rejected_empty, 17);
    Ok(())
}

fn translate_historical_assembly(
    root: &Path,
    assembly: LegacyAssembly,
) -> Result<HistoricalReconstruction, TranslationError> {
    check_field("artifact id", &assembly.artifact_id)?;
    check_digest("manuscript hash", &assembly.manuscript_hash)?;
    if assembly.record_type != "ManuscriptAssembly"
        || assembly.version != "model-span-assembly.v1"
        || assembly.spans.is_empty()
        || assembly.spans.len() > MAX_SPANS
        || assembly.separators.len() != assembly.spans.len().saturating_sub(1)
        || assembly
            .separators
            .iter()
            .any(|separator| separator != "\n\n")
    {
        return Err(TranslationError::Invalid(
            "legacy assembly shape or separator is invalid".to_string(),
        ));
    }

    let mut reconstructed = Vec::new();
    for (index, span) in assembly.spans.iter().enumerate() {
        for (label, value) in [
            ("call id", span.call_id.as_str()),
            ("role", span.role.as_str()),
            ("span id", span.span_id.as_str()),
            ("raw path", span.raw_path.as_str()),
        ] {
            check_field(label, value)?;
        }
        check_digest("raw hash", &span.raw_hash)?;
        check_digest("span text hash", &span.text_hash)?;
        if !span.call_ledger_path.is_empty() || !span.call_record_hash.is_empty() {
            return Err(TranslationError::Invalid(
                "fixture unexpectedly contains a canonical call reference".to_string(),
            ));
        }

        let raw_path = confined_file(root, &span.raw_path)?;
        let raw_bytes = read_bounded(&raw_path, MAX_RAW_BYTES)?;
        let raw = python_utf8_text(raw_bytes)?;
        if digest(raw.as_bytes()) != span.raw_hash {
            return Err(TranslationError::Invalid(format!(
                "legacy raw response changed for {}",
                span.span_id
            )));
        }

        let start = python_char_to_byte(&raw, span.raw_char_start).ok_or_else(|| {
            TranslationError::Invalid(format!("span start is out of bounds: {}", span.span_id))
        })?;
        let end = python_char_to_byte(&raw, span.raw_char_end).ok_or_else(|| {
            TranslationError::Invalid(format!("span end is out of bounds: {}", span.span_id))
        })?;
        let range = NonEmptyByteRange::new(start as u64, end as u64).map_err(|error| {
            if start == end {
                TranslationError::EmptySpan {
                    span_id: span.span_id.clone(),
                }
            } else {
                TranslationError::Invalid(format!(
                    "legacy span range is invalid for {}: {error}",
                    span.span_id
                ))
            }
        })?;
        let text = range.checked_str(raw.as_bytes()).map_err(|error| {
            TranslationError::Invalid(format!(
                "legacy span is not a UTF-8 byte slice for {}: {error}",
                span.span_id
            ))
        })?;
        if digest(text.as_bytes()) != span.text_hash {
            return Err(TranslationError::Invalid(format!(
                "legacy span extraction changed for {}",
                span.span_id
            )));
        }
        if index > 0 {
            reconstructed.extend_from_slice(assembly.separators[index - 1].as_bytes());
        }
        reconstructed.extend_from_slice(text.as_bytes());
    }

    let manuscript_hash = digest(&reconstructed);
    if manuscript_hash != assembly.manuscript_hash {
        return Err(TranslationError::Invalid(
            "assembled manuscript hash mismatch".to_string(),
        ));
    }
    Ok(HistoricalReconstruction {
        artifact_id: assembly.artifact_id,
        manuscript_hash,
        span_count: assembly.spans.len(),
    })
}

fn python_char_to_byte(text: &str, character_index: usize) -> Option<usize> {
    if character_index == text.chars().count() {
        return Some(text.len());
    }
    text.char_indices()
        .nth(character_index)
        .map(|(byte_index, _)| byte_index)
}

fn python_utf8_text(bytes: Vec<u8>) -> Result<String, TranslationError> {
    let text = String::from_utf8(bytes).map_err(|error| {
        TranslationError::Invalid(format!("raw response is not UTF-8: {error}"))
    })?;
    if !text.contains('\r') {
        return Ok(text);
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn confined_file(root: &Path, relative: &str) -> Result<PathBuf, TranslationError> {
    check_field("relative fixture path", relative)?;
    let relative = Path::new(relative);
    if relative.is_absolute() {
        return Err(TranslationError::Invalid(
            "fixture path must be relative".to_string(),
        ));
    }
    let path = root.join(relative).canonicalize().map_err(|error| {
        TranslationError::Invalid(format!("failed to resolve fixture path: {error}"))
    })?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(TranslationError::Invalid(
            "fixture path escapes the preserved root or is not a file".to_string(),
        ));
    }
    Ok(path)
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, TranslationError> {
    let file = fs::File::open(path).map_err(|error| {
        TranslationError::Invalid(format!("failed to open fixture file: {error}"))
    })?;
    let metadata = file.metadata().map_err(|error| {
        TranslationError::Invalid(format!("failed to inspect fixture file: {error}"))
    })?;
    if metadata.len() > maximum {
        return Err(TranslationError::Invalid(format!(
            "fixture file exceeds {maximum} bytes"
        )));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        TranslationError::Invalid("fixture size does not fit this platform".to_string())
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            TranslationError::Invalid(format!("failed to read fixture file: {error}"))
        })?;
    if bytes.len() as u64 > maximum {
        return Err(TranslationError::Invalid(format!(
            "fixture file exceeds {maximum} bytes"
        )));
    }
    if bytes.len() as u64 != metadata.len() {
        return Err(TranslationError::Invalid(
            "fixture file changed while it was read".to_string(),
        ));
    }
    Ok(bytes)
}

fn verify_audit_hash(value: &mut Value, expected: &str) -> Result<(), TranslationError> {
    let object = value.as_object_mut().ok_or_else(|| {
        TranslationError::Invalid("legacy audit is not a JSON object".to_string())
    })?;
    object.remove("audited_at");
    object.remove("audit_hash");
    let mut canonical = serde_json::to_string_pretty(value)
        .map_err(|error| TranslationError::Invalid(format!("audit JSON failed: {error}")))?;
    canonical.push('\n');
    if digest(canonical.as_bytes()) != expected {
        return Err(TranslationError::Invalid(
            "legacy audit hash does not match canonical content".to_string(),
        ));
    }
    Ok(())
}

fn check_field(label: &str, value: &str) -> Result<(), TranslationError> {
    if value.is_empty() || value.len() > MAX_FIELD_BYTES || value.contains('\0') {
        return Err(TranslationError::Invalid(format!(
            "{label} is empty or exceeds its fixture bound"
        )));
    }
    Ok(())
}

fn check_digest(label: &str, value: &str) -> Result<(), TranslationError> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(TranslationError::Invalid(format!(
            "{label} is not a canonical SHA-256 digest"
        )));
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    BlobId::digest(bytes).to_string()
}
