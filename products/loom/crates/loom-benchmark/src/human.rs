//! Diagnostic encrypted human-label archives and a closed confirmation gate.
//!
//! This crate does not implement cryptography. Ciphertext can be archived and
//! replayed, but it cannot confirm a profile. A future crypto/adjudication
//! verifier must issue the private move-only confirmation lease.

use std::collections::BTreeSet;

use loom_research_types::{BoundError, BoundedVec, NonEmptyBoundedVec};
use loom_types::BlobId;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::FrontierReviewedProvisionalProfile;

pub const MAX_ENCRYPTED_HUMAN_LABEL_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ENCRYPTION_NONCE_BYTES: usize = 64;
pub const MAX_HUMAN_REVIEWER_GROUPS: usize = 64;
pub const MIN_HUMAN_REVIEWER_GROUPS: usize = 3;

const ENVELOPE_FINGERPRINT_DOMAIN: &[u8] = b"loom/encrypted-human-label-archive/v2\0";
const ADJUDICATION_FINGERPRINT_DOMAIN: &[u8] = b"loom/human-adjudication-archive/v1\0";
const CONFIRMED_PROFILE_DOMAIN: &[u8] = b"loom/human-confirmed-profile/v2\0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanLabelEncryptionAlgorithm {
    XChaCha20Poly1305V1,
    AgeX25519V1,
}

impl HumanLabelEncryptionAlgorithm {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::XChaCha20Poly1305V1 => 0,
            Self::AgeX25519V1 => 1,
        }
    }
}

/// Opaque diagnostic ciphertext. Structural validation is not decryption or
/// authentication and grants no human-review authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EncryptedHumanLabelArchive {
    seal_fingerprint: BlobId,
    label_schema_fingerprint: BlobId,
    key_id_fingerprint: BlobId,
    associated_data_fingerprint: BlobId,
    algorithm: HumanLabelEncryptionAlgorithm,
    nonce: BoundedVec<u8, MAX_ENCRYPTION_NONCE_BYTES>,
    ciphertext: BoundedVec<u8, MAX_ENCRYPTED_HUMAN_LABEL_BYTES>,
    fingerprint: BlobId,
}

impl EncryptedHumanLabelArchive {
    pub fn archive(
        seal_fingerprint: BlobId,
        label_schema_fingerprint: BlobId,
        key_id_fingerprint: BlobId,
        associated_data_fingerprint: BlobId,
        algorithm: HumanLabelEncryptionAlgorithm,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    ) -> Result<Self, HumanConfirmationError> {
        validate_ciphertext_shape(algorithm, &nonce, &ciphertext)?;
        let nonce = BoundedVec::new(nonce)?;
        let ciphertext = BoundedVec::new(ciphertext)?;
        let mut archive = Self {
            seal_fingerprint,
            label_schema_fingerprint,
            key_id_fingerprint,
            associated_data_fingerprint,
            algorithm,
            nonce,
            ciphertext,
            fingerprint: BlobId::digest(&[]),
        };
        archive.fingerprint = archive.compute_fingerprint();
        Ok(archive)
    }

    pub const fn seal_fingerprint(&self) -> BlobId {
        self.seal_fingerprint
    }

    pub const fn label_schema_fingerprint(&self) -> BlobId {
        self.label_schema_fingerprint
    }

    pub const fn key_id_fingerprint(&self) -> BlobId {
        self.key_id_fingerprint
    }

    pub const fn associated_data_fingerprint(&self) -> BlobId {
        self.associated_data_fingerprint
    }

    pub const fn algorithm(&self) -> HumanLabelEncryptionAlgorithm {
        self.algorithm
    }

    pub fn nonce(&self) -> &[u8] {
        &self.nonce
    }

    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn validate_internal(&self) -> Result<(), HumanConfirmationError> {
        validate_ciphertext_shape(self.algorithm, &self.nonce, &self.ciphertext)?;
        if self.compute_fingerprint() != self.fingerprint {
            return Err(HumanConfirmationError::EnvelopeFingerprintMismatch);
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(ENVELOPE_FINGERPRINT_DOMAIN);
        digest.update(self.seal_fingerprint.as_bytes());
        digest.update(self.label_schema_fingerprint.as_bytes());
        digest.update(self.key_id_fingerprint.as_bytes());
        digest.update(self.associated_data_fingerprint.as_bytes());
        digest.update([self.algorithm.domain_tag()]);
        digest.update((self.nonce.len() as u64).to_be_bytes());
        digest.update(&*self.nonce);
        digest.update((self.ciphertext.len() as u64).to_be_bytes());
        digest.update(&*self.ciphertext);
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncryptedHumanLabelArchiveWire {
    seal_fingerprint: BlobId,
    label_schema_fingerprint: BlobId,
    key_id_fingerprint: BlobId,
    associated_data_fingerprint: BlobId,
    algorithm: HumanLabelEncryptionAlgorithm,
    nonce: BoundedVec<u8, MAX_ENCRYPTION_NONCE_BYTES>,
    ciphertext: BoundedVec<u8, MAX_ENCRYPTED_HUMAN_LABEL_BYTES>,
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for EncryptedHumanLabelArchive {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EncryptedHumanLabelArchiveWire::deserialize(deserializer)?;
        let archive = Self {
            seal_fingerprint: wire.seal_fingerprint,
            label_schema_fingerprint: wire.label_schema_fingerprint,
            key_id_fingerprint: wire.key_id_fingerprint,
            associated_data_fingerprint: wire.associated_data_fingerprint,
            algorithm: wire.algorithm,
            nonce: wire.nonce,
            ciphertext: wire.ciphertext,
            fingerprint: wire.fingerprint,
        };
        archive
            .validate_internal()
            .map_err(serde::de::Error::custom)?;
        Ok(archive)
    }
}

/// Persisted human-review metadata. It remains diagnostic until its exact
/// ciphertext and reviewer receipts are cryptographically/adjudicatively
/// verified.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HumanAdjudicationArchiveRecord {
    seal_fingerprint: BlobId,
    finalist_selection_fingerprint: BlobId,
    provisional_profile_fingerprint: BlobId,
    encrypted_label_archive_fingerprint: BlobId,
    review_protocol_fingerprint: BlobId,
    reviewer_group_fingerprints: NonEmptyBoundedVec<BlobId, MAX_HUMAN_REVIEWER_GROUPS>,
    adjudication_receipt_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl HumanAdjudicationArchiveRecord {
    pub fn archive(
        seal_fingerprint: BlobId,
        finalist_selection_fingerprint: BlobId,
        provisional_profile_fingerprint: BlobId,
        encrypted_label_archive_fingerprint: BlobId,
        review_protocol_fingerprint: BlobId,
        reviewer_group_fingerprints: Vec<BlobId>,
        adjudication_receipt_fingerprint: BlobId,
    ) -> Result<Self, HumanConfirmationError> {
        let reviewer_group_fingerprints = validate_reviewer_groups(reviewer_group_fingerprints)?;
        let mut record = Self {
            seal_fingerprint,
            finalist_selection_fingerprint,
            provisional_profile_fingerprint,
            encrypted_label_archive_fingerprint,
            review_protocol_fingerprint,
            reviewer_group_fingerprints,
            adjudication_receipt_fingerprint,
            fingerprint: BlobId::digest(&[]),
        };
        record.fingerprint = record.compute_fingerprint();
        Ok(record)
    }

    pub const fn provisional_profile_fingerprint(&self) -> BlobId {
        self.provisional_profile_fingerprint
    }

    pub const fn seal_fingerprint(&self) -> BlobId {
        self.seal_fingerprint
    }

    pub const fn finalist_selection_fingerprint(&self) -> BlobId {
        self.finalist_selection_fingerprint
    }

    pub const fn encrypted_label_archive_fingerprint(&self) -> BlobId {
        self.encrypted_label_archive_fingerprint
    }

    pub fn reviewer_group_fingerprints(&self) -> &[BlobId] {
        &self.reviewer_group_fingerprints
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn validate_internal(&self) -> Result<(), HumanConfirmationError> {
        validate_reviewer_groups(self.reviewer_group_fingerprints.clone().into_inner())?;
        if self.compute_fingerprint() != self.fingerprint {
            return Err(HumanConfirmationError::AdjudicationFingerprintMismatch);
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(ADJUDICATION_FINGERPRINT_DOMAIN);
        digest.update(self.seal_fingerprint.as_bytes());
        digest.update(self.finalist_selection_fingerprint.as_bytes());
        digest.update(self.provisional_profile_fingerprint.as_bytes());
        digest.update(self.encrypted_label_archive_fingerprint.as_bytes());
        digest.update(self.review_protocol_fingerprint.as_bytes());
        digest.update((self.reviewer_group_fingerprints.len() as u64).to_be_bytes());
        for group in self.reviewer_group_fingerprints.iter() {
            digest.update(group.as_bytes());
        }
        digest.update(self.adjudication_receipt_fingerprint.as_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanAdjudicationArchiveRecordWire {
    seal_fingerprint: BlobId,
    finalist_selection_fingerprint: BlobId,
    provisional_profile_fingerprint: BlobId,
    encrypted_label_archive_fingerprint: BlobId,
    review_protocol_fingerprint: BlobId,
    reviewer_group_fingerprints: NonEmptyBoundedVec<BlobId, MAX_HUMAN_REVIEWER_GROUPS>,
    adjudication_receipt_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for HumanAdjudicationArchiveRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = HumanAdjudicationArchiveRecordWire::deserialize(deserializer)?;
        let record = Self {
            seal_fingerprint: wire.seal_fingerprint,
            finalist_selection_fingerprint: wire.finalist_selection_fingerprint,
            provisional_profile_fingerprint: wire.provisional_profile_fingerprint,
            encrypted_label_archive_fingerprint: wire.encrypted_label_archive_fingerprint,
            review_protocol_fingerprint: wire.review_protocol_fingerprint,
            reviewer_group_fingerprints: wire.reviewer_group_fingerprints,
            adjudication_receipt_fingerprint: wire.adjudication_receipt_fingerprint,
            fingerprint: wire.fingerprint,
        };
        record
            .validate_internal()
            .map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

/// Move-only output of the future decryption, nonce/key/AAD, reviewer-session,
/// packet-coverage, and adjudication verifier. No production constructor is
/// intentionally available.
///
/// ```compile_fail
/// use loom_benchmark::VerifiedHumanConfirmationLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<VerifiedHumanConfirmationLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedHumanConfirmationLease {
    provisional_profile: BlobId,
    benchmark_seal: BlobId,
    finalist_selection: BlobId,
    encrypted_label_archive: BlobId,
    adjudication_archive: BlobId,
    crypto_verifier_receipt: BlobId,
    authority: HumanConfirmationAuthority,
}

#[derive(Debug)]
struct HumanConfirmationAuthority;

impl VerifiedHumanConfirmationLease {
    #[cfg(test)]
    pub(crate) fn for_test(
        provisional: &FrontierReviewedProvisionalProfile,
        envelope: &EncryptedHumanLabelArchive,
        adjudication: &HumanAdjudicationArchiveRecord,
    ) -> Self {
        Self {
            provisional_profile: provisional.fingerprint(),
            benchmark_seal: provisional.seal_fingerprint(),
            finalist_selection: provisional.finalist_selection_fingerprint(),
            encrypted_label_archive: envelope.fingerprint(),
            adjudication_archive: adjudication.fingerprint(),
            crypto_verifier_receipt: BlobId::digest(b"test-crypto-verifier"),
            authority: HumanConfirmationAuthority,
        }
    }
}

/// Human-confirmed artifact. Its only constructor consumes an unforgeable
/// verifier lease; ciphertext or a self-hashed archive is insufficient.
///
/// ```compile_fail
/// use loom_benchmark::HumanConfirmedProfile;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<HumanConfirmedProfile>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HumanConfirmedProfile {
    provisional: FrontierReviewedProvisionalProfile,
    benchmark_seal_fingerprint: BlobId,
    encrypted_label_archive_fingerprint: BlobId,
    adjudication_archive_fingerprint: BlobId,
    crypto_verifier_receipt_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl HumanConfirmedProfile {
    pub fn from_verified(
        provisional: FrontierReviewedProvisionalProfile,
        envelope: &EncryptedHumanLabelArchive,
        adjudication: &HumanAdjudicationArchiveRecord,
        lease: VerifiedHumanConfirmationLease,
    ) -> Result<Self, HumanConfirmationError> {
        envelope.validate_internal()?;
        adjudication.validate_internal()?;
        let VerifiedHumanConfirmationLease {
            provisional_profile,
            benchmark_seal,
            finalist_selection,
            encrypted_label_archive,
            adjudication_archive,
            crypto_verifier_receipt,
            authority: _authority,
        } = lease;
        if provisional_profile != provisional.fingerprint()
            || benchmark_seal != provisional.seal_fingerprint()
            || finalist_selection != provisional.finalist_selection_fingerprint()
            || encrypted_label_archive != envelope.fingerprint()
            || adjudication_archive != adjudication.fingerprint()
            || envelope.seal_fingerprint() != provisional.seal_fingerprint()
            || adjudication.provisional_profile_fingerprint != provisional.fingerprint()
            || adjudication.finalist_selection_fingerprint
                != provisional.finalist_selection_fingerprint()
            || adjudication.encrypted_label_archive_fingerprint != envelope.fingerprint()
            || adjudication.seal_fingerprint != provisional.seal_fingerprint()
        {
            return Err(HumanConfirmationError::ConfirmationLeaseMismatch);
        }
        let fingerprint = fingerprint_confirmed_profile(
            provisional.fingerprint(),
            provisional.seal_fingerprint(),
            envelope.fingerprint(),
            adjudication.fingerprint(),
            crypto_verifier_receipt,
        );
        let confirmed = Self {
            benchmark_seal_fingerprint: provisional.seal_fingerprint(),
            encrypted_label_archive_fingerprint: envelope.fingerprint(),
            provisional,
            adjudication_archive_fingerprint: adjudication.fingerprint(),
            crypto_verifier_receipt_fingerprint: crypto_verifier_receipt,
            fingerprint,
        };
        confirmed.validate_confirmation_evidence(envelope, adjudication)?;
        Ok(confirmed)
    }

    pub const fn provisional(&self) -> &FrontierReviewedProvisionalProfile {
        &self.provisional
    }

    /// Fingerprint of the reusable candidate profile, not this confirmation
    /// artifact. Use [`Self::fingerprint`] for the latter.
    pub const fn profile_fingerprint(&self) -> BlobId {
        self.provisional.candidate().fingerprint()
    }

    pub const fn benchmark_seal_fingerprint(&self) -> BlobId {
        self.benchmark_seal_fingerprint
    }

    pub const fn encrypted_label_archive_fingerprint(&self) -> BlobId {
        self.encrypted_label_archive_fingerprint
    }

    pub const fn adjudication_archive_fingerprint(&self) -> BlobId {
        self.adjudication_archive_fingerprint
    }

    pub fn validate_confirmation_evidence(
        &self,
        envelope: &EncryptedHumanLabelArchive,
        adjudication: &HumanAdjudicationArchiveRecord,
    ) -> Result<(), HumanConfirmationError> {
        envelope.validate_internal()?;
        adjudication.validate_internal()?;
        let exact = self.benchmark_seal_fingerprint == self.provisional.seal_fingerprint()
            && envelope.seal_fingerprint() == self.benchmark_seal_fingerprint
            && envelope.fingerprint() == self.encrypted_label_archive_fingerprint
            && adjudication.seal_fingerprint() == self.benchmark_seal_fingerprint
            && adjudication.finalist_selection_fingerprint()
                == self.provisional.finalist_selection_fingerprint()
            && adjudication.provisional_profile_fingerprint() == self.provisional.fingerprint()
            && adjudication.encrypted_label_archive_fingerprint() == envelope.fingerprint()
            && adjudication.fingerprint() == self.adjudication_archive_fingerprint
            && fingerprint_confirmed_profile(
                self.provisional.fingerprint(),
                self.benchmark_seal_fingerprint,
                self.encrypted_label_archive_fingerprint,
                self.adjudication_archive_fingerprint,
                self.crypto_verifier_receipt_fingerprint,
            ) == self.fingerprint;
        if !exact {
            return Err(HumanConfirmationError::ConfirmationEvidenceMismatch);
        }
        Ok(())
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum HumanConfirmationError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("XChaCha20-Poly1305 requires an exact 24-byte nonce")]
    InvalidXChaChaNonce,
    #[error("Age archives carry their own envelope and require an empty detached nonce")]
    InvalidAgeNonce,
    #[error("encrypted-label ciphertext is structurally too short")]
    CiphertextTooShort,
    #[error("encrypted-label archive fingerprint mismatch")]
    EnvelopeFingerprintMismatch,
    #[error("human adjudication requires at least three unique reviewer groups")]
    InsufficientReviewerGroups,
    #[error("human adjudication repeats a reviewer group")]
    DuplicateReviewerGroup,
    #[error("human adjudication archive fingerprint mismatch")]
    AdjudicationFingerprintMismatch,
    #[error("crypto/adjudication verifier lease does not match the exact finalist archives")]
    ConfirmationLeaseMismatch,
    #[error("human-confirmed profile does not match its exact encrypted/adjudicated evidence")]
    ConfirmationEvidenceMismatch,
}

fn validate_ciphertext_shape(
    algorithm: HumanLabelEncryptionAlgorithm,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<(), HumanConfirmationError> {
    match algorithm {
        HumanLabelEncryptionAlgorithm::XChaCha20Poly1305V1 => {
            if nonce.len() != 24 {
                return Err(HumanConfirmationError::InvalidXChaChaNonce);
            }
            if ciphertext.len() < 16 {
                return Err(HumanConfirmationError::CiphertextTooShort);
            }
        }
        HumanLabelEncryptionAlgorithm::AgeX25519V1 => {
            if !nonce.is_empty() {
                return Err(HumanConfirmationError::InvalidAgeNonce);
            }
            if ciphertext.is_empty() {
                return Err(HumanConfirmationError::CiphertextTooShort);
            }
        }
    }
    Ok(())
}

fn validate_reviewer_groups(
    groups: Vec<BlobId>,
) -> Result<NonEmptyBoundedVec<BlobId, MAX_HUMAN_REVIEWER_GROUPS>, HumanConfirmationError> {
    if groups.len() < MIN_HUMAN_REVIEWER_GROUPS {
        return Err(HumanConfirmationError::InsufficientReviewerGroups);
    }
    let groups = NonEmptyBoundedVec::new(groups)?;
    let mut unique = BTreeSet::new();
    if groups.iter().any(|group| !unique.insert(*group)) {
        return Err(HumanConfirmationError::DuplicateReviewerGroup);
    }
    Ok(groups)
}

fn fingerprint_confirmed_profile(
    provisional: BlobId,
    benchmark_seal: BlobId,
    encrypted_labels: BlobId,
    adjudication: BlobId,
    crypto_verifier: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(CONFIRMED_PROFILE_DOMAIN);
    digest.update(provisional.as_bytes());
    digest.update(benchmark_seal.as_bytes());
    digest.update(encrypted_labels.as_bytes());
    digest.update(adjudication.as_bytes());
    digest.update(crypto_verifier.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}
