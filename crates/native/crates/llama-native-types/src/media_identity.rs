//! Bounded identity admission shared by document and conversation composition.
//!
//! This checks retained bytes, not codecs or authorization. Attachment inspection,
//! source grants and the selected model's modality gate remain independently
//! necessary. Deduplicating payload storage must not deduplicate source evidence.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{MediaInput, MediaKind};

pub const MAX_MEDIA_PAYLOADS: usize = 32;
pub const MAX_MEDIA_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_MEDIA_OCCURRENCES: usize = MAX_MEDIA_PAYLOADS * 257;
/// Include repeated bytes in the work budget even when their payload is shared.
/// Thirty-two references plus the initiating document can each carry the full
/// admitted byte budget. A larger expansion must be explicitly reduced.
pub const MAX_MEDIA_VERIFICATION_BYTES: u64 = MAX_MEDIA_BYTES as u64 * 33;
const MAX_METADATA_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaAdmission {
    NewPayload,
    DuplicatePayload,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MediaIdentityError {
    #[error("media identity or MIME metadata is empty, invalid or too long")]
    Metadata,
    #[error("media bytes do not match their canonical lowercase SHA-256")]
    Digest,
    #[error("one media occurrence ID names conflicting bytes or media types")]
    IdentityConflict,
    #[error("identical media payloads have conflicting MIME types")]
    MimeConflict,
    #[error("too many distinct media payloads")]
    PayloadLimit,
    #[error("media payload byte budget exceeded")]
    ByteLimit,
    #[error("media occurrence budget exceeded")]
    OccurrenceLimit,
    #[error("media verification work budget exceeded")]
    VerificationLimit,
}

impl MediaIdentityError {
    pub const fn is_limit(self) -> bool {
        matches!(
            self,
            Self::PayloadLimit | Self::ByteLimit | Self::OccurrenceLimit | Self::VerificationLimit
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Identity {
    kind: MediaKind,
    digest: String,
    mime: String,
}

/// Request-local bookkeeping, not a transferable validation capability. Native
/// admission still validates the final request after the caller takes its bytes.
#[derive(Debug, Default)]
pub struct MediaIdentityLedger {
    identities: BTreeMap<String, Identity>,
    payloads: BTreeMap<(MediaKind, String), String>,
    bytes: usize,
    occurrences: usize,
    verified_bytes: u64,
}

impl MediaIdentityLedger {
    /// Validate every occurrence before sharing its payload. On error none of
    /// the admitted identities or budgets changes; callers abort the request.
    pub fn admit(&mut self, item: &MediaInput) -> Result<MediaAdmission, MediaIdentityError> {
        if !valid_metadata(&item.id) || !valid_metadata(&item.mime) {
            return Err(MediaIdentityError::Metadata);
        }
        if self.occurrences >= MAX_MEDIA_OCCURRENCES {
            return Err(MediaIdentityError::OccurrenceLimit);
        }
        if item.bytes.len() > MAX_MEDIA_BYTES {
            return Err(MediaIdentityError::ByteLimit);
        }
        let verified_bytes = self
            .verified_bytes
            .checked_add(u64::try_from(item.bytes.len()).map_err(|_| MediaIdentityError::ByteLimit)?)
            .filter(|bytes| *bytes <= MAX_MEDIA_VERIFICATION_BYTES)
            .ok_or(MediaIdentityError::VerificationLimit)?;
        // A duplicate digest claim is not proof of equality. In particular,
        // neither repeated IDs nor an earlier good occurrence skip this hash.
        if item.sha256.len() != 64
            || !item.sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || format!("{:x}", Sha256::digest(&item.bytes)) != item.sha256
        {
            return Err(MediaIdentityError::Digest);
        }
        let identity = Identity {
            kind: item.kind,
            digest: item.sha256.clone(),
            mime: item.mime.clone(),
        };
        if self.identities.get(&item.id).is_some_and(|existing| existing != &identity) {
            return Err(MediaIdentityError::IdentityConflict);
        }
        let key = (item.kind, item.sha256.clone());
        let admission = match self.payloads.get(&key) {
            Some(mime) if mime != &item.mime => return Err(MediaIdentityError::MimeConflict),
            Some(_) => MediaAdmission::DuplicatePayload,
            None => MediaAdmission::NewPayload,
        };
        let bytes = if admission == MediaAdmission::NewPayload {
            if self.payloads.len() >= MAX_MEDIA_PAYLOADS {
                return Err(MediaIdentityError::PayloadLimit);
            }
            self.bytes.checked_add(item.bytes.len())
                .filter(|bytes| *bytes <= MAX_MEDIA_BYTES)
                .ok_or(MediaIdentityError::ByteLimit)?
        } else {
            self.bytes
        };
        self.identities.insert(item.id.clone(), identity);
        self.payloads.entry(key).or_insert_with(|| item.mime.clone());
        self.bytes = bytes;
        self.occurrences += 1;
        self.verified_bytes = verified_bytes;
        Ok(admission)
    }

    pub fn payload_count(&self) -> usize { self.payloads.len() }
    pub const fn byte_len(&self) -> usize { self.bytes }
    pub const fn occurrence_count(&self) -> usize { self.occurrences }
}

fn valid_metadata(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_METADATA_BYTES
        && !value.chars().any(char::is_control)
        && value.trim() == value
}

#[cfg(test)]
mod tests {
    use super::*;

    // These bytes test identity only; they are not asserted to be decoded media.
    fn item(id: &str, bytes: &[u8]) -> MediaInput {
        MediaInput {
            id: id.into(), kind: MediaKind::Image, mime: "image/png".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)), bytes: bytes.to_vec(),
        }
    }
    fn counts(ledger: &MediaIdentityLedger) -> (usize, usize, usize, u64) {
        (ledger.payload_count(), ledger.byte_len(), ledger.occurrence_count(), ledger.verified_bytes)
    }

    #[test]
    fn sharing_counts_every_occurrence_but_only_one_payload() {
        let mut ledger = MediaIdentityLedger::default();
        assert_eq!(ledger.admit(&item("first", b"same")), Ok(MediaAdmission::NewPayload));
        assert_eq!(ledger.admit(&item("second", b"same")), Ok(MediaAdmission::DuplicatePayload));
        assert_eq!(ledger.admit(&item("first", b"same")), Ok(MediaAdmission::DuplicatePayload));
        assert_eq!(counts(&ledger), (1, 4, 3, 12));
    }

    #[test]
    fn neither_repeated_ids_nor_new_ids_hide_corruption() {
        for id in ["first", "second"] {
            let mut ledger = MediaIdentityLedger::default();
            let first = item("first", b"original");
            ledger.admit(&first).expect("original");
            let before = counts(&ledger);
            let mut corrupt = first.clone();
            corrupt.id = id.into();
            corrupt.bytes = b"changed!".to_vec();
            assert_eq!(ledger.admit(&corrupt), Err(MediaIdentityError::Digest));
            assert_eq!(counts(&ledger), before);
        }
    }

    #[test]
    fn conflicting_ids_and_mime_are_atomic_errors() {
        let mut ledger = MediaIdentityLedger::default();
        ledger.admit(&item("first", b"original")).expect("original");
        let before = counts(&ledger);
        assert_eq!(ledger.admit(&item("first", b"changed")), Err(MediaIdentityError::IdentityConflict));
        let mut mime = item("second", b"original");
        mime.mime = "image/jpeg".into();
        assert_eq!(ledger.admit(&mime), Err(MediaIdentityError::MimeConflict));
        assert_eq!(counts(&ledger), before);
    }

    #[test]
    fn different_modalities_do_not_share_a_payload() {
        let mut ledger = MediaIdentityLedger::default();
        ledger.admit(&item("image", b"bytes")).expect("image identity");
        let mut audio = item("audio", b"bytes");
        audio.kind = MediaKind::Audio;
        audio.mime = "audio/wav".into();
        assert_eq!(ledger.admit(&audio), Ok(MediaAdmission::NewPayload));
        assert_eq!(ledger.payload_count(), 2);
    }

    #[test]
    fn metadata_and_digest_spelling_are_not_normalized() {
        for field in ["id", "mime", "digest"] {
            let mut value = item("source", b"bytes");
            match field {
                "id" => value.id = "bad\nsource".into(),
                "mime" => value.mime = "image/png ".into(),
                _ => value.sha256 = value.sha256.to_uppercase(),
            }
            let mut ledger = MediaIdentityLedger::default();
            assert!(ledger.admit(&value).is_err());
            assert_eq!(counts(&ledger), (0, 0, 0, 0));
        }
    }

    #[test]
    fn repeated_occurrences_cannot_bypass_work_limits() {
        let value = item("same", b"x");
        let mut ledger = MediaIdentityLedger::default();
        for _ in 0..MAX_MEDIA_OCCURRENCES { ledger.admit(&value).expect("within cap"); }
        let before = counts(&ledger);
        assert_eq!(ledger.admit(&value), Err(MediaIdentityError::OccurrenceLimit));
        assert_eq!(counts(&ledger), before);
        let mut ledger = MediaIdentityLedger {
            verified_bytes: MAX_MEDIA_VERIFICATION_BYTES, ..MediaIdentityLedger::default()
        };
        assert_eq!(ledger.admit(&value), Err(MediaIdentityError::VerificationLimit));
    }

    #[test]
    fn payload_and_aggregate_byte_limits_remain_independent() {
        let mut ledger = MediaIdentityLedger::default();
        for index in 0..MAX_MEDIA_PAYLOADS {
            ledger.admit(&item(&index.to_string(), &index.to_le_bytes())).expect("within cap");
        }
        assert_eq!(ledger.admit(&item("extra", b"extra")), Err(MediaIdentityError::PayloadLimit));
        let mut ledger = MediaIdentityLedger { bytes: MAX_MEDIA_BYTES, ..MediaIdentityLedger::default() };
        assert_eq!(ledger.admit(&item("extra", b"x")), Err(MediaIdentityError::ByteLimit));
        assert_eq!(ledger.payload_count(), 0);
    }
}
