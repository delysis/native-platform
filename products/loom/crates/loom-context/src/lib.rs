#![forbid(unsafe_code)]

mod anti_copy;
mod prompt_bridge;
mod retrieval;
mod text;

pub use anti_copy::{
    AntiCopyConfig, AntiCopyError, AntiCopyEvidence, AntiCopyReport, DEFAULT_EXACT_SHINGLE_WORDS,
    DEFAULT_FUZZY_SHINGLE_WORDS, ExactCopySignals, ExternalSemanticSimilarity, FuzzyCopySignals,
    MAX_ANTI_COPY_CANDIDATE_BYTES, MAX_ANTI_COPY_PROMPT_BYTES, MAX_ANTI_COPY_PROMPT_EXCERPTS,
    MAX_EXACT_SHINGLE_WORDS, MAX_FUZZY_SHINGLE_WORDS, MIN_EXACT_SHINGLE_WORDS,
    MIN_FUZZY_SHINGLE_WORDS, analyze_anti_copy,
};
pub use prompt_bridge::{
    PreparedRetrievedPrompt, PromptBoundAntiCopyReport, RetrievalPromptWitness,
    RetrievedPromptError, RetrievedPromptPlan, VerifiedPromptApprenticeshipSet,
};
pub use retrieval::{
    DiversifiedExcerpt, DiversifiedSelection, ExactExcerpt, ExactExcerptIdentity, ExactTokenCount,
    HybridScoreEvidence, MAX_CRAFT_TAG_BYTES, MAX_CRAFT_TAGS, MAX_EXCERPT_BYTES,
    MAX_EXCERPT_TOKENS, MAX_RETRIEVAL_CANDIDATES, MAX_RETRIEVAL_INPUT_BYTES,
    MAX_RETRIEVAL_SHINGLES, MAX_SELECTED_EXCERPTS, MAX_SELECTION_BYTES, MAX_SELECTION_TOKENS,
    RankingWeights, RetrievalCandidate, RetrievalError, RetrievalQuery, SCORE_SCALE,
    SelectionBudget, SourceByteRange, UnitScore, select_diversified,
};
