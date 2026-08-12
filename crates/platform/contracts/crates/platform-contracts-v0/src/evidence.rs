use crate::ids::ContentDigest;
use crate::{ContractError, validate_nonempty_bounded};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const EVIDENCE_SCHEMA_V0: &str = "delysis.evidence_claim.v0";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTier {
    Operational,
    Reproducible,
    ResearchEligible,
    ExternallyAttested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    Fixture,
    LocalRuntime,
    HostedNetwork,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceClaimV0 {
    pub schema: String,
    pub tier: EvidenceTier,
    pub threat_model: String,
    pub exact_source: ContentDigest,
    pub exact_runtime_or_artifact: ContentDigest,
    pub execution_kind: ExecutionKind,
    pub omitted_claims: Vec<String>,
    pub negative_evidence: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvidenceClaimV0 {
    schema: String,
    tier: EvidenceTier,
    threat_model: String,
    exact_source: ContentDigest,
    exact_runtime_or_artifact: ContentDigest,
    execution_kind: ExecutionKind,
    omitted_claims: Vec<String>,
    negative_evidence: Vec<String>,
}

impl<'de> Deserialize<'de> for EvidenceClaimV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawEvidenceClaimV0::deserialize(deserializer)?;
        let value = Self {
            schema: raw.schema,
            tier: raw.tier,
            threat_model: raw.threat_model,
            exact_source: raw.exact_source,
            exact_runtime_or_artifact: raw.exact_runtime_or_artifact,
            execution_kind: raw.execution_kind,
            omitted_claims: raw.omitted_claims,
            negative_evidence: raw.negative_evidence,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl EvidenceClaimV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != EVIDENCE_SCHEMA_V0 {
            return Err(ContractError::Invalid { field: "schema" });
        }
        validate_nonempty_bounded("threat_model", &self.threat_model, 4096)?;
        self.exact_source
            .validate()
            .map_err(|_| ContractError::Invalid {
                field: "exact_source",
            })?;
        self.exact_runtime_or_artifact
            .validate()
            .map_err(|_| ContractError::Invalid {
                field: "exact_runtime_or_artifact",
            })?;
        validate_unique_nonempty("omitted_claims", &self.omitted_claims)?;
        validate_unique_nonempty("negative_evidence", &self.negative_evidence)?;
        if matches!(
            self.tier,
            EvidenceTier::ResearchEligible | EvidenceTier::ExternallyAttested
        ) {
            return Err(ContractError::Inconsistent { field: "tier" });
        }
        Ok(())
    }
}

fn validate_unique_nonempty(field: &'static str, values: &[String]) -> Result<(), ContractError> {
    let mut seen = BTreeSet::new();
    for value in values {
        validate_nonempty_bounded(field, value, 2048)?;
        if !seen.insert(value) {
            return Err(ContractError::Duplicate { field });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim() -> EvidenceClaimV0 {
        EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: "local fixture protocol behavior only".to_owned(),
            exact_source: ContentDigest::sha256("a".repeat(64)).expect("digest"),
            exact_runtime_or_artifact: ContentDigest::sha256("b".repeat(64)).expect("digest"),
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: vec!["live hosted-provider behavior".to_owned()],
            negative_evidence: Vec::new(),
        }
    }

    #[test]
    fn serializable_v0_accepts_only_operational_and_reproducible_tiers() {
        let mut value = claim();
        value.tier = EvidenceTier::Operational;
        assert!(value.validate().is_ok());
        value.tier = EvidenceTier::Reproducible;
        assert!(value.validate().is_ok());
    }

    #[test]
    fn serializable_v0_rejects_higher_tiers_for_live_execution_kinds() {
        for execution_kind in [ExecutionKind::LocalRuntime, ExecutionKind::HostedNetwork] {
            for tier in [
                EvidenceTier::ResearchEligible,
                EvidenceTier::ExternallyAttested,
            ] {
                let mut value = claim();
                value.execution_kind = execution_kind;
                value.tier = tier;
                value.threat_model.push_str(" with independent attestation");
                assert_eq!(
                    value.validate(),
                    Err(ContractError::Inconsistent { field: "tier" })
                );
            }
        }
    }

    #[test]
    fn evidence_has_no_serialized_live_authority_field() {
        let json = serde_json::to_value(claim()).expect("serialize claim");
        assert!(json.get("authority").is_none());
        assert!(json.get("foreground_command").is_none());
    }
}
