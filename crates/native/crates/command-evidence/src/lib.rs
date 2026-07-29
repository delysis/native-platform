//! Evidence-gated command readiness contracts.
//!
//! A blueprint may request a readiness level, but only current, matching V2
//! receipts can prove it. Legacy receipts remain parseable history and never
//! unlock a command.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The immutable identity against which command evidence is evaluated.
#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandEvidenceContext {
    pub app_id: String,
    pub command_id: String,
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub plugin_version: String,
    #[serde(default)]
    pub source_path: String,
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub runtime_fingerprint: String,
    #[serde(default)]
    pub engine_fingerprint: String,
    #[serde(default)]
    pub model_fingerprint: String,
    #[serde(default)]
    pub platform: String,
}

/// Kind of evidence named by a command readiness requirement.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEvidenceKind {
    CompiledLinkage,
    RustTest,
    RuntimeProbe,
    UiProbe,
    Acceptance,
}

/// One independently checkable requirement for a command.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandEvidenceRequirement {
    pub id: String,
    pub kind: CommandEvidenceKind,
    pub reference: String,
    #[serde(default)]
    pub minimum_implementation: ImplementationState,
    #[serde(default)]
    pub minimum_runtime_proof: RuntimeProof,
    #[serde(default)]
    pub minimum_acceptance: AcceptanceState,
}

/// Static implementation maturity proven for a command boundary.
#[derive(Debug, Default, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationState {
    #[default]
    Declared,
    Contracted,
    Compiled,
    Linked,
    HostIntegrated,
}

/// Runtime proof strength for the exact current engine/model fingerprint.
#[derive(Debug, Default, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProof {
    #[default]
    None,
    Fixture,
    RealSmoke,
    #[serde(rename = "real_e2e", alias = "real_e2_e")]
    RealE2E,
}

/// Human/product acceptance, kept separate from implementation and runtime.
#[derive(Debug, Default, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceState {
    #[default]
    Unaccepted,
    UserAccepted,
    Released,
}

/// Result of an evidence attempt.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    Passed,
    Failed,
    Blocked,
}

/// Current evidence receipt. All identity and runtime fields are bound.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadinessReceiptV2 {
    pub schema_version: String,
    pub receipt_id: String,
    pub requirement_id: String,
    pub context: CommandEvidenceContext,
    pub implementation: ImplementationState,
    pub runtime_proof: RuntimeProof,
    pub acceptance: AcceptanceState,
    pub outcome: EvidenceOutcome,
    #[serde(default)]
    pub blockers: Vec<String>,
    pub timestamp: String,
}

impl ReadinessReceiptV2 {
    pub const SCHEMA_VERSION: &'static str = "coop.command-readiness-receipt.v2";
}

/// Legacy receipt retained only for audit history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadinessReceiptV1 {
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

/// Version-tolerant receipt parsing. V2 is attempted before the permissive V1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VersionedReadinessReceipt {
    V2(Box<ReadinessReceiptV2>),
    V1(ReadinessReceiptV1),
}

/// Version-tolerant receipt history.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadinessReceiptHistory {
    #[serde(default)]
    pub receipts: Vec<VersionedReadinessReceipt>,
}

/// Derived command readiness. This, not a blueprint claim, gates controls.
#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectiveCommandReadiness {
    pub implementation: ImplementationState,
    pub runtime_proof: RuntimeProof,
    pub acceptance: AcceptanceState,
    pub unlocked: bool,
    pub matched_receipts: Vec<String>,
    pub blockers: Vec<String>,
}

/// Derive effective readiness from current, exact-match evidence.
#[must_use]
pub fn derive_effective_command_readiness(
    expected: &CommandEvidenceContext,
    requirements: &[CommandEvidenceRequirement],
    receipts: &[VersionedReadinessReceipt],
) -> EffectiveCommandReadiness {
    let mut effective = EffectiveCommandReadiness::default();
    if requirements.is_empty() {
        effective
            .blockers
            .push("missing typed command evidence requirements".to_string());
        return effective;
    }

    let current = receipts
        .iter()
        .filter_map(|receipt| match receipt {
            VersionedReadinessReceipt::V2(receipt)
                if receipt.schema_version == ReadinessReceiptV2::SCHEMA_VERSION
                    && receipt.context == *expected =>
            {
                Some(receipt)
            }
            VersionedReadinessReceipt::V2(_) | VersionedReadinessReceipt::V1(_) => None,
        })
        .collect::<Vec<_>>();

    let mut all_satisfied = true;
    for requirement in requirements {
        let latest = current
            .iter()
            .enumerate()
            .filter(|(_, receipt)| receipt.requirement_id == requirement.id)
            .max_by(|(left_index, left), (right_index, right)| {
                left.timestamp
                    .cmp(&right.timestamp)
                    .then_with(|| left_index.cmp(right_index))
            })
            .map(|(_, receipt)| *receipt);

        let Some(receipt) = latest else {
            all_satisfied = false;
            effective.blockers.push(format!(
                "missing current evidence for requirement `{}`",
                requirement.id
            ));
            continue;
        };

        if receipt.outcome != EvidenceOutcome::Passed {
            all_satisfied = false;
            if receipt.blockers.is_empty() {
                effective.blockers.push(format!(
                    "latest evidence attempt for `{}` is {:?}",
                    requirement.id, receipt.outcome
                ));
            } else {
                effective.blockers.extend(receipt.blockers.clone());
            }
            continue;
        }

        if receipt.implementation < requirement.minimum_implementation
            || receipt.runtime_proof < requirement.minimum_runtime_proof
            || receipt.acceptance < requirement.minimum_acceptance
        {
            all_satisfied = false;
            effective.blockers.push(format!(
                "evidence `{}` is below the required implementation/runtime/acceptance axes",
                receipt.receipt_id
            ));
            continue;
        }

        effective.implementation = effective.implementation.max(receipt.implementation);
        effective.runtime_proof = effective.runtime_proof.max(receipt.runtime_proof);
        effective.acceptance = effective.acceptance.max(receipt.acceptance);
        effective.matched_receipts.push(receipt.receipt_id.clone());
    }
    effective.unlocked = all_satisfied;
    effective
}

/// Atomic, append-only V2 readiness ledger.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadinessLedger {
    pub schema_version: String,
    #[serde(default)]
    pub receipts: Vec<ReadinessReceiptV2>,
}

impl Default for ReadinessLedger {
    fn default() -> Self {
        Self {
            schema_version: "coop.command-readiness-ledger.v2".to_string(),
            receipts: Vec::new(),
        }
    }
}

impl ReadinessLedger {
    /// Load a ledger. A missing file is an empty fail-closed ledger.
    pub fn load(path: &Path) -> Result<Self, ReadinessLedgerError> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Append one receipt and atomically replace the ledger file.
    pub fn append_atomic(
        path: &Path,
        receipt: ReadinessReceiptV2,
    ) -> Result<Self, ReadinessLedgerError> {
        let mut ledger = Self::load(path)?;
        ledger.receipts.push(receipt);
        ledger.write_atomic(path)?;
        Ok(ledger)
    }

    /// Return versioned receipts suitable for readiness derivation.
    #[must_use]
    pub fn versioned_receipts(&self) -> Vec<VersionedReadinessReceipt> {
        self.receipts
            .iter()
            .cloned()
            .map(Box::new)
            .map(VersionedReadinessReceipt::V2)
            .collect()
    }

    fn write_atomic(&self, path: &Path) -> Result<(), ReadinessLedgerError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temp_path = temporary_path(path, self.receipts.len());
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        let result = (|| -> Result<(), ReadinessLedgerError> {
            let bytes = serde_json::to_vec_pretty(self)?;
            temp.write_all(&bytes)?;
            temp.write_all(b"\n")?;
            temp.sync_all()?;
            fs::rename(&temp_path, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ignored = fs::remove_file(&temp_path);
        }
        result
    }
}

fn temporary_path(path: &Path, receipt_count: usize) -> PathBuf {
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("readiness-ledger");
    path.with_file_name(format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        receipt_count
    ))
}

#[derive(Debug, Error)]
pub enum ReadinessLedgerError {
    #[error("readiness ledger I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("readiness ledger JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn atomic_append_preserves_attempt_order() -> Result<(), ReadinessLedgerError> {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root =
            std::env::temp_dir().join(format!("command-evidence-{}-{suffix}", std::process::id()));
        let path = root.join("readiness-ledger-v2.json");
        let base = ReadinessReceiptV2 {
            schema_version: ReadinessReceiptV2::SCHEMA_VERSION.to_string(),
            receipt_id: "pass".to_string(),
            requirement_id: "probe".to_string(),
            context: CommandEvidenceContext::default(),
            implementation: ImplementationState::HostIntegrated,
            runtime_proof: RuntimeProof::RealSmoke,
            acceptance: AcceptanceState::Unaccepted,
            outcome: EvidenceOutcome::Passed,
            blockers: Vec::new(),
            timestamp: "2026-07-17T00:00:00Z".to_string(),
        };
        ReadinessLedger::append_atomic(&path, base.clone())?;
        let mut blocked = base;
        blocked.receipt_id = "blocked".to_string();
        blocked.outcome = EvidenceOutcome::Blocked;
        blocked.timestamp = "2026-07-17T00:01:00Z".to_string();
        ReadinessLedger::append_atomic(&path, blocked)?;
        let loaded = ReadinessLedger::load(&path)?;
        assert_eq!(loaded.receipts.len(), 2);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
