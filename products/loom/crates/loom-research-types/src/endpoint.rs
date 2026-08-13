//! Bounded paragraph-endpoint extraction contracts.

use loom_types::BlobId;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{BoundError, BoundedVec, MAX_RAW_OUTPUT_BYTES, NonEmptyByteRange, RangeError};

pub const MAX_ENDPOINT_CANDIDATES: usize = 4_096;

const ENDPOINT_FINGERPRINT_DOMAIN: &[u8] = b"loom/paragraph-endpoint-extraction/v1\0";
const SELECTION_FINGERPRINT_DOMAIN: &[u8] = b"loom/paragraph-endpoint-selection/v1\0";

/// Exact suffix removed because a declared stop rule matched it.
///
/// The raw completion itself is never changed; the suffix remains bound by
/// its byte range and blob digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TrimmedStopSuffixWitness {
    range: NonEmptyByteRange,
    suffix_blob_id: BlobId,
    stop_rule_fingerprint: BlobId,
}

impl TrimmedStopSuffixWitness {
    pub fn from_raw(
        raw_output: &[u8],
        range: NonEmptyByteRange,
        stop_rule_fingerprint: BlobId,
    ) -> Result<Self, EndpointError> {
        validate_raw_output(raw_output)?;
        if range.end() != raw_output.len() as u64 {
            return Err(EndpointError::StopSuffixNotAtOutputEnd);
        }
        let exact_suffix = range.checked_str(raw_output)?.as_bytes();
        Ok(Self {
            range,
            suffix_blob_id: BlobId::digest(exact_suffix),
            stop_rule_fingerprint,
        })
    }

    pub const fn range(self) -> NonEmptyByteRange {
        self.range
    }

    pub const fn suffix_blob_id(self) -> BlobId {
        self.suffix_blob_id
    }

    pub const fn stop_rule_fingerprint(self) -> BlobId {
        self.stop_rule_fingerprint
    }

    fn verify(self, raw_output: &[u8]) -> Result<(), EndpointError> {
        if self.range.end() != raw_output.len() as u64 {
            return Err(EndpointError::StopSuffixNotAtOutputEnd);
        }
        let exact_suffix = self.range.checked_str(raw_output)?.as_bytes();
        if BlobId::digest(exact_suffix) != self.suffix_blob_id {
            return Err(EndpointError::StopSuffixBlobMismatch);
        }
        Ok(())
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.range.start().to_be_bytes());
        digest.update(self.range.end().to_be_bytes());
        digest.update(self.suffix_blob_id.as_bytes());
        digest.update(self.stop_rule_fingerprint.as_bytes());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrimmedStopSuffixWitnessWire {
    range: NonEmptyByteRange,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    suffix_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    stop_rule_fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for TrimmedStopSuffixWitness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TrimmedStopSuffixWitnessWire::deserialize(deserializer)?;
        Ok(Self {
            range: wire.range,
            suffix_blob_id: wire.suffix_blob_id,
            stop_rule_fingerprint: wire.stop_rule_fingerprint,
        })
    }
}

/// Exact syntax witnessing that a prefix ends at a paragraph boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum ParagraphBoundaryWitness {
    /// A blank physical line immediately follows the selected paragraph.
    BlankLine { separator_range: NonEmptyByteRange },
    /// The eligible output ends in one or more line terminators after content.
    TerminalLineBreak { separator_range: NonEmptyByteRange },
}

impl ParagraphBoundaryWitness {
    pub const fn separator_range(self) -> NonEmptyByteRange {
        match self {
            Self::BlankLine { separator_range } | Self::TerminalLineBreak { separator_range } => {
                separator_range
            }
        }
    }

    const fn domain_tag(self) -> u8 {
        match self {
            Self::BlankLine { .. } => 0,
            Self::TerminalLineBreak { .. } => 1,
        }
    }
}

/// One exact raw-output prefix eligible for admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ParagraphEndpointCandidate {
    raw_prefix_range: NonEmptyByteRange,
    boundary: ParagraphBoundaryWitness,
}

impl ParagraphEndpointCandidate {
    fn new(
        raw_prefix_range: NonEmptyByteRange,
        boundary: ParagraphBoundaryWitness,
        eligible_end: u64,
        max_prefix_bytes: u64,
    ) -> Result<Self, EndpointError> {
        if raw_prefix_range.start() != 0 {
            return Err(EndpointError::EndpointIsNotPrefix);
        }
        if raw_prefix_range.end() > max_prefix_bytes {
            return Err(EndpointError::EndpointExceedsMaximum);
        }
        let separator = boundary.separator_range();
        if separator.start() != raw_prefix_range.end() || separator.end() > eligible_end {
            return Err(EndpointError::InvalidBoundaryGeometry);
        }
        if matches!(boundary, ParagraphBoundaryWitness::TerminalLineBreak { .. })
            && separator.end() != eligible_end
        {
            return Err(EndpointError::InvalidBoundaryGeometry);
        }
        Ok(Self {
            raw_prefix_range,
            boundary,
        })
    }

    pub const fn raw_prefix_range(self) -> NonEmptyByteRange {
        self.raw_prefix_range
    }

    pub const fn boundary(self) -> ParagraphBoundaryWitness {
        self.boundary
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.raw_prefix_range.start().to_be_bytes());
        digest.update(self.raw_prefix_range.end().to_be_bytes());
        digest.update([self.boundary.domain_tag()]);
        let separator = self.boundary.separator_range();
        digest.update(separator.start().to_be_bytes());
        digest.update(separator.end().to_be_bytes());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParagraphEndpointCandidateWire {
    raw_prefix_range: NonEmptyByteRange,
    boundary: ParagraphBoundaryWitness,
}

/// A replayable scan of one preserved raw completion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ParagraphEndpointExtraction {
    raw_output_blob_id: BlobId,
    raw_output_len: u64,
    max_prefix_bytes: u64,
    trimmed_stop_suffix: Option<TrimmedStopSuffixWitness>,
    candidates: BoundedVec<ParagraphEndpointCandidate, MAX_ENDPOINT_CANDIDATES>,
    fingerprint: BlobId,
}

impl ParagraphEndpointExtraction {
    pub fn extract(
        raw_output: &[u8],
        max_prefix_bytes: u64,
        trimmed_stop_suffix: Option<TrimmedStopSuffixWitness>,
    ) -> Result<Self, EndpointError> {
        validate_raw_output(raw_output)?;
        if max_prefix_bytes == 0 || max_prefix_bytes > MAX_RAW_OUTPUT_BYTES {
            return Err(EndpointError::InvalidMaximum {
                actual: max_prefix_bytes,
                maximum: MAX_RAW_OUTPUT_BYTES,
            });
        }
        if let Some(witness) = trimmed_stop_suffix {
            witness.verify(raw_output)?;
        }
        let eligible_end =
            trimmed_stop_suffix.map_or(raw_output.len() as u64, |suffix| suffix.range().start());
        let eligible_end_usize =
            usize::try_from(eligible_end).map_err(|_| EndpointError::RawOutputTooLarge)?;
        let candidates =
            scan_paragraph_boundaries(&raw_output[..eligible_end_usize], max_prefix_bytes)?;
        let candidates = BoundedVec::new(candidates)?;
        let mut extraction = Self {
            raw_output_blob_id: BlobId::digest(raw_output),
            raw_output_len: raw_output.len() as u64,
            max_prefix_bytes,
            trimmed_stop_suffix,
            candidates,
            fingerprint: BlobId::digest(&[]),
        };
        extraction.fingerprint = extraction.compute_fingerprint();
        Ok(extraction)
    }

    pub const fn raw_output_blob_id(&self) -> BlobId {
        self.raw_output_blob_id
    }

    pub const fn raw_output_len(&self) -> u64 {
        self.raw_output_len
    }

    pub const fn max_prefix_bytes(&self) -> u64 {
        self.max_prefix_bytes
    }

    pub const fn trimmed_stop_suffix(&self) -> Option<TrimmedStopSuffixWitness> {
        self.trimmed_stop_suffix
    }

    pub fn candidates(&self) -> &[ParagraphEndpointCandidate] {
        &self.candidates
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    /// Strictly regenerates every candidate from the preserved raw bytes.
    pub fn replay_against_raw(&self, raw_output: &[u8]) -> Result<(), EndpointError> {
        let replayed = Self::extract(raw_output, self.max_prefix_bytes, self.trimmed_stop_suffix)?;
        if replayed != *self {
            return Err(EndpointError::ReplayMismatch);
        }
        Ok(())
    }

    pub fn select(self, candidate_index: usize) -> Result<EndpointSelection, EndpointError> {
        let candidate = self.candidates.get(candidate_index).copied().ok_or(
            EndpointError::CandidateIndexOutOfBounds {
                index: candidate_index,
                count: self.candidates.len(),
            },
        )?;
        let fingerprint = fingerprint_selection(self.fingerprint, candidate_index, candidate);
        Ok(EndpointSelection {
            extraction: self,
            candidate_index,
            candidate,
            fingerprint,
        })
    }

    fn from_wire(wire: ParagraphEndpointExtractionWire) -> Result<Self, EndpointError> {
        if wire.raw_output_len == 0 || wire.raw_output_len > MAX_RAW_OUTPUT_BYTES {
            return Err(EndpointError::RawOutputTooLarge);
        }
        if wire.max_prefix_bytes == 0 || wire.max_prefix_bytes > MAX_RAW_OUTPUT_BYTES {
            return Err(EndpointError::InvalidMaximum {
                actual: wire.max_prefix_bytes,
                maximum: MAX_RAW_OUTPUT_BYTES,
            });
        }
        let eligible_end = wire
            .trimmed_stop_suffix
            .map_or(wire.raw_output_len, |suffix| {
                if suffix.range().end() != wire.raw_output_len {
                    return 0;
                }
                suffix.range().start()
            });
        if wire.trimmed_stop_suffix.is_some() && eligible_end == 0 {
            return Err(EndpointError::StopSuffixNotAtOutputEnd);
        }
        let mut previous_prefix_end = None;
        let mut terminal_seen = false;
        for candidate in wire.candidates.iter().copied() {
            ParagraphEndpointCandidate::new(
                candidate.raw_prefix_range,
                candidate.boundary,
                eligible_end,
                wire.max_prefix_bytes,
            )?;
            if previous_prefix_end.is_some_and(|end| candidate.raw_prefix_range.end() <= end)
                || terminal_seen
            {
                return Err(EndpointError::CandidatesUnorderedOrDuplicate);
            }
            previous_prefix_end = Some(candidate.raw_prefix_range.end());
            terminal_seen = matches!(
                candidate.boundary,
                ParagraphBoundaryWitness::TerminalLineBreak { .. }
            );
        }
        let extraction = Self {
            raw_output_blob_id: wire.raw_output_blob_id,
            raw_output_len: wire.raw_output_len,
            max_prefix_bytes: wire.max_prefix_bytes,
            trimmed_stop_suffix: wire.trimmed_stop_suffix,
            candidates: wire.candidates,
            fingerprint: wire.fingerprint,
        };
        if extraction.compute_fingerprint() != extraction.fingerprint {
            return Err(EndpointError::ExtractionFingerprintMismatch);
        }
        Ok(extraction)
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(ENDPOINT_FINGERPRINT_DOMAIN);
        digest.update(self.raw_output_blob_id.as_bytes());
        digest.update(self.raw_output_len.to_be_bytes());
        digest.update(self.max_prefix_bytes.to_be_bytes());
        match self.trimmed_stop_suffix {
            Some(witness) => {
                digest.update([1]);
                witness.update_digest(&mut digest);
            }
            None => digest.update([0]),
        }
        digest.update((self.candidates.len() as u64).to_be_bytes());
        for candidate in self.candidates.iter().copied() {
            candidate.update_digest(&mut digest);
        }
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParagraphEndpointExtractionWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    raw_output_blob_id: BlobId,
    raw_output_len: u64,
    max_prefix_bytes: u64,
    trimmed_stop_suffix: Option<TrimmedStopSuffixWitness>,
    candidates: BoundedVec<ParagraphEndpointCandidate, MAX_ENDPOINT_CANDIDATES>,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for ParagraphEndpointExtraction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ParagraphEndpointExtractionWire::deserialize(deserializer)?;
        Self::from_wire(wire).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for ParagraphEndpointCandidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ParagraphEndpointCandidateWire::deserialize(deserializer)?;
        // The enclosing extraction validates eligible-end and maximum bounds.
        if wire.raw_prefix_range.start() != 0
            || wire.boundary.separator_range().start() != wire.raw_prefix_range.end()
        {
            return Err(serde::de::Error::custom(
                EndpointError::InvalidBoundaryGeometry,
            ));
        }
        Ok(Self {
            raw_prefix_range: wire.raw_prefix_range,
            boundary: wire.boundary,
        })
    }
}

/// Move-only selection of one exact prefix. It cannot alter or hide the raw
/// completion and must replay against those bytes before extraction.
pub struct EndpointSelection {
    extraction: ParagraphEndpointExtraction,
    candidate_index: usize,
    candidate: ParagraphEndpointCandidate,
    fingerprint: BlobId,
}

impl EndpointSelection {
    pub const fn extraction(&self) -> &ParagraphEndpointExtraction {
        &self.extraction
    }

    pub const fn candidate_index(&self) -> usize {
        self.candidate_index
    }

    pub const fn candidate(&self) -> ParagraphEndpointCandidate {
        self.candidate
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn selected_bytes<'a>(&self, raw_output: &'a [u8]) -> Result<&'a [u8], EndpointError> {
        self.extraction.replay_against_raw(raw_output)?;
        Ok(self
            .candidate
            .raw_prefix_range()
            .checked_str(raw_output)?
            .as_bytes())
    }

    pub fn into_parts(
        self,
    ) -> (
        ParagraphEndpointExtraction,
        usize,
        ParagraphEndpointCandidate,
        BlobId,
    ) {
        (
            self.extraction,
            self.candidate_index,
            self.candidate,
            self.fingerprint,
        )
    }
}

impl std::fmt::Debug for EndpointSelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EndpointSelection")
            .field("extraction_fingerprint", &self.extraction.fingerprint())
            .field("candidate_index", &self.candidate_index)
            .field("candidate", &self.candidate)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EndpointError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Range(#[from] RangeError),
    #[error("raw output is empty or exceeds the configured maximum")]
    RawOutputTooLarge,
    #[error("endpoint maximum {actual} must be in 1..={maximum}")]
    InvalidMaximum { actual: u64, maximum: u64 },
    #[error("trimmed stop suffix does not end at the raw-output end")]
    StopSuffixNotAtOutputEnd,
    #[error("trimmed stop suffix bytes do not match its blob digest")]
    StopSuffixBlobMismatch,
    #[error("endpoint candidate is not a prefix beginning at byte zero")]
    EndpointIsNotPrefix,
    #[error("endpoint candidate exceeds the declared maximum")]
    EndpointExceedsMaximum,
    #[error("paragraph-boundary witness has invalid range geometry")]
    InvalidBoundaryGeometry,
    #[error(
        "paragraph endpoint candidates are duplicated, unordered, or follow a terminal boundary"
    )]
    CandidatesUnorderedOrDuplicate,
    #[error("raw output has more than {maximum} paragraph boundaries")]
    TooManyCandidates { maximum: usize },
    #[error("endpoint extraction fingerprint mismatch")]
    ExtractionFingerprintMismatch,
    #[error("strict endpoint replay does not reconstruct the persisted extraction")]
    ReplayMismatch,
    #[error("endpoint candidate index {index} is outside 0..{count}")]
    CandidateIndexOutOfBounds { index: usize, count: usize },
}

fn validate_raw_output(raw_output: &[u8]) -> Result<&str, EndpointError> {
    if raw_output.is_empty() || raw_output.len() as u64 > MAX_RAW_OUTPUT_BYTES {
        return Err(EndpointError::RawOutputTooLarge);
    }
    std::str::from_utf8(raw_output).map_err(|_| EndpointError::Range(RangeError::InvalidUtf8))
}

fn scan_paragraph_boundaries(
    eligible: &[u8],
    max_prefix_bytes: u64,
) -> Result<Vec<ParagraphEndpointCandidate>, EndpointError> {
    let _ =
        std::str::from_utf8(eligible).map_err(|_| EndpointError::Range(RangeError::InvalidUtf8))?;
    let mut candidates = Vec::new();
    let mut offset = 0_usize;
    while offset < eligible.len() {
        let Some(first_len) = line_ending_len(eligible, offset) else {
            offset += 1;
            continue;
        };
        let mut cursor = offset + first_len;
        while cursor < eligible.len() && matches!(eligible[cursor], b' ' | b'\t') {
            cursor += 1;
        }
        if let Some(second_len) = line_ending_len(eligible, cursor) {
            let separator_end = cursor + second_len;
            push_boundary_candidate(
                &mut candidates,
                eligible,
                offset,
                separator_end,
                max_prefix_bytes,
                false,
            )?;
            offset = separator_end;
            continue;
        }
        offset += first_len;
    }

    if let Some(separator_start) = terminal_line_break_start(eligible) {
        push_boundary_candidate(
            &mut candidates,
            eligible,
            separator_start,
            eligible.len(),
            max_prefix_bytes,
            true,
        )?;
    }
    Ok(candidates)
}

fn push_boundary_candidate(
    candidates: &mut Vec<ParagraphEndpointCandidate>,
    eligible: &[u8],
    prefix_end: usize,
    separator_end: usize,
    max_prefix_bytes: u64,
    terminal: bool,
) -> Result<(), EndpointError> {
    if prefix_end == 0
        || prefix_end as u64 > max_prefix_bytes
        || eligible[..prefix_end].iter().all(u8::is_ascii_whitespace)
    {
        return Ok(());
    }
    let raw_prefix_range = NonEmptyByteRange::new(0, prefix_end as u64)?;
    if candidates
        .iter()
        .any(|candidate| candidate.raw_prefix_range() == raw_prefix_range)
    {
        return Ok(());
    }
    if candidates.len() == MAX_ENDPOINT_CANDIDATES {
        return Err(EndpointError::TooManyCandidates {
            maximum: MAX_ENDPOINT_CANDIDATES,
        });
    }
    let separator_range = NonEmptyByteRange::new(prefix_end as u64, separator_end as u64)?;
    let boundary = if terminal {
        ParagraphBoundaryWitness::TerminalLineBreak { separator_range }
    } else {
        ParagraphBoundaryWitness::BlankLine { separator_range }
    };
    let candidate = ParagraphEndpointCandidate::new(
        raw_prefix_range,
        boundary,
        eligible.len() as u64,
        max_prefix_bytes,
    )?;
    if candidates.last().copied() != Some(candidate) {
        candidates.push(candidate);
    }
    Ok(())
}

fn line_ending_len(bytes: &[u8], offset: usize) -> Option<usize> {
    match bytes.get(offset..) {
        Some([b'\r', b'\n', ..]) => Some(2),
        Some([b'\n', ..]) => Some(1),
        _ => None,
    }
}

fn terminal_line_break_start(bytes: &[u8]) -> Option<usize> {
    let mut cursor = bytes.len();
    let mut found = false;
    loop {
        while cursor > 0 && matches!(bytes[cursor - 1], b' ' | b'\t') {
            cursor -= 1;
        }
        if cursor >= 2 && bytes[cursor - 2..cursor] == *b"\r\n" {
            cursor -= 2;
            found = true;
        } else if cursor >= 1 && bytes[cursor - 1] == b'\n' {
            cursor -= 1;
            found = true;
        } else {
            break;
        }
    }
    found.then_some(cursor)
}

fn fingerprint_selection(
    extraction: BlobId,
    candidate_index: usize,
    candidate: ParagraphEndpointCandidate,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(SELECTION_FINGERPRINT_DOMAIN);
    digest.update(extraction.as_bytes());
    digest.update((candidate_index as u64).to_be_bytes());
    candidate.update_digest(&mut digest);
    BlobId::from_bytes(digest.finalize().into())
}
