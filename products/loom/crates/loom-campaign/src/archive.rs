use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{ManifestKey, TrialCaseId};
use loom_search::{SCORE_SCALE, UnitScore};
use loom_types::{ArtifactId, BlobId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CampaignBudgetAmount, EvaluationLeasePurpose, HalvingOutcome, VerifiedEvaluatedCandidateLease,
};

pub const MAX_DESCRIPTOR_AXES: usize = 8;
pub const MAX_DESCRIPTOR_BINS: u16 = 64;
pub const MAX_MAP_ELITES_CELLS: usize = 4_096;
pub const MAX_ELITES_PER_CELL: usize = 16;
pub const MAX_GLOBAL_PARETO_POINTS: usize = 256;
pub const MAX_SEEN_ARCHIVE_OCCURRENCES: usize = 65_536;

const ARCHIVE_DOMAIN: &[u8] = b"loom/map-elites-archive/v2\0";
const OCCURRENCE_COMMITMENT_DOMAIN: &[u8] = b"loom/map-elites-occurrence/v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DescriptorAxis {
    id: ManifestKey,
    bins: u16,
}

impl DescriptorAxis {
    pub fn new(id: ManifestKey, bins: u16) -> Result<Self, ArchiveError> {
        if bins == 0 || bins > MAX_DESCRIPTOR_BINS {
            return Err(ArchiveError::InvalidBinCount(bins));
        }
        Ok(Self { id, bins })
    }

    pub const fn id(&self) -> &ManifestKey {
        &self.id
    }

    pub const fn bins(&self) -> u16 {
        self.bins
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ArchiveCellCoordinate(Vec<u16>);

impl ArchiveCellCoordinate {
    pub fn bins(&self) -> &[u16] {
        &self.0
    }
}

/// Evidence retained for one causal evaluated occurrence. Construction is
/// private and consumes a live [`VerifiedEvaluatedCandidateLease`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MapEliteCandidate {
    occurrence_id: ArtifactId,
    content_blob_id: BlobId,
    trial_fingerprint: BlobId,
    case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    evaluation_receipt_fingerprint: BlobId,
    evaluation_coverage_fingerprint: BlobId,
    actual_charge: CampaignBudgetAmount,
    quality: UnitScore,
    compute_cost: u64,
    descriptors: Vec<(ManifestKey, UnitScore)>,
    commitment: BlobId,
}

impl MapEliteCandidate {
    pub const fn occurrence_id(&self) -> ArtifactId {
        self.occurrence_id
    }

    pub const fn content_blob_id(&self) -> BlobId {
        self.content_blob_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub const fn actual_charge(&self) -> CampaignBudgetAmount {
        self.actual_charge
    }

    pub const fn quality(&self) -> UnitScore {
        self.quality
    }

    pub const fn compute_cost(&self) -> u64 {
        self.compute_cost
    }

    pub fn descriptors(&self) -> &[(ManifestKey, UnitScore)] {
        &self.descriptors
    }

    pub const fn commitment(&self) -> BlobId {
        self.commitment
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveDecisionKind {
    Inserted,
    ExpandedParetoCell,
    ReplacedDominated,
    RetainedIncumbent,
    PrunedByCellBound,
    AlreadyPresent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ArchiveDecision {
    kind: ArchiveDecisionKind,
    coordinate: ArchiveCellCoordinate,
    challenger_occurrence: ArtifactId,
    removed_occurrences: Vec<ArtifactId>,
}

impl ArchiveDecision {
    pub const fn kind(&self) -> ArchiveDecisionKind {
        self.kind
    }

    pub const fn coordinate(&self) -> &ArchiveCellCoordinate {
        &self.coordinate
    }

    pub const fn challenger_occurrence(&self) -> ArtifactId {
        self.challenger_occurrence
    }

    pub fn removed_occurrences(&self) -> &[ArtifactId] {
        &self.removed_occurrences
    }
}

/// Immutable archive snapshot with append-only occurrence commitments.
///
/// Each cell stores a bounded nondominated quality/compute set. A separate
/// bounded global Pareto set retains cross-cell quality/compute evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MapElitesArchive {
    campaign_fingerprint: BlobId,
    axes: Vec<DescriptorAxis>,
    generation: u64,
    parent_fingerprint: Option<BlobId>,
    seen_occurrences: BTreeMap<ArtifactId, BlobId>,
    cells: BTreeMap<ArchiveCellCoordinate, Vec<MapEliteCandidate>>,
    global_pareto: Vec<MapEliteCandidate>,
    fingerprint: BlobId,
}

impl MapElitesArchive {
    pub fn empty(
        campaign_fingerprint: BlobId,
        axes: Vec<DescriptorAxis>,
    ) -> Result<Self, ArchiveError> {
        validate_axes(&axes)?;
        let mut archive = Self {
            campaign_fingerprint,
            axes,
            generation: 0,
            parent_fingerprint: None,
            seen_occurrences: BTreeMap::new(),
            cells: BTreeMap::new(),
            global_pareto: Vec::new(),
            fingerprint: BlobId::digest(b"uninitialized map elites archive"),
        };
        archive.fingerprint = fingerprint_archive(&archive);
        Ok(archive)
    }

    pub(crate) fn consider(
        &self,
        lease: VerifiedEvaluatedCandidateLease,
    ) -> Result<ArchiveUpdate, ArchiveError> {
        self.verify()?;
        verify_archive_lease(self, &lease)?;
        let challenger = candidate_from_lease(lease)?;
        let coordinate = coordinate_for(&self.axes, &challenger)?;
        if let Some(commitment) = self.seen_occurrences.get(&challenger.occurrence_id) {
            if *commitment != challenger.commitment {
                return Err(ArchiveError::OccurrenceConflict(challenger.occurrence_id));
            }
            return Ok(ArchiveUpdate {
                decision: ArchiveDecision {
                    kind: ArchiveDecisionKind::AlreadyPresent,
                    coordinate,
                    challenger_occurrence: challenger.occurrence_id,
                    removed_occurrences: Vec::new(),
                },
                snapshot: self.clone(),
            });
        }
        if self.seen_occurrences.len() >= MAX_SEEN_ARCHIVE_OCCURRENCES {
            return Err(ArchiveError::SeenOccurrenceLimit);
        }

        let incumbents = self.cells.get(&coordinate).cloned().unwrap_or_default();
        let (cell, kind, removed_occurrences) =
            update_pareto_set(incumbents, &challenger, MAX_ELITES_PER_CELL);
        let (global_pareto, _, _) = update_pareto_set(
            self.global_pareto.clone(),
            &challenger,
            MAX_GLOBAL_PARETO_POINTS,
        );
        let mut seen_occurrences = self.seen_occurrences.clone();
        seen_occurrences.insert(challenger.occurrence_id, challenger.commitment);
        let mut cells = self.cells.clone();
        if cell.is_empty() {
            cells.remove(&coordinate);
        } else {
            cells.insert(coordinate.clone(), cell);
        }
        if cells.len() > MAX_MAP_ELITES_CELLS {
            return Err(ArchiveError::CellLimit);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ArchiveError::GenerationOverflow)?;
        let mut snapshot = Self {
            campaign_fingerprint: self.campaign_fingerprint,
            axes: self.axes.clone(),
            generation,
            parent_fingerprint: Some(self.fingerprint),
            seen_occurrences,
            cells,
            global_pareto,
            fingerprint: BlobId::digest(b"uninitialized map elites update"),
        };
        snapshot.fingerprint = fingerprint_archive(&snapshot);
        Ok(ArchiveUpdate {
            decision: ArchiveDecision {
                kind,
                coordinate,
                challenger_occurrence: challenger.occurrence_id,
                removed_occurrences,
            },
            snapshot,
        })
    }

    pub const fn campaign_fingerprint(&self) -> BlobId {
        self.campaign_fingerprint
    }

    pub fn axes(&self) -> &[DescriptorAxis] {
        &self.axes
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn parent_fingerprint(&self) -> Option<BlobId> {
        self.parent_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn seen_occurrences(&self) -> impl Iterator<Item = (ArtifactId, BlobId)> + '_ {
        self.seen_occurrences
            .iter()
            .map(|(occurrence, commitment)| (*occurrence, *commitment))
    }

    pub fn occurrence_commitment(&self, occurrence_id: ArtifactId) -> Option<BlobId> {
        self.seen_occurrences.get(&occurrence_id).copied()
    }

    pub fn cells(&self) -> impl Iterator<Item = (&ArchiveCellCoordinate, &[MapEliteCandidate])> {
        self.cells
            .iter()
            .map(|(coordinate, candidates)| (coordinate, candidates.as_slice()))
    }

    pub fn cell(&self, coordinate: &ArchiveCellCoordinate) -> Option<&[MapEliteCandidate]> {
        self.cells.get(coordinate).map(Vec::as_slice)
    }

    pub fn global_pareto(&self) -> &[MapEliteCandidate] {
        &self.global_pareto
    }

    pub(crate) fn verify(&self) -> Result<(), ArchiveError> {
        validate_axes(&self.axes)?;
        if self.cells.len() > MAX_MAP_ELITES_CELLS
            || self.seen_occurrences.len() > MAX_SEEN_ARCHIVE_OCCURRENCES
            || self
                .cells
                .values()
                .any(|cell| cell.len() > MAX_ELITES_PER_CELL)
            || self.global_pareto.len() > MAX_GLOBAL_PARETO_POINTS
            || fingerprint_archive(self) != self.fingerprint
        {
            return Err(ArchiveError::SnapshotIntegrity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ArchiveUpdate {
    decision: ArchiveDecision,
    snapshot: MapElitesArchive,
}

impl ArchiveUpdate {
    pub const fn decision(&self) -> &ArchiveDecision {
        &self.decision
    }

    pub const fn snapshot(&self) -> &MapElitesArchive {
        &self.snapshot
    }

    pub fn into_snapshot(self) -> MapElitesArchive {
        self.snapshot
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ArchiveError {
    #[error("descriptor bin count {0} is outside 1..={MAX_DESCRIPTOR_BINS}")]
    InvalidBinCount(u16),
    #[error("descriptor axis count {0} is outside 1..={MAX_DESCRIPTOR_AXES}")]
    InvalidAxisCount(usize),
    #[error("descriptor axes repeat an id")]
    DuplicateAxis,
    #[error("descriptor grid exceeds {MAX_MAP_ELITES_CELLS} cells")]
    CellLimit,
    #[error("archive has reached {MAX_SEEN_ARCHIVE_OCCURRENCES} seen occurrences")]
    SeenOccurrenceLimit,
    #[error("candidate compute cost must be nonzero")]
    EmptyComputeCost,
    #[error("archive candidates require a scored evaluator outcome matching quality")]
    IneligibleEvaluation,
    #[error("candidate compute accounting overflowed")]
    ComputeOverflow,
    #[error("candidate descriptor count {0} is outside 1..={MAX_DESCRIPTOR_AXES}")]
    InvalidDescriptorCount(usize),
    #[error("candidate repeats a descriptor")]
    DuplicateDescriptor,
    #[error("candidate descriptors do not match the declared archive axes")]
    DescriptorMismatch,
    #[error("verified archive lease does not match this campaign or parent snapshot")]
    LeaseMismatch,
    #[error("occurrence {0} was reused with different evaluated evidence")]
    OccurrenceConflict(ArtifactId),
    #[error("archive generation overflowed")]
    GenerationOverflow,
    #[error("archive snapshot fingerprint or bounds are invalid")]
    SnapshotIntegrity,
}

fn verify_archive_lease(
    archive: &MapElitesArchive,
    lease: &VerifiedEvaluatedCandidateLease,
) -> Result<(), ArchiveError> {
    if lease.campaign_fingerprint != archive.campaign_fingerprint
        || lease.purpose
            != (EvaluationLeasePurpose::Archive {
                parent_fingerprint: archive.fingerprint,
            })
    {
        return Err(ArchiveError::LeaseMismatch);
    }
    Ok(())
}

fn candidate_from_lease(
    lease: VerifiedEvaluatedCandidateLease,
) -> Result<MapEliteCandidate, ArchiveError> {
    if lease.outcome
        != (HalvingOutcome::Scored {
            score: lease.quality,
        })
    {
        return Err(ArchiveError::IneligibleEvaluation);
    }
    let compute_cost = compute_cost(lease.actual_charge)?;
    if compute_cost == 0 {
        return Err(ArchiveError::EmptyComputeCost);
    }
    if lease.descriptors.is_empty() || lease.descriptors.len() > MAX_DESCRIPTOR_AXES {
        return Err(ArchiveError::InvalidDescriptorCount(
            lease.descriptors.len(),
        ));
    }
    let mut descriptors = BTreeMap::new();
    for (axis, score) in lease.descriptors {
        if descriptors.insert(axis, score).is_some() {
            return Err(ArchiveError::DuplicateDescriptor);
        }
    }
    let descriptors = descriptors.into_iter().collect::<Vec<_>>();
    let mut candidate = MapEliteCandidate {
        occurrence_id: lease.occurrence_id,
        content_blob_id: lease.content_blob_id,
        trial_fingerprint: lease.trial_fingerprint,
        case_id: lease.case_id,
        treatment_fingerprint: lease.treatment_fingerprint,
        evaluation_receipt_fingerprint: lease.evaluation_receipt_fingerprint,
        evaluation_coverage_fingerprint: lease.coverage_fingerprint,
        actual_charge: lease.actual_charge,
        quality: lease.quality,
        compute_cost,
        descriptors,
        commitment: BlobId::digest(b"uninitialized occurrence commitment"),
    };
    candidate.commitment = fingerprint_occurrence(&candidate);
    Ok(candidate)
}

fn update_pareto_set(
    mut incumbents: Vec<MapEliteCandidate>,
    challenger: &MapEliteCandidate,
    maximum: usize,
) -> (Vec<MapEliteCandidate>, ArchiveDecisionKind, Vec<ArtifactId>) {
    if incumbents
        .iter()
        .any(|incumbent| dominates_or_stable_tie(incumbent, challenger))
    {
        return (
            incumbents,
            ArchiveDecisionKind::RetainedIncumbent,
            Vec::new(),
        );
    }
    let mut removed = incumbents
        .iter()
        .filter(|incumbent| dominates_or_stable_tie(challenger, incumbent))
        .map(MapEliteCandidate::occurrence_id)
        .collect::<Vec<_>>();
    incumbents.retain(|incumbent| !dominates_or_stable_tie(challenger, incumbent));
    let initial_was_empty = incumbents.is_empty() && removed.is_empty();
    let replaced = !removed.is_empty();
    incumbents.push(challenger.clone());
    incumbents.sort_unstable_by(compare_archive_order);
    let mut pruned_challenger = false;
    if incumbents.len() > maximum {
        let pruned = incumbents.split_off(maximum);
        pruned_challenger = pruned
            .iter()
            .any(|candidate| candidate.occurrence_id == challenger.occurrence_id);
        removed.extend(pruned.iter().map(MapEliteCandidate::occurrence_id));
    }
    removed.sort_unstable();
    let kind = if pruned_challenger {
        ArchiveDecisionKind::PrunedByCellBound
    } else if initial_was_empty {
        ArchiveDecisionKind::Inserted
    } else if replaced {
        ArchiveDecisionKind::ReplacedDominated
    } else {
        ArchiveDecisionKind::ExpandedParetoCell
    };
    (incumbents, kind, removed)
}

fn dominates_or_stable_tie(left: &MapEliteCandidate, right: &MapEliteCandidate) -> bool {
    let weakly_better = left.quality >= right.quality && left.compute_cost <= right.compute_cost;
    let strictly_better = left.quality > right.quality || left.compute_cost < right.compute_cost;
    weakly_better
        && (strictly_better
            || (left.quality == right.quality
                && left.compute_cost == right.compute_cost
                && left.occurrence_id < right.occurrence_id))
}

fn compare_archive_order(left: &MapEliteCandidate, right: &MapEliteCandidate) -> Ordering {
    right
        .quality
        .cmp(&left.quality)
        .then_with(|| left.compute_cost.cmp(&right.compute_cost))
        .then_with(|| left.occurrence_id.cmp(&right.occurrence_id))
}

fn validate_axes(axes: &[DescriptorAxis]) -> Result<(), ArchiveError> {
    if axes.is_empty() || axes.len() > MAX_DESCRIPTOR_AXES {
        return Err(ArchiveError::InvalidAxisCount(axes.len()));
    }
    let mut ids = BTreeSet::new();
    let mut cell_count = 1_usize;
    for axis in axes {
        if !ids.insert(&axis.id) {
            return Err(ArchiveError::DuplicateAxis);
        }
        cell_count = cell_count
            .checked_mul(usize::from(axis.bins))
            .ok_or(ArchiveError::CellLimit)?;
        if cell_count > MAX_MAP_ELITES_CELLS {
            return Err(ArchiveError::CellLimit);
        }
    }
    Ok(())
}

fn coordinate_for(
    axes: &[DescriptorAxis],
    candidate: &MapEliteCandidate,
) -> Result<ArchiveCellCoordinate, ArchiveError> {
    let descriptors = candidate
        .descriptors
        .iter()
        .map(|(axis, value)| (axis, *value))
        .collect::<BTreeMap<_, _>>();
    if descriptors.len() != axes.len()
        || axes.iter().any(|axis| !descriptors.contains_key(&axis.id))
    {
        return Err(ArchiveError::DescriptorMismatch);
    }
    let bins = axes
        .iter()
        .map(|axis| {
            let score = u64::from(descriptors[&axis.id].millionths());
            let bin = score * u64::from(axis.bins) / (u64::from(SCORE_SCALE) + 1);
            u16::try_from(bin).expect("bounded score and bin count fit u16")
        })
        .collect();
    Ok(ArchiveCellCoordinate(bins))
}

fn fingerprint_occurrence(candidate: &MapEliteCandidate) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(OCCURRENCE_COMMITMENT_DOMAIN);
    digest.update(candidate.occurrence_id.as_ulid().to_bytes());
    digest.update(candidate.content_blob_id.as_bytes());
    digest.update(candidate.trial_fingerprint.as_bytes());
    digest.update(candidate.case_id.as_ulid().to_bytes());
    digest.update(candidate.treatment_fingerprint.as_bytes());
    digest.update(candidate.evaluation_receipt_fingerprint.as_bytes());
    digest.update(candidate.evaluation_coverage_fingerprint.as_bytes());
    candidate.actual_charge.update_digest(&mut digest);
    digest.update(candidate.quality.millionths().to_be_bytes());
    digest.update(candidate.compute_cost.to_be_bytes());
    for (axis, value) in &candidate.descriptors {
        update_text(&mut digest, axis.as_str());
        digest.update(value.millionths().to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn compute_cost(charge: CampaignBudgetAmount) -> Result<u64, ArchiveError> {
    charge
        .writer_tokens()
        .checked_add(charge.controller_tokens())
        .and_then(|value| value.checked_add(u64::from(charge.evaluations())))
        .and_then(|value| value.checked_add(charge.wall_time_ms()))
        .ok_or(ArchiveError::ComputeOverflow)
}

fn fingerprint_archive(archive: &MapElitesArchive) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(ARCHIVE_DOMAIN);
    digest.update(archive.campaign_fingerprint.as_bytes());
    digest.update(archive.generation.to_be_bytes());
    update_optional_blob(&mut digest, archive.parent_fingerprint);
    digest.update((archive.axes.len() as u64).to_be_bytes());
    for axis in &archive.axes {
        update_text(&mut digest, axis.id.as_str());
        digest.update(axis.bins.to_be_bytes());
    }
    digest.update((archive.seen_occurrences.len() as u64).to_be_bytes());
    for (occurrence, commitment) in &archive.seen_occurrences {
        digest.update(occurrence.as_ulid().to_bytes());
        digest.update(commitment.as_bytes());
    }
    digest.update((archive.cells.len() as u64).to_be_bytes());
    for (coordinate, candidates) in &archive.cells {
        update_coordinate(&mut digest, coordinate);
        update_candidates(&mut digest, candidates);
    }
    update_candidates(&mut digest, &archive.global_pareto);
    BlobId::from_bytes(digest.finalize().into())
}

fn update_coordinate(digest: &mut Sha256, coordinate: &ArchiveCellCoordinate) {
    digest.update((coordinate.0.len() as u64).to_be_bytes());
    for bin in &coordinate.0 {
        digest.update(bin.to_be_bytes());
    }
}

fn update_candidates(digest: &mut Sha256, candidates: &[MapEliteCandidate]) {
    digest.update((candidates.len() as u64).to_be_bytes());
    for candidate in candidates {
        digest.update(candidate.commitment.as_bytes());
    }
}

fn update_optional_blob(digest: &mut Sha256, value: Option<BlobId>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.as_bytes());
        }
        None => digest.update([0]),
    }
}

fn update_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}
