use crate::error::{ErrorClass, ServiceErrorV0};
use crate::ids::ContentDigest;
use crate::{ContractError, validate_nonempty_bounded};
use serde::{Deserialize, Serialize};

pub const PUBLICATION_RECEIPT_SCHEMA_V0: &str = "delysis.publication_receipt.v0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactIdentityV0 {
    pub id: String,
    pub digest: ContentDigest,
    pub length: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArtifactIdentityV0 {
    id: String,
    digest: ContentDigest,
    length: u64,
}

impl<'de> Deserialize<'de> for ArtifactIdentityV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawArtifactIdentityV0::deserialize(deserializer)?;
        let value = Self {
            id: raw.id,
            digest: raw.digest,
            length: raw.length,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl ArtifactIdentityV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_nonempty_bounded("artifact.id", &self.id, 256)?;
        self.digest.validate().map_err(|_| ContractError::Invalid {
            field: "artifact.digest",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationIdentityV0 {
    /// Opaque identity for the publication filesystem, not an access path.
    pub filesystem_id: String,
    /// Opaque identity for the destination entry, not an authority-bearing path.
    pub path_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDestinationIdentityV0 {
    filesystem_id: String,
    path_id: String,
}

impl<'de> Deserialize<'de> for DestinationIdentityV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawDestinationIdentityV0::deserialize(deserializer)?;
        let value = Self {
            filesystem_id: raw.filesystem_id,
            path_id: raw.path_id,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl DestinationIdentityV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_nonempty_bounded("destination.filesystem_id", &self.filesystem_id, 512)?;
        validate_nonempty_bounded("destination.path_id", &self.path_id, 1024)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReceiptV0 {
    pub schema: String,
    pub artifact: ArtifactIdentityV0,
    pub destination: DestinationIdentityV0,
    pub visible: bool,
    pub file_synced: bool,
    pub directory_synced: bool,
    pub idempotent_recovery: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPublicationReceiptV0 {
    schema: String,
    artifact: ArtifactIdentityV0,
    destination: DestinationIdentityV0,
    visible: bool,
    file_synced: bool,
    directory_synced: bool,
    idempotent_recovery: bool,
}

impl<'de> Deserialize<'de> for PublicationReceiptV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawPublicationReceiptV0::deserialize(deserializer)?;
        let value = Self {
            schema: raw.schema,
            artifact: raw.artifact,
            destination: raw.destination,
            visible: raw.visible,
            file_synced: raw.file_synced,
            directory_synced: raw.directory_synced,
            idempotent_recovery: raw.idempotent_recovery,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl PublicationReceiptV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != PUBLICATION_RECEIPT_SCHEMA_V0 {
            return Err(ContractError::Invalid { field: "schema" });
        }
        self.artifact.validate()?;
        self.destination.validate()?;
        if !self.visible {
            return Err(ContractError::Invalid { field: "visible" });
        }
        if self.directory_synced && !self.file_synced {
            return Err(ContractError::Inconsistent {
                field: "directory_synced",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicationOutcomeV0 {
    NotPublished {
        error: ServiceErrorV0,
    },
    Published {
        receipt: PublicationReceiptV0,
    },
    PublishedDurabilityUnknown {
        receipt: PublicationReceiptV0,
        error: ServiceErrorV0,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RawPublicationOutcomeV0 {
    NotPublished {
        error: ServiceErrorV0,
    },
    Published {
        receipt: PublicationReceiptV0,
    },
    PublishedDurabilityUnknown {
        receipt: PublicationReceiptV0,
        error: ServiceErrorV0,
    },
}

impl<'de> Deserialize<'de> for PublicationOutcomeV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawPublicationOutcomeV0::deserialize(deserializer)?;
        let value = match raw {
            RawPublicationOutcomeV0::NotPublished { error } => Self::NotPublished { error },
            RawPublicationOutcomeV0::Published { receipt } => Self::Published { receipt },
            RawPublicationOutcomeV0::PublishedDurabilityUnknown { receipt, error } => {
                Self::PublishedDurabilityUnknown { receipt, error }
            }
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl PublicationOutcomeV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::NotPublished { error } => error.validate(),
            Self::Published { receipt } => {
                receipt.validate()?;
                if !receipt.file_synced || !receipt.directory_synced {
                    return Err(ContractError::Inconsistent {
                        field: "published.receipt",
                    });
                }
                Ok(())
            }
            Self::PublishedDurabilityUnknown { receipt, error } => {
                receipt.validate()?;
                error.validate()?;
                if receipt.directory_synced {
                    return Err(ContractError::Inconsistent {
                        field: "published_durability_unknown.receipt",
                    });
                }
                if error.class != ErrorClass::Publication {
                    return Err(ContractError::Inconsistent {
                        field: "published_durability_unknown.error",
                    });
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{RetryAdvice, SERVICE_ERROR_SCHEMA_V0};
    use crate::ids::ServiceId;

    fn receipt(file_synced: bool, directory_synced: bool) -> PublicationReceiptV0 {
        PublicationReceiptV0 {
            schema: PUBLICATION_RECEIPT_SCHEMA_V0.to_owned(),
            artifact: ArtifactIdentityV0 {
                id: "artifact-1".to_owned(),
                digest: ContentDigest::sha256("a".repeat(64)).expect("digest"),
                length: 5,
            },
            destination: DestinationIdentityV0 {
                filesystem_id: "fs-1".to_owned(),
                path_id: "destination-1".to_owned(),
            },
            visible: true,
            file_synced,
            directory_synced,
            idempotent_recovery: false,
        }
    }

    fn publication_error() -> ServiceErrorV0 {
        ServiceErrorV0 {
            schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
            code: "publication.directory_sync".to_owned(),
            class: ErrorClass::Publication,
            retry: RetryAdvice::AfterRestart,
            operation_id: None,
            service: ServiceId::new("information").expect("service ID"),
            safe_detail: "the visible artifact has unknown durability".to_owned(),
        }
    }

    #[test]
    fn published_requires_both_sync_boundaries() {
        assert!(
            PublicationOutcomeV0::Published {
                receipt: receipt(true, true)
            }
            .validate()
            .is_ok()
        );
        assert!(
            PublicationOutcomeV0::Published {
                receipt: receipt(true, false)
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn durability_unknown_requires_visible_unsynced_directory() {
        assert!(
            PublicationOutcomeV0::PublishedDurabilityUnknown {
                receipt: receipt(true, false),
                error: publication_error(),
            }
            .validate()
            .is_ok()
        );
        assert!(
            PublicationOutcomeV0::PublishedDurabilityUnknown {
                receipt: receipt(true, true),
                error: publication_error(),
            }
            .validate()
            .is_err()
        );

        assert!(
            PublicationOutcomeV0::PublishedDurabilityUnknown {
                receipt: receipt(false, false),
                error: publication_error(),
            }
            .validate()
            .is_ok()
        );

        let mut invisible = receipt(true, false);
        invisible.visible = false;
        assert!(
            PublicationOutcomeV0::PublishedDurabilityUnknown {
                receipt: invisible,
                error: publication_error(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn durability_unknown_requires_publication_class_error() {
        let mut error = publication_error();
        error.class = ErrorClass::Storage;
        assert_eq!(
            PublicationOutcomeV0::PublishedDurabilityUnknown {
                receipt: receipt(true, false),
                error,
            }
            .validate(),
            Err(ContractError::Inconsistent {
                field: "published_durability_unknown.error"
            })
        );
    }

    #[test]
    fn outcome_tag_and_unknown_fields_are_strict() {
        let json = serde_json::json!({
            "kind": "not_published",
            "error": publication_error(),
            "visible": false
        });
        assert!(serde_json::from_value::<PublicationOutcomeV0>(json).is_err());
    }
}
