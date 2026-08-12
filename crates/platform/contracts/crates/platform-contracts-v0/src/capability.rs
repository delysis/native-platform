use crate::ids::{ContentDigest, ServiceId};
use crate::{ContractError, validate_nonempty_bounded};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const CAPABILITY_SCHEMA_V0: &str = "delysis.capability_snapshot.v0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriState {
    Yes,
    No,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEntryV0 {
    pub operation: String,
    pub backend_or_resource_id: String,
    pub readiness: Readiness,
    pub limits: BTreeMap<String, u64>,
    pub network: TriState,
    pub privacy_eligible: TriState,
    pub evidence_source: String,
    pub evidence_outcome: String,
    pub observed_at_unix_ms: Option<u64>,
    pub remediation: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilityEntryV0 {
    operation: String,
    backend_or_resource_id: String,
    readiness: Readiness,
    limits: BTreeMap<String, u64>,
    network: TriState,
    privacy_eligible: TriState,
    evidence_source: String,
    evidence_outcome: String,
    observed_at_unix_ms: Option<u64>,
    remediation: Option<String>,
}

impl<'de> Deserialize<'de> for CapabilityEntryV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawCapabilityEntryV0::deserialize(deserializer)?;
        let value = Self {
            operation: raw.operation,
            backend_or_resource_id: raw.backend_or_resource_id,
            readiness: raw.readiness,
            limits: raw.limits,
            network: raw.network,
            privacy_eligible: raw.privacy_eligible,
            evidence_source: raw.evidence_source,
            evidence_outcome: raw.evidence_outcome,
            observed_at_unix_ms: raw.observed_at_unix_ms,
            remediation: raw.remediation,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl CapabilityEntryV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_nonempty_bounded("operation", &self.operation, 128)?;
        validate_nonempty_bounded("backend_or_resource_id", &self.backend_or_resource_id, 256)?;
        validate_nonempty_bounded("evidence_source", &self.evidence_source, 512)?;
        validate_nonempty_bounded("evidence_outcome", &self.evidence_outcome, 512)?;
        if self.limits.keys().any(|key| {
            key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        }) {
            return Err(ContractError::Invalid { field: "limits" });
        }
        if self.readiness != Readiness::Ready && self.remediation.is_none() {
            return Err(ContractError::Inconsistent {
                field: "remediation",
            });
        }
        if let Some(remediation) = &self.remediation {
            validate_nonempty_bounded("remediation", remediation, 1024)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySourceReportV0 {
    pub source_id: String,
    pub outcome: String,
    pub safe_detail: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilitySourceReportV0 {
    source_id: String,
    outcome: String,
    safe_detail: Option<String>,
}

impl<'de> Deserialize<'de> for CapabilitySourceReportV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawCapabilitySourceReportV0::deserialize(deserializer)?;
        let value = Self {
            source_id: raw.source_id,
            outcome: raw.outcome,
            safe_detail: raw.safe_detail,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl CapabilitySourceReportV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_nonempty_bounded("source_id", &self.source_id, 256)?;
        validate_nonempty_bounded("outcome", &self.outcome, 128)?;
        if let Some(detail) = &self.safe_detail {
            validate_nonempty_bounded("safe_detail", detail, 2048)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySnapshotV0 {
    pub schema: String,
    pub snapshot_id: ContentDigest,
    pub target: String,
    pub services: BTreeMap<ServiceId, Vec<CapabilityEntryV0>>,
    pub reports: Vec<CapabilitySourceReportV0>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilitySnapshotV0 {
    schema: String,
    snapshot_id: ContentDigest,
    target: String,
    services: BTreeMap<ServiceId, Vec<CapabilityEntryV0>>,
    reports: Vec<CapabilitySourceReportV0>,
}

impl<'de> Deserialize<'de> for CapabilitySnapshotV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawCapabilitySnapshotV0::deserialize(deserializer)?;
        let value = Self {
            schema: raw.schema,
            snapshot_id: raw.snapshot_id,
            target: raw.target,
            services: raw.services,
            reports: raw.reports,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl CapabilitySnapshotV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != CAPABILITY_SCHEMA_V0 {
            return Err(ContractError::Invalid { field: "schema" });
        }
        self.snapshot_id
            .validate()
            .map_err(|_| ContractError::Invalid {
                field: "snapshot_id",
            })?;
        validate_nonempty_bounded("target", &self.target, 256)?;
        if self.services.is_empty() {
            return Err(ContractError::Empty { field: "services" });
        }
        for entries in self.services.values() {
            if entries.is_empty() {
                return Err(ContractError::Empty {
                    field: "service_entries",
                });
            }
            for entry in entries {
                entry.validate()?;
            }
        }
        let mut source_ids = BTreeSet::new();
        for report in &self.reports {
            report.validate()?;
            if !source_ids.insert(&report.source_id) {
                return Err(ContractError::Duplicate {
                    field: "reports.source_id",
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(readiness: Readiness, remediation: Option<&str>) -> CapabilityEntryV0 {
        CapabilityEntryV0 {
            operation: "generate".to_owned(),
            backend_or_resource_id: "local-model".to_owned(),
            readiness,
            limits: BTreeMap::from([("context_tokens".to_owned(), 4096)]),
            network: TriState::No,
            privacy_eligible: TriState::Yes,
            evidence_source: "runtime probe".to_owned(),
            evidence_outcome: "observed".to_owned(),
            observed_at_unix_ms: Some(1),
            remediation: remediation.map(str::to_owned),
        }
    }

    #[test]
    fn unknown_and_unavailable_require_real_remediation() {
        assert!(entry(Readiness::Ready, None).validate().is_ok());
        assert!(entry(Readiness::Unknown, None).validate().is_err());
        assert!(
            entry(Readiness::Unavailable, Some("choose a model"))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn unknown_is_not_serialized_as_boolean() {
        assert_eq!(
            serde_json::to_string(&TriState::Unknown).expect("serialize"),
            "\"unknown\""
        );
    }

    #[test]
    fn capability_entry_denies_unknown_fields() {
        let mut json = serde_json::to_value(entry(Readiness::Ready, None)).expect("serialize");
        json.as_object_mut()
            .expect("entry object")
            .insert("unlimited".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<CapabilityEntryV0>(json).is_err());
    }
}
