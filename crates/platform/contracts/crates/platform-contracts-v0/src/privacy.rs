use crate::ContractError;
use crate::ids::ProviderId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PRIVACY_POLICY_SCHEMA_V0: &str = "delysis.privacy_policy.v0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicyV0 {
    Deny,
    AllowListed,
    AllowHosted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataHandlingV0 {
    LocalOnly,
    HostedAllowed,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataTierV0 {
    Public,
    Private,
    Restricted,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadRedactionV0 {
    LocalOnly,
    RequiredBeforeEgress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoggingPolicyV0 {
    Disabled,
    RedactedMetadataOnly,
}

/// Declarative privacy policy. Provider IDs are opaque labels, not endpoints,
/// credentials, network clients, or authority to contact a provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrivacyPolicyV0 {
    pub schema: String,
    pub network: NetworkPolicyV0,
    pub data_handling: DataHandlingV0,
    pub allowed_provider_ids: Vec<ProviderId>,
    pub allowed_hosted_data_tiers: Vec<DataTierV0>,
    pub payload_redaction: PayloadRedactionV0,
    pub logging: LoggingPolicyV0,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrivacyPolicyV0 {
    schema: String,
    network: NetworkPolicyV0,
    data_handling: DataHandlingV0,
    allowed_provider_ids: Vec<ProviderId>,
    allowed_hosted_data_tiers: Vec<DataTierV0>,
    payload_redaction: PayloadRedactionV0,
    logging: LoggingPolicyV0,
}

impl<'de> Deserialize<'de> for PrivacyPolicyV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawPrivacyPolicyV0::deserialize(deserializer)?;
        let policy = Self {
            schema: raw.schema,
            network: raw.network,
            data_handling: raw.data_handling,
            allowed_provider_ids: raw.allowed_provider_ids,
            allowed_hosted_data_tiers: raw.allowed_hosted_data_tiers,
            payload_redaction: raw.payload_redaction,
            logging: raw.logging,
        };
        policy.validate().map_err(serde::de::Error::custom)?;
        Ok(policy)
    }
}

impl PrivacyPolicyV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != PRIVACY_POLICY_SCHEMA_V0 {
            return Err(ContractError::Invalid { field: "schema" });
        }

        let mut provider_ids = BTreeSet::new();
        for provider_id in &self.allowed_provider_ids {
            if !provider_ids.insert(provider_id) {
                return Err(ContractError::Duplicate {
                    field: "allowed_provider_ids",
                });
            }
        }

        let mut tiers = BTreeSet::new();
        for tier in &self.allowed_hosted_data_tiers {
            if *tier == DataTierV0::Unknown {
                return Err(ContractError::Invalid {
                    field: "allowed_hosted_data_tiers",
                });
            }
            if !tiers.insert(*tier) {
                return Err(ContractError::Duplicate {
                    field: "allowed_hosted_data_tiers",
                });
            }
        }

        match self.data_handling {
            DataHandlingV0::LocalOnly => {
                if self.network != NetworkPolicyV0::Deny
                    || !self.allowed_provider_ids.is_empty()
                    || !self.allowed_hosted_data_tiers.is_empty()
                    || self.payload_redaction != PayloadRedactionV0::LocalOnly
                {
                    return Err(ContractError::Inconsistent {
                        field: "data_handling",
                    });
                }
            }
            DataHandlingV0::HostedAllowed => {
                if self.network == NetworkPolicyV0::Deny
                    || self.allowed_hosted_data_tiers.is_empty()
                    || self.payload_redaction != PayloadRedactionV0::RequiredBeforeEgress
                {
                    return Err(ContractError::Inconsistent {
                        field: "data_handling",
                    });
                }
                match self.network {
                    NetworkPolicyV0::AllowListed if self.allowed_provider_ids.is_empty() => {
                        return Err(ContractError::Empty {
                            field: "allowed_provider_ids",
                        });
                    }
                    NetworkPolicyV0::AllowHosted if !self.allowed_provider_ids.is_empty() => {
                        return Err(ContractError::Inconsistent {
                            field: "allowed_provider_ids",
                        });
                    }
                    NetworkPolicyV0::Deny
                    | NetworkPolicyV0::AllowListed
                    | NetworkPolicyV0::AllowHosted => {}
                }
            }
        }
        Ok(())
    }

    /// Decide whether a proposed route is permitted. Invalid policy state,
    /// unknown observations, and anything not explicitly allowed are denied.
    #[must_use]
    pub fn decide(&self, route: &RoutePrivacyContextV0) -> PrivacyDecisionV0 {
        if self.validate().is_err() {
            return PrivacyDecisionV0::Denied(PrivacyDenialV0::InvalidPolicy);
        }
        if route.data_tier == DataTierV0::Unknown {
            return PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownDataTier);
        }

        let RouteTargetV0::Hosted { provider_id } = &route.target else {
            return match route.target {
                RouteTargetV0::Local => PrivacyDecisionV0::Allowed,
                RouteTargetV0::Unknown => PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownRoute),
                RouteTargetV0::Hosted { .. } => unreachable!("hosted route matched above"),
            };
        };

        if self.data_handling == DataHandlingV0::LocalOnly {
            return PrivacyDecisionV0::Denied(PrivacyDenialV0::LocalOnlyBoundary);
        }
        match self.network {
            NetworkPolicyV0::Deny => {
                return PrivacyDecisionV0::Denied(PrivacyDenialV0::NetworkDenied);
            }
            NetworkPolicyV0::AllowListed if !self.allowed_provider_ids.contains(provider_id) => {
                return PrivacyDecisionV0::Denied(PrivacyDenialV0::ProviderNotAllowed);
            }
            NetworkPolicyV0::AllowListed | NetworkPolicyV0::AllowHosted => {}
        }
        if !self.allowed_hosted_data_tiers.contains(&route.data_tier) {
            return PrivacyDecisionV0::Denied(PrivacyDenialV0::DataTierNotAllowed);
        }
        match route.redaction {
            RedactionStateV0::Applied => PrivacyDecisionV0::Allowed,
            RedactionStateV0::NotApplied => {
                PrivacyDecisionV0::Denied(PrivacyDenialV0::RedactionRequired)
            }
            RedactionStateV0::Unknown => {
                PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownRedactionState)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteTargetV0 {
    Local,
    Hosted { provider_id: ProviderId },
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedactionStateV0 {
    Applied,
    NotApplied,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutePrivacyContextV0 {
    pub target: RouteTargetV0,
    pub data_tier: DataTierV0,
    pub redaction: RedactionStateV0,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivacyDecisionV0 {
    Allowed,
    Denied(PrivacyDenialV0),
}

impl PrivacyDecisionV0 {
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivacyDenialV0 {
    InvalidPolicy,
    UnknownRoute,
    UnknownDataTier,
    UnknownRedactionState,
    LocalOnlyBoundary,
    NetworkDenied,
    ProviderNotAllowed,
    DataTierNotAllowed,
    RedactionRequired,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted_policy() -> PrivacyPolicyV0 {
        PrivacyPolicyV0 {
            schema: PRIVACY_POLICY_SCHEMA_V0.to_owned(),
            network: NetworkPolicyV0::AllowListed,
            data_handling: DataHandlingV0::HostedAllowed,
            allowed_provider_ids: vec![ProviderId::new("provider.cerebras").expect("provider ID")],
            allowed_hosted_data_tiers: vec![DataTierV0::Public],
            payload_redaction: PayloadRedactionV0::RequiredBeforeEgress,
            logging: LoggingPolicyV0::RedactedMetadataOnly,
        }
    }

    fn hosted_route(provider_id: &str, tier: DataTierV0) -> RoutePrivacyContextV0 {
        RoutePrivacyContextV0 {
            target: RouteTargetV0::Hosted {
                provider_id: ProviderId::new(provider_id).expect("provider ID"),
            },
            data_tier: tier,
            redaction: RedactionStateV0::Applied,
        }
    }

    #[test]
    fn hosted_decision_requires_provider_tier_and_redaction() {
        let policy = hosted_policy();
        assert_eq!(
            policy.decide(&hosted_route("provider.cerebras", DataTierV0::Public)),
            PrivacyDecisionV0::Allowed
        );
        assert_eq!(
            policy.decide(&hosted_route("provider.other", DataTierV0::Public)),
            PrivacyDecisionV0::Denied(PrivacyDenialV0::ProviderNotAllowed)
        );
        assert_eq!(
            policy.decide(&hosted_route("provider.cerebras", DataTierV0::Private)),
            PrivacyDecisionV0::Denied(PrivacyDenialV0::DataTierNotAllowed)
        );

        let mut unknown = hosted_route("provider.cerebras", DataTierV0::Public);
        unknown.redaction = RedactionStateV0::Unknown;
        assert_eq!(
            policy.decide(&unknown),
            PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownRedactionState)
        );
    }

    #[test]
    fn unknown_route_and_data_tier_fail_closed() {
        let policy = hosted_policy();
        assert_eq!(
            policy.decide(&RoutePrivacyContextV0 {
                target: RouteTargetV0::Unknown,
                data_tier: DataTierV0::Public,
                redaction: RedactionStateV0::Unknown,
            }),
            PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownRoute)
        );
        assert_eq!(
            policy.decide(&hosted_route("provider.cerebras", DataTierV0::Unknown)),
            PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownDataTier)
        );
    }

    #[test]
    fn unknown_fields_and_authority_are_rejected() {
        let mut value = serde_json::to_value(hosted_policy()).expect("serialize policy");
        value
            .as_object_mut()
            .expect("policy object")
            .insert("api_key".to_owned(), serde_json::json!("secret"));
        assert!(serde_json::from_value::<PrivacyPolicyV0>(value).is_err());
    }

    #[test]
    fn semantic_policy_violations_fail_during_deserialization() {
        let mut local_with_provider = serde_json::json!({
            "schema": PRIVACY_POLICY_SCHEMA_V0,
            "network": "deny",
            "data_handling": "local_only",
            "allowed_provider_ids": [],
            "allowed_hosted_data_tiers": [],
            "payload_redaction": "local_only",
            "logging": "disabled"
        });
        local_with_provider["allowed_provider_ids"] = serde_json::json!(["provider.cerebras"]);
        assert!(serde_json::from_value::<PrivacyPolicyV0>(local_with_provider).is_err());

        let mut hosted_without_provider =
            serde_json::to_value(hosted_policy()).expect("serialize hosted policy");
        hosted_without_provider["allowed_provider_ids"] = serde_json::json!([]);
        assert!(serde_json::from_value::<PrivacyPolicyV0>(hosted_without_provider).is_err());

        let mut unknown_tier =
            serde_json::to_value(hosted_policy()).expect("serialize hosted policy");
        unknown_tier["allowed_hosted_data_tiers"] = serde_json::json!(["unknown"]);
        assert!(serde_json::from_value::<PrivacyPolicyV0>(unknown_tier).is_err());

        let mut duplicate_provider =
            serde_json::to_value(hosted_policy()).expect("serialize hosted policy");
        duplicate_provider["allowed_provider_ids"] =
            serde_json::json!(["provider.cerebras", "provider.cerebras"]);
        assert!(serde_json::from_value::<PrivacyPolicyV0>(duplicate_provider).is_err());
    }
}
