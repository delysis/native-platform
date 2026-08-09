use std::fmt;

use loom_research_types::{
    CallScope, CompiledBaseCompletionPrompt, CompletionPromptBlockRole, CompletionPromptTail,
    ExactPromptBlockBytes, ExactPromptSource, FrozenBaseCompletionPrompt,
    FrozenCompletionPromptBlock, NonEmptyByteRange, PromptBlockWitness, PromptCompileError,
    PromptSourceRange,
};
use loom_types::{BlobId, ProjectId, RevisionId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    AntiCopyConfig, AntiCopyError, AntiCopyReport, DiversifiedSelection, ExactExcerpt,
    ExactExcerptIdentity, ExternalSemanticSimilarity,
};

const RETRIEVAL_RECIPE_DOMAIN: &[u8] = b"loom/retrieval-bound-prompt-recipe/v1\0";

/// Frozen inputs for compiling exact retrieved apprenticeship text into a raw
/// base-model completion prompt.
///
/// The selection cannot be reconstructed from serialized claims: its fields
/// are private and `DiversifiedSelection` has no `Deserialize` implementation.
/// Compilation still replays every selected range against the full immutable
/// source bytes before any prompt becomes inference-ready.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetrievedPromptPlan {
    project_id: ProjectId,
    scope: CallScope,
    base_treatment_recipe_fingerprint: BlobId,
    leading_blocks: Vec<FrozenCompletionPromptBlock>,
    selection: DiversifiedSelection,
    tail: CompletionPromptTail,
}

impl RetrievedPromptPlan {
    pub fn new(
        project_id: ProjectId,
        scope: CallScope,
        base_treatment_recipe_fingerprint: BlobId,
        leading_blocks: Vec<FrozenCompletionPromptBlock>,
        selection: DiversifiedSelection,
        tail: CompletionPromptTail,
    ) -> Result<Self, RetrievedPromptError> {
        if selection.prompt_order().is_empty() {
            return Err(RetrievedPromptError::EmptySelection);
        }
        if leading_blocks
            .iter()
            .any(|block| block.role() == CompletionPromptBlockRole::SourceApprenticeship)
        {
            return Err(RetrievedPromptError::UntrackedApprenticeshipBlock);
        }
        Ok(Self {
            project_id,
            scope,
            base_treatment_recipe_fingerprint,
            leading_blocks,
            selection,
            tail,
        })
    }

    /// Compiles the exact prompt and returns the separately retained source set
    /// required to hard-gate every generated candidate for copying.
    ///
    /// `exact_sources` must be the complete and minimal set used by all leading
    /// blocks, selected excerpts, and the final tail. The lower-level compiler
    /// rejects missing, duplicated, changed, and unreferenced bindings.
    pub fn compile(
        self,
        exact_sources: &[ExactPromptSource<'_>],
    ) -> Result<PreparedRetrievedPrompt, RetrievedPromptError> {
        let selection_fingerprint = self.selection.selection_fingerprint();
        let bound_treatment_recipe_fingerprint = bind_treatment_recipe(
            self.base_treatment_recipe_fingerprint,
            selection_fingerprint,
        );
        let mut blocks = self.leading_blocks;
        let mut exact_excerpts = Vec::with_capacity(self.selection.prompt_order().len());
        for selected in self.selection.prompt_order() {
            let excerpt = selected.candidate().excerpt();
            verify_exact_excerpt_source(excerpt, exact_sources)?;
            let identity = excerpt.identity();
            let range = NonEmptyByteRange::new(
                identity.source_range.start(),
                identity.source_range.end_exclusive(),
            )?;
            let source = PromptSourceRange::new(
                identity.source_revision_id,
                identity.source_blob_id,
                range,
            )?;
            blocks.push(FrozenCompletionPromptBlock::new(
                CompletionPromptBlockRole::SourceApprenticeship,
                ExactPromptBlockBytes::new(excerpt.text().as_bytes().to_vec())?,
                PromptBlockWitness::exact_source(source),
            )?);
            exact_excerpts.push(excerpt.clone());
        }

        let exact_prompt_bytes = assemble_exact_prompt_bytes(&blocks, self.tail, exact_sources)?;
        let specification = FrozenBaseCompletionPrompt::new(
            self.project_id,
            self.scope,
            bound_treatment_recipe_fingerprint,
            blocks,
            self.tail,
        )?;
        let compiled_prompt = specification.compile(exact_prompt_bytes, exact_sources)?;
        let compiled_prompt_fingerprint = compiled_prompt.fingerprint();
        let source_set_fingerprint = fingerprint_source_set(
            selection_fingerprint,
            compiled_prompt_fingerprint,
            &exact_excerpts,
        );
        let witness = RetrievalPromptWitness {
            base_treatment_recipe_fingerprint: self.base_treatment_recipe_fingerprint,
            bound_treatment_recipe_fingerprint,
            selection_fingerprint,
            compiled_prompt_fingerprint,
            source_set_fingerprint,
            prompt_order: exact_excerpts.iter().map(ExactExcerpt::identity).collect(),
            total_apprenticeship_tokens: self.selection.total_tokens(),
            total_apprenticeship_bytes: self.selection.total_bytes(),
        };
        Ok(PreparedRetrievedPrompt {
            compiled_prompt,
            anti_copy_sources: VerifiedPromptApprenticeshipSet {
                selection_fingerprint,
                compiled_prompt_fingerprint,
                source_set_fingerprint,
                exact_excerpts,
            },
            witness,
        })
    }
}

/// Serializable diagnostic evidence for one source-grounded prompt compile.
/// It is not live inference, source-catalog, evaluation, or benchmark authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RetrievalPromptWitness {
    base_treatment_recipe_fingerprint: BlobId,
    bound_treatment_recipe_fingerprint: BlobId,
    selection_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    source_set_fingerprint: BlobId,
    prompt_order: Vec<ExactExcerptIdentity>,
    total_apprenticeship_tokens: u64,
    total_apprenticeship_bytes: usize,
}

impl RetrievalPromptWitness {
    pub const fn base_treatment_recipe_fingerprint(&self) -> BlobId {
        self.base_treatment_recipe_fingerprint
    }

    pub const fn bound_treatment_recipe_fingerprint(&self) -> BlobId {
        self.bound_treatment_recipe_fingerprint
    }

    pub const fn selection_fingerprint(&self) -> BlobId {
        self.selection_fingerprint
    }

    pub const fn compiled_prompt_fingerprint(&self) -> BlobId {
        self.compiled_prompt_fingerprint
    }

    pub const fn source_set_fingerprint(&self) -> BlobId {
        self.source_set_fingerprint
    }

    pub fn prompt_order(&self) -> &[ExactExcerptIdentity] {
        &self.prompt_order
    }

    pub const fn total_apprenticeship_tokens(&self) -> u64 {
        self.total_apprenticeship_tokens
    }

    pub const fn total_apprenticeship_bytes(&self) -> usize {
        self.total_apprenticeship_bytes
    }
}

/// Inference-ready prompt plus the exact source set that must survive until
/// every sibling candidate has passed the anti-copy gate.
///
/// This type is deliberately move-only and non-serializable.
///
/// ```compile_fail
/// use loom_context::PreparedRetrievedPrompt;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<PreparedRetrievedPrompt>();
/// ```
///
/// ```compile_fail
/// use loom_context::PreparedRetrievedPrompt;
/// fn requires_serialize<T: serde::Serialize>() {}
/// requires_serialize::<PreparedRetrievedPrompt>();
/// ```
pub struct PreparedRetrievedPrompt {
    compiled_prompt: CompiledBaseCompletionPrompt,
    anti_copy_sources: VerifiedPromptApprenticeshipSet,
    witness: RetrievalPromptWitness,
}

impl fmt::Debug for PreparedRetrievedPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedRetrievedPrompt")
            .field("compiled_prompt", &self.compiled_prompt)
            .field("anti_copy_sources", &self.anti_copy_sources)
            .field("witness", &self.witness)
            .finish()
    }
}

impl PreparedRetrievedPrompt {
    pub fn into_parts(
        self,
    ) -> (
        CompiledBaseCompletionPrompt,
        VerifiedPromptApprenticeshipSet,
        RetrievalPromptWitness,
    ) {
        (self.compiled_prompt, self.anti_copy_sources, self.witness)
    }
}

/// Non-serializable evidence that every apprenticeship excerpt was replayed
/// against the full immutable source used by the compiled prompt.
///
/// ```compile_fail
/// use loom_context::VerifiedPromptApprenticeshipSet;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<VerifiedPromptApprenticeshipSet>();
/// ```
pub struct VerifiedPromptApprenticeshipSet {
    selection_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    source_set_fingerprint: BlobId,
    exact_excerpts: Vec<ExactExcerpt>,
}

impl fmt::Debug for VerifiedPromptApprenticeshipSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedPromptApprenticeshipSet")
            .field("selection_fingerprint", &self.selection_fingerprint)
            .field(
                "compiled_prompt_fingerprint",
                &self.compiled_prompt_fingerprint,
            )
            .field("source_set_fingerprint", &self.source_set_fingerprint)
            .field("excerpt_count", &self.exact_excerpts.len())
            .finish_non_exhaustive()
    }
}

impl VerifiedPromptApprenticeshipSet {
    pub const fn selection_fingerprint(&self) -> BlobId {
        self.selection_fingerprint
    }

    pub const fn compiled_prompt_fingerprint(&self) -> BlobId {
        self.compiled_prompt_fingerprint
    }

    pub const fn source_set_fingerprint(&self) -> BlobId {
        self.source_set_fingerprint
    }

    pub fn exact_excerpts(&self) -> &[ExactExcerpt] {
        &self.exact_excerpts
    }

    /// Computes complete exact/fuzzy/optional-semantic evidence. This report is
    /// deterministic evidence only; an admitted-candidate adapter must bind it
    /// to writer authority before it can satisfy a campaign or benchmark gate.
    pub fn analyze_candidate(
        &self,
        candidate: &str,
        external_semantic_scores: &[ExternalSemanticSimilarity],
        config: AntiCopyConfig,
    ) -> Result<PromptBoundAntiCopyReport, AntiCopyError> {
        let report = crate::analyze_anti_copy(
            candidate,
            &self.exact_excerpts,
            external_semantic_scores,
            config,
        )?;
        let evidence_fingerprint = fingerprint_anti_copy_evidence(
            self.source_set_fingerprint,
            &report,
            external_semantic_scores,
            config,
        );
        Ok(PromptBoundAntiCopyReport {
            selection_fingerprint: self.selection_fingerprint,
            compiled_prompt_fingerprint: self.compiled_prompt_fingerprint,
            source_set_fingerprint: self.source_set_fingerprint,
            evidence_fingerprint,
            semantic_evidence_count: external_semantic_scores.len(),
            semantic_evidence_complete: external_semantic_scores.len() == self.exact_excerpts.len(),
            report,
        })
    }
}

/// Prompt-bound deterministic anti-copy evidence.
///
/// This remains a diagnostic record rather than gate authority: optional
/// semantic scores are still external claims until a native embedding/scorer
/// verifier supplies a separate move-only lease.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromptBoundAntiCopyReport {
    selection_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    source_set_fingerprint: BlobId,
    evidence_fingerprint: BlobId,
    semantic_evidence_count: usize,
    semantic_evidence_complete: bool,
    report: AntiCopyReport,
}

impl PromptBoundAntiCopyReport {
    pub const fn selection_fingerprint(&self) -> BlobId {
        self.selection_fingerprint
    }

    pub const fn compiled_prompt_fingerprint(&self) -> BlobId {
        self.compiled_prompt_fingerprint
    }

    pub const fn source_set_fingerprint(&self) -> BlobId {
        self.source_set_fingerprint
    }

    pub const fn evidence_fingerprint(&self) -> BlobId {
        self.evidence_fingerprint
    }

    pub const fn semantic_evidence_count(&self) -> usize {
        self.semantic_evidence_count
    }

    pub const fn semantic_evidence_complete(&self) -> bool {
        self.semantic_evidence_complete
    }

    pub const fn report(&self) -> &AntiCopyReport {
        &self.report
    }

    pub const fn any_suspect(&self) -> bool {
        self.report.any_suspect
    }
}

#[derive(Debug, Error)]
pub enum RetrievedPromptError {
    #[error("retrieved apprenticeship compilation requires at least one selected excerpt")]
    EmptySelection,
    #[error("leading blocks may not smuggle an untracked source-apprenticeship block")]
    UntrackedApprenticeshipBlock,
    #[error("exact source revision {0} is absent from prompt compilation bindings")]
    MissingSourceRevision(RevisionId),
    #[error("exact source revision {0} appears more than once in prompt compilation bindings")]
    DuplicateSourceRevision(RevisionId),
    #[error("selected source revision {revision_id} hashes to {actual}, expected {expected}")]
    SourceBlobMismatch {
        revision_id: RevisionId,
        expected: BlobId,
        actual: BlobId,
    },
    #[error("selected excerpt range is not an exact UTF-8 slice of source revision {0}")]
    InvalidExcerptRange(RevisionId),
    #[error("selected excerpt text differs from its exact immutable source range")]
    ExcerptBytesMismatch,
    #[error("tail source binding is missing")]
    MissingTailSource,
    #[error("tail source binding appears more than once")]
    DuplicateTailSource,
    #[error("tail source bytes differ from the frozen tail blob")]
    TailBlobMismatch,
    #[error("tail range is not an exact UTF-8 slice of its frozen source")]
    InvalidTailRange,
    #[error("prompt byte-size arithmetic overflowed")]
    PromptSizeOverflow,
    #[error(transparent)]
    Prompt(#[from] PromptCompileError),
    #[error(transparent)]
    Range(#[from] loom_research_types::RangeError),
}

fn verify_exact_excerpt_source(
    excerpt: &ExactExcerpt,
    exact_sources: &[ExactPromptSource<'_>],
) -> Result<(), RetrievedPromptError> {
    let identity = excerpt.identity();
    let source = unique_revision_source(identity.source_revision_id, exact_sources)?;
    let actual = BlobId::digest(source.bytes());
    if actual != identity.source_blob_id {
        return Err(RetrievedPromptError::SourceBlobMismatch {
            revision_id: identity.source_revision_id,
            expected: identity.source_blob_id,
            actual,
        });
    }
    let range = NonEmptyByteRange::new(
        identity.source_range.start(),
        identity.source_range.end_exclusive(),
    )?;
    let exact = range
        .checked_str(source.bytes())
        .map_err(|_| RetrievedPromptError::InvalidExcerptRange(identity.source_revision_id))?;
    if exact != excerpt.text() || BlobId::digest(exact.as_bytes()) != identity.excerpt_hash {
        return Err(RetrievedPromptError::ExcerptBytesMismatch);
    }
    Ok(())
}

fn unique_revision_source<'a>(
    revision_id: RevisionId,
    exact_sources: &'a [ExactPromptSource<'a>],
) -> Result<ExactPromptSource<'a>, RetrievedPromptError> {
    let mut matching = exact_sources
        .iter()
        .copied()
        .filter(|source| source.revision_id() == Some(revision_id));
    let source = matching
        .next()
        .ok_or(RetrievedPromptError::MissingSourceRevision(revision_id))?;
    if matching.next().is_some() {
        return Err(RetrievedPromptError::DuplicateSourceRevision(revision_id));
    }
    Ok(source)
}

fn assemble_exact_prompt_bytes(
    blocks: &[FrozenCompletionPromptBlock],
    tail: CompletionPromptTail,
    exact_sources: &[ExactPromptSource<'_>],
) -> Result<Vec<u8>, RetrievedPromptError> {
    let mut bytes = Vec::new();
    for block in blocks {
        bytes
            .len()
            .checked_add(block.bytes().len())
            .ok_or(RetrievedPromptError::PromptSizeOverflow)?;
        bytes.extend_from_slice(block.bytes().as_bytes());
    }
    let mut matching = exact_sources.iter().copied().filter(|source| match tail {
        CompletionPromptTail::LiveManuscript {
            source_revision_id, ..
        } => source.revision_id() == Some(source_revision_id),
        CompletionPromptTail::AdmittedAssembly { assembly_id, .. } => {
            source.assembly_id() == Some(assembly_id)
        }
    });
    let source = matching
        .next()
        .ok_or(RetrievedPromptError::MissingTailSource)?;
    if matching.next().is_some() {
        return Err(RetrievedPromptError::DuplicateTailSource);
    }
    if BlobId::digest(source.bytes()) != tail.source_blob_id() {
        return Err(RetrievedPromptError::TailBlobMismatch);
    }
    let tail_bytes = tail
        .range()
        .checked_str(source.bytes())
        .map_err(|_| RetrievedPromptError::InvalidTailRange)?;
    bytes
        .len()
        .checked_add(tail_bytes.len())
        .ok_or(RetrievedPromptError::PromptSizeOverflow)?;
    bytes.extend_from_slice(tail_bytes.as_bytes());
    Ok(bytes)
}

fn bind_treatment_recipe(base: BlobId, selection: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(RETRIEVAL_RECIPE_DOMAIN);
    digest.update(base.as_bytes());
    digest.update(selection.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_source_set(selection: BlobId, prompt: BlobId, excerpts: &[ExactExcerpt]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/verified-prompt-apprenticeship-set/v1\0");
    digest.update(selection.as_bytes());
    digest.update(prompt.as_bytes());
    digest.update((excerpts.len() as u64).to_be_bytes());
    for excerpt in excerpts {
        let identity = excerpt.identity();
        digest.update(identity.source_artifact_id.as_ulid().to_bytes());
        digest.update(identity.source_revision_id.as_ulid().to_bytes());
        digest.update(identity.source_blob_id.as_bytes());
        digest.update(identity.source_range.start().to_be_bytes());
        digest.update(identity.source_range.end_exclusive().to_be_bytes());
        digest.update(identity.excerpt_hash.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_anti_copy_evidence(
    source_set: BlobId,
    report: &AntiCopyReport,
    external_semantic_scores: &[ExternalSemanticSimilarity],
    config: AntiCopyConfig,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/prompt-bound-anti-copy-evidence/v1\0");
    digest.update(source_set.as_bytes());
    digest.update(report.candidate_hash.as_bytes());
    digest.update((config.exact_shingle_words() as u64).to_be_bytes());
    digest.update((config.fuzzy_shingle_words() as u64).to_be_bytes());
    digest.update(config.fuzzy_threshold().millionths().to_be_bytes());
    digest.update(config.semantic_threshold().millionths().to_be_bytes());
    let mut semantic_scores = external_semantic_scores.to_vec();
    semantic_scores.sort_unstable_by_key(|score| score.excerpt);
    digest.update((semantic_scores.len() as u64).to_be_bytes());
    for score in semantic_scores {
        let excerpt = score.excerpt;
        digest.update(excerpt.source_artifact_id.as_ulid().to_bytes());
        digest.update(excerpt.source_revision_id.as_ulid().to_bytes());
        digest.update(excerpt.source_blob_id.as_bytes());
        digest.update(excerpt.source_range.start().to_be_bytes());
        digest.update(excerpt.source_range.end_exclusive().to_be_bytes());
        digest.update(excerpt.excerpt_hash.as_bytes());
        digest.update(score.evidence_artifact_id.as_ulid().to_bytes());
        digest.update(score.similarity.millionths().to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use loom_research_types::{CampaignId, StageAttemptId, StageId, TrialCaseId};
    use loom_types::{ArtifactId, RevisionId};

    use super::*;
    use crate::{
        ExactTokenCount, RankingWeights, RetrievalCandidate, RetrievalQuery, SelectionBudget,
        SourceByteRange, UnitScore, select_diversified,
    };

    fn blob(byte: u8) -> BlobId {
        BlobId::from_bytes([byte; 32])
    }

    fn scope() -> CallScope {
        CallScope::new(
            CampaignId::new(),
            StageId::new(),
            StageAttemptId::new(),
            TrialCaseId::new(),
        )
    }

    fn selection(
        source_revision_id: RevisionId,
        source: &[u8],
        start: usize,
        end: usize,
    ) -> DiversifiedSelection {
        let text = std::str::from_utf8(&source[start..end]).unwrap();
        let excerpt = ExactExcerpt::new(
            ArtifactId::new(),
            source_revision_id,
            BlobId::digest(source),
            SourceByteRange::new(start as u64, end as u64).unwrap(),
            text,
        )
        .unwrap();
        let token_count = ExactTokenCount::new(
            blob(77),
            excerpt.identity().excerpt_hash,
            u32::try_from(text.split_whitespace().count()).unwrap(),
        )
        .unwrap();
        let candidate =
            RetrievalCandidate::new(excerpt, token_count, Some(UnitScore::ONE), None, Vec::new())
                .unwrap();
        select_diversified(
            &[candidate],
            &RetrievalQuery::new(
                Vec::new(),
                RankingWeights::new(
                    UnitScore::ONE,
                    UnitScore::ZERO,
                    UnitScore::ZERO,
                    UnitScore::ZERO,
                )
                .unwrap(),
            )
            .unwrap(),
            SelectionBudget::new(blob(77), 100, 1_000, 1).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn exact_source_selection_compiles_and_tail_is_last() {
        let apprenticeship = b"Borrowed opening.\n\nA rain-polished street.\n\n";
        let apprenticeship_revision = RevisionId::new();
        let live = b"Mara put her hand on the door and listened.";
        let live_revision = RevisionId::new();
        let selected = selection(
            apprenticeship_revision,
            apprenticeship,
            19,
            apprenticeship.len(),
        );
        let tail = CompletionPromptTail::live_manuscript(
            live_revision,
            BlobId::digest(live),
            NonEmptyByteRange::new(0, live.len() as u64).unwrap(),
        )
        .unwrap();
        let sources = [
            ExactPromptSource::new(apprenticeship_revision, apprenticeship),
            ExactPromptSource::new(live_revision, live),
        ];
        let prepared = RetrievedPromptPlan::new(
            ProjectId::new(),
            scope(),
            blob(9),
            Vec::new(),
            selected,
            tail,
        )
        .unwrap()
        .compile(&sources)
        .unwrap();
        let (prompt, source_set, witness) = prepared.into_parts();
        assert_eq!(
            prompt.exact_bytes(),
            b"A rain-polished street.\n\nMara put her hand on the door and listened."
        );
        assert!(prompt.exact_bytes().ends_with(live));
        assert_eq!(source_set.exact_excerpts().len(), 1);
        assert_eq!(witness.compiled_prompt_fingerprint(), prompt.fingerprint());
        assert_eq!(
            witness.source_set_fingerprint(),
            source_set.source_set_fingerprint()
        );
    }

    #[test]
    fn claimed_excerpt_not_equal_to_full_source_range_fails_closed() {
        let source = b"alpha beta gamma";
        let revision = RevisionId::new();
        let selected = selection(revision, source, 0, 5);
        let changed = b"ALPHA beta gamma";
        let live = b"tail";
        let live_revision = RevisionId::new();
        let tail = CompletionPromptTail::live_manuscript(
            live_revision,
            BlobId::digest(live),
            NonEmptyByteRange::new(0, live.len() as u64).unwrap(),
        )
        .unwrap();
        let sources = [
            ExactPromptSource::new(revision, changed),
            ExactPromptSource::new(live_revision, live),
        ];
        let error = RetrievedPromptPlan::new(
            ProjectId::new(),
            scope(),
            blob(1),
            Vec::new(),
            selected,
            tail,
        )
        .unwrap()
        .compile(&sources)
        .unwrap_err();
        assert!(matches!(
            error,
            RetrievedPromptError::SourceBlobMismatch { .. }
        ));
    }

    #[test]
    fn anti_copy_set_survives_prompt_consumption_and_rejects_copy() {
        let apprenticeship = b"amber birch cedar dogwood elm fir granite hemlock iron juniper kiln linden maple nettle oak pine";
        let revision = RevisionId::new();
        let selected = selection(revision, apprenticeship, 0, apprenticeship.len());
        let live = b"She remembered";
        let live_revision = RevisionId::new();
        let tail = CompletionPromptTail::live_manuscript(
            live_revision,
            BlobId::digest(live),
            NonEmptyByteRange::new(0, live.len() as u64).unwrap(),
        )
        .unwrap();
        let sources = [
            ExactPromptSource::new(revision, apprenticeship),
            ExactPromptSource::new(live_revision, live),
        ];
        let (_, source_set, _) = RetrievedPromptPlan::new(
            ProjectId::new(),
            scope(),
            blob(2),
            Vec::new(),
            selected,
            tail,
        )
        .unwrap()
        .compile(&sources)
        .unwrap()
        .into_parts();
        let report = source_set
            .analyze_candidate(
                "elm fir granite hemlock iron juniper kiln linden maple nettle oak pine",
                &[],
                AntiCopyConfig::default(),
            )
            .unwrap();
        assert!(report.any_suspect());
        assert!(!report.semantic_evidence_complete());
    }

    #[test]
    fn prompt_bound_report_distinguishes_complete_semantic_evidence() {
        let apprenticeship = b"a river under glass";
        let revision = RevisionId::new();
        let selected = selection(revision, apprenticeship, 0, apprenticeship.len());
        let live = b"She remembered";
        let live_revision = RevisionId::new();
        let tail = CompletionPromptTail::live_manuscript(
            live_revision,
            BlobId::digest(live),
            NonEmptyByteRange::new(0, live.len() as u64).unwrap(),
        )
        .unwrap();
        let sources = [
            ExactPromptSource::new(revision, apprenticeship),
            ExactPromptSource::new(live_revision, live),
        ];
        let (_, source_set, _) = RetrievedPromptPlan::new(
            ProjectId::new(),
            scope(),
            blob(3),
            Vec::new(),
            selected,
            tail,
        )
        .unwrap()
        .compile(&sources)
        .unwrap()
        .into_parts();
        let semantic = ExternalSemanticSimilarity {
            excerpt: source_set.exact_excerpts()[0].identity(),
            evidence_artifact_id: ArtifactId::new(),
            similarity: UnitScore::ZERO,
        };
        let report = source_set
            .analyze_candidate(
                "entirely different words",
                &[semantic],
                AntiCopyConfig::default(),
            )
            .unwrap();
        assert!(report.semantic_evidence_complete());
        assert_eq!(report.semantic_evidence_count(), 1);
        assert!(!report.any_suspect());
    }
}
