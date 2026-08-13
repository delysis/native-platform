//! Temporary conversion boundary for the canonical Wave 1 publication contract.
//!
//! This module is feature-gated because the contract repository is a temporary
//! pre-monorepo host. It converts product-owned receipts and outcomes without
//! carrying filesystem paths, file handles, or publication authority into the
//! shared envelope.

use crate::{AcquireError, PublicationReceipt, VerifiedFetch};
use information_native_types::ArtifactId;
use platform_contracts_v0::error::SERVICE_ERROR_SCHEMA_V0;
use platform_contracts_v0::publication::PUBLICATION_RECEIPT_SCHEMA_V0;
use platform_contracts_v0::{
    ArtifactIdentityV0, ContractError, DestinationIdentityV0, ErrorClass, PublicationOutcomeV0,
    PublicationReceiptV0, RetryAdvice, ServiceErrorV0, ServiceId,
};
use sha2::{Digest, Sha256};

const INFORMATION_ACQUIRE_SERVICE: &str = "information-acquire";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationContractContext {
    artifact_id: ArtifactId,
}

impl PublicationContractContext {
    pub const fn new(artifact_id: ArtifactId) -> Self {
        Self { artifact_id }
    }
}

pub fn publication_outcome_v0(
    result: Result<VerifiedFetch, AcquireError>,
    context: &PublicationContractContext,
) -> Result<PublicationOutcomeV0, ContractError> {
    let outcome = match result {
        Ok(fetch) if fetch.publication.directory_synced => PublicationOutcomeV0::Published {
            receipt: receipt_v0(&fetch.publication, context)?,
        },
        Ok(fetch) => PublicationOutcomeV0::PublishedDurabilityUnknown {
            receipt: receipt_v0(&fetch.publication, context)?,
            error: publication_error(
                "information.publish.directory_sync_unavailable",
                "verified bytes are visible but parent-directory durability is unavailable",
            ),
        },
        Err(AcquireError::PublishedDurabilityUnknown { receipt, .. }) => {
            PublicationOutcomeV0::PublishedDurabilityUnknown {
                receipt: receipt_v0(&receipt, context)?,
                error: publication_error(
                    "information.publish.durability_unknown",
                    "verified bytes are visible but publication durability is unknown",
                ),
            }
        }
        Err(error) => PublicationOutcomeV0::NotPublished {
            error: not_published_error(&error),
        },
    };
    outcome.validate()?;
    Ok(outcome)
}

fn receipt_v0(
    receipt: &PublicationReceipt,
    context: &PublicationContractContext,
) -> Result<PublicationReceiptV0, ContractError> {
    let attributed_artifact = receipt.artifact_id.as_ref().ok_or(ContractError::Invalid {
        field: "publication.artifact_id",
    })?;
    if attributed_artifact != &context.artifact_id {
        return Err(ContractError::Inconsistent {
            field: "publication.artifact_id",
        });
    }
    let receipt = PublicationReceiptV0 {
        schema: PUBLICATION_RECEIPT_SCHEMA_V0.to_owned(),
        artifact: ArtifactIdentityV0 {
            id: attributed_artifact.as_str().to_owned(),
            digest: platform_contracts_v0::ContentDigest::sha256(receipt.sha256.clone()).map_err(
                |_| ContractError::Invalid {
                    field: "artifact.digest",
                },
            )?,
            length: receipt.bytes,
        },
        destination: destination_identity_v0(receipt),
        visible: receipt.visible,
        file_synced: receipt.file_synced,
        directory_synced: receipt.directory_synced,
        idempotent_recovery: receipt.idempotent_recovery,
    };
    receipt.validate()?;
    Ok(receipt)
}

fn destination_identity_v0(receipt: &PublicationReceipt) -> DestinationIdentityV0 {
    match receipt.destination_identity {
        #[cfg(unix)]
        crate::PublicationDestinationIdentity::Unix { device, inode } => DestinationIdentityV0 {
            filesystem_id: format!("unix-device:{device}"),
            path_id: format!("unix-inode:{inode}"),
        },
        crate::PublicationDestinationIdentity::Unavailable => DestinationIdentityV0 {
            filesystem_id: "platform-filesystem-identity:unavailable".to_owned(),
            path_id: format!(
                "destination-path-sha256:{}",
                destination_path_digest(&receipt.destination)
            ),
        },
    }
}

fn destination_path_digest(path: &std::path::Path) -> String {
    let mut digest = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        digest.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            digest.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    digest.update(path.to_string_lossy().as_bytes());
    hex::encode(digest.finalize())
}

fn information_acquire_service() -> ServiceId {
    ServiceId::new(INFORMATION_ACQUIRE_SERVICE).expect("fixed service ID is valid")
}

fn publication_error(code: &str, safe_detail: &str) -> ServiceErrorV0 {
    ServiceErrorV0 {
        schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
        code: code.to_owned(),
        class: ErrorClass::Publication,
        retry: RetryAdvice::Immediate,
        operation_id: None,
        service: information_acquire_service(),
        safe_detail: safe_detail.to_owned(),
    }
}

fn not_published_error(error: &AcquireError) -> ServiceErrorV0 {
    let (code, class, retry, safe_detail) = match error {
        AcquireError::StagingPathExists => (
            "information.publish.destination_conflict",
            ErrorClass::Integrity,
            RetryAdvice::AfterUserAction,
            "the destination already contains different or unverifiable bytes",
        ),
        AcquireError::DigestMismatch { .. } | AcquireError::LengthMismatch { .. } => (
            "information.acquire.verification_failed",
            ErrorClass::Integrity,
            RetryAdvice::DifferentRoute,
            "acquired bytes did not match the declared artifact identity",
        ),
        AcquireError::Cancelled { .. } => (
            "information.acquire.cancelled",
            ErrorClass::Cancelled,
            RetryAdvice::Immediate,
            "acquisition was cancelled before publication",
        ),
        AcquireError::FileUriForbidden
        | AcquireError::FileOutsideGrantedRoots(_)
        | AcquireError::NetworkDestinationForbidden { .. } => (
            "information.acquire.permission_denied",
            ErrorClass::Permission,
            RetryAdvice::AfterUserAction,
            "the requested source was outside the caller-granted authority",
        ),
        _ => (
            "information.acquire.not_published",
            ErrorClass::Internal,
            RetryAdvice::AfterUserAction,
            "acquisition failed before verified bytes became visible",
        ),
    };
    ServiceErrorV0 {
        schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
        code: code.to_owned(),
        class,
        retry,
        operation_id: None,
        service: information_acquire_service(),
        safe_detail: safe_detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AcquireClient, AcquisitionPolicy, ArtifactFetchOptions, PublicationDestinationIdentity,
        ResumePolicy,
    };
    use information_native_types::{ArtifactId, PlannedArtifact};
    use platform_contracts_v0::PublicationOutcomeV0;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io;
    use tempfile::tempdir;

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn context() -> PublicationContractContext {
        PublicationContractContext::new(ArtifactId::parse("artifact.fixture").expect("artifact ID"))
    }

    fn synthetic_receipt(directory_synced: bool) -> PublicationReceipt {
        PublicationReceipt {
            artifact_id: Some(ArtifactId::parse("artifact.fixture").expect("artifact ID")),
            sha256: digest(b"payload"),
            bytes: 7,
            destination: "not-serialized".into(),
            destination_identity: PublicationDestinationIdentity::Unavailable,
            visible: true,
            file_synced: true,
            directory_synced,
            idempotent_recovery: false,
        }
    }

    #[test]
    fn real_success_and_exact_retry_map_to_canonical_published_receipts() {
        let directory = tempdir().expect("temporary directory");
        let source = directory.path().join("source.bin");
        let destination = directory.path().join("destination.bin");
        fs::write(&source, b"payload").expect("source bytes");
        let client = AcquireClient::with_defaults().expect("client");
        let mut policy = AcquisitionPolicy::restricted();
        policy
            .grant_file_root(directory.path())
            .expect("grant fixture directory");
        let options = ArtifactFetchOptions {
            acquisition_policy: policy,
            resume: ResumePolicy::Disabled,
        };
        let planned = PlannedArtifact {
            artifact_id: ArtifactId::parse("artifact.fixture").expect("artifact ID"),
            file_name: "destination.bin".to_owned(),
            source_uri: url::Url::from_file_path(&source)
                .expect("fixture file URI")
                .to_string(),
            expected_bytes: 7,
            sha256: digest(b"payload"),
        };

        let mut progress = |_progress| crate::ProgressControl::Continue;
        let first = client.fetch_planned_artifact_with_options(
            &planned,
            &destination,
            1024,
            &options,
            &mut progress,
        );
        let first = publication_outcome_v0(first, &context()).expect("canonical outcome");
        if cfg!(unix) {
            assert!(matches!(first, PublicationOutcomeV0::Published { .. }));
            if let PublicationOutcomeV0::Published { receipt } = &first {
                assert!(!receipt.idempotent_recovery);
            }
        } else {
            assert!(matches!(
                first,
                PublicationOutcomeV0::PublishedDurabilityUnknown { .. }
            ));
        }

        fs::remove_file(source).expect("remove source to prove destination recovery");
        let recovered = client.fetch_planned_artifact_with_options(
            &planned,
            &destination,
            1024,
            &options,
            &mut progress,
        );
        let recovered = publication_outcome_v0(recovered, &context()).expect("canonical recovery");
        assert!(!matches!(
            recovered,
            PublicationOutcomeV0::NotPublished { .. }
        ));
        if let PublicationOutcomeV0::Published { receipt }
        | PublicationOutcomeV0::PublishedDurabilityUnknown { receipt, .. } = &recovered
        {
            assert!(receipt.idempotent_recovery);
        }
        assert_eq!(fs::read(destination).expect("visible bytes"), b"payload");
    }

    #[test]
    fn conflict_and_verification_failure_map_to_not_published_without_path_leakage() {
        let directory = tempdir().expect("temporary directory");
        let source = directory.path().join("source.bin");
        let destination = directory.path().join("destination.bin");
        fs::write(&source, b"payload").expect("source bytes");
        fs::write(&destination, b"caller-owned").expect("caller bytes");
        let client = AcquireClient::with_defaults().expect("client");
        let outcome = publication_outcome_v0(
            client.fetch_file_artifact(&source, &destination, 7, &digest(b"payload"), 1024),
            &context(),
        )
        .expect("canonical conflict");
        let encoded = serde_json::to_string(&outcome).expect("serialize outcome");
        assert!(matches!(outcome, PublicationOutcomeV0::NotPublished { .. }));
        assert!(!encoded.contains(directory.path().to_string_lossy().as_ref()));
        assert_eq!(
            fs::read(destination).expect("caller bytes"),
            b"caller-owned"
        );

        let absent = directory.path().join("absent.bin");
        let outcome = publication_outcome_v0(
            client.fetch_file_artifact(&source, &absent, 7, &digest(b"different"), 1024),
            &context(),
        )
        .expect("canonical verification failure");
        assert!(matches!(outcome, PublicationOutcomeV0::NotPublished { .. }));
        assert!(!absent.exists());
    }

    #[test]
    fn visible_without_parent_durability_maps_to_the_third_outcome() {
        let receipt = synthetic_receipt(false);
        let source = io::Error::other("injected parent sync failure");
        let outcome = publication_outcome_v0(
            Err(AcquireError::PublishedDurabilityUnknown { receipt, source }),
            &context(),
        )
        .expect("canonical durability-unknown outcome");
        assert!(matches!(
            outcome,
            PublicationOutcomeV0::PublishedDurabilityUnknown { .. }
        ));
    }

    #[test]
    fn visible_receipts_require_product_attribution_and_reject_context_mismatch() {
        let mut receipt = synthetic_receipt(true);
        receipt.artifact_id = None;
        assert!(matches!(
            publication_outcome_v0(
                Ok(VerifiedFetch {
                    bytes: receipt.bytes,
                    sha256: receipt.sha256.clone(),
                    network_used: false,
                    final_source_uri: None,
                    redirects: 0,
                    source_attestation: None,
                    source_attestations: Vec::new(),
                    started_at_unix_ms: 0,
                    finished_at_unix_ms: 0,
                    resumed_bytes: 0,
                    publication: receipt.clone(),
                }),
                &context(),
            ),
            Err(ContractError::Invalid {
                field: "publication.artifact_id"
            })
        ));

        receipt.artifact_id = Some(ArtifactId::parse("different").expect("artifact ID"));
        assert!(matches!(
            receipt_v0(&receipt, &context()),
            Err(ContractError::Inconsistent {
                field: "publication.artifact_id"
            })
        ));
    }
}
