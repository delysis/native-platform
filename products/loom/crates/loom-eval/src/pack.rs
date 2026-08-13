use std::{collections::BTreeMap, fmt};

use loom_research_types::{
    CompiledManifest, MAX_CRITERIA, ManifestCompileError, ManifestDocument, ManifestFormat,
    compile_manifest,
};
use loom_search::{CriterionWeight, Rubric, RubricCriterion, SCORE_SCALE};
use loom_types::{ArtifactId, BlobId};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

const CORE_SOURCE: &[u8] = include_bytes!("../packs/fiction-core-v1.toml");
const CORE_PLACEHOLDER: &str = "{{CORE_ARTIFACT_SHA256}}";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BuiltInGenreFunction {
    IntimateRomanticTension,
    SuspenseThrillerCausality,
    SpeculativeWorldConsistency,
    MysteryRevealLogic,
    VoiceHeavyLiteraryCharacterWork,
}

impl BuiltInGenreFunction {
    pub const ALL: [Self; 5] = [
        Self::IntimateRomanticTension,
        Self::SuspenseThrillerCausality,
        Self::SpeculativeWorldConsistency,
        Self::MysteryRevealLogic,
        Self::VoiceHeavyLiteraryCharacterWork,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::IntimateRomanticTension => "intimate_romantic_tension",
            Self::SuspenseThrillerCausality => "suspense_thriller_causality",
            Self::SpeculativeWorldConsistency => "speculative_world_consistency",
            Self::MysteryRevealLogic => "mystery_reveal_logic",
            Self::VoiceHeavyLiteraryCharacterWork => "voice_heavy_literary_character_work",
        }
    }

    const fn template(self) -> &'static str {
        match self {
            Self::IntimateRomanticTension => {
                include_str!("../packs/intimate-romantic-tension-v1.toml.in")
            }
            Self::SuspenseThrillerCausality => {
                include_str!("../packs/suspense-thriller-causality-v1.toml.in")
            }
            Self::SpeculativeWorldConsistency => {
                include_str!("../packs/speculative-world-consistency-v1.toml.in")
            }
            Self::MysteryRevealLogic => {
                include_str!("../packs/mystery-reveal-logic-v1.toml.in")
            }
            Self::VoiceHeavyLiteraryCharacterWork => {
                include_str!("../packs/voice-literary-character-v1.toml.in")
            }
        }
    }
}

impl fmt::Display for BuiltInGenreFunction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledFictionCriterion {
    key: String,
    label: String,
    description: String,
    weight_bits: u64,
    behavioral_anchors: Vec<String>,
    tags: Vec<String>,
}

impl CompiledFictionCriterion {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn weight(&self) -> f64 {
        f64::from_bits(self.weight_bits)
    }

    pub fn behavioral_anchors(&self) -> &[String] {
        &self.behavioral_anchors
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}

/// Fully linked immutable rubric pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FictionEvaluationPack {
    core_artifact_hash: BlobId,
    genre_artifact_hash: Option<BlobId>,
    genre_function: Option<String>,
    criteria: Vec<CompiledFictionCriterion>,
    fingerprint: BlobId,
}

impl FictionEvaluationPack {
    pub const fn core_artifact_hash(&self) -> BlobId {
        self.core_artifact_hash
    }

    pub const fn genre_artifact_hash(&self) -> Option<BlobId> {
        self.genre_artifact_hash
    }

    pub fn genre_function(&self) -> Option<&str> {
        self.genre_function.as_deref()
    }

    pub fn criteria(&self) -> &[CompiledFictionCriterion] {
        &self.criteria
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn criterion(&self, key: &str) -> Option<&CompiledFictionCriterion> {
        self.criteria.iter().find(|criterion| criterion.key == key)
    }

    pub fn search_rubric(&self) -> Result<Rubric, EvaluationPackError> {
        let maximum = self
            .criteria
            .iter()
            .map(CompiledFictionCriterion::weight)
            .fold(0.0_f64, f64::max);
        if !maximum.is_finite() || maximum <= 0.0 {
            return Err(EvaluationPackError::InvalidWeight);
        }
        let criteria = self
            .criteria
            .iter()
            .map(|criterion| -> Result<_, EvaluationPackError> {
                let scaled = ((criterion.weight() / maximum) * f64::from(SCORE_SCALE)).round();
                let scaled = rounded_weight(scaled.clamp(1.0, f64::from(SCORE_SCALE)))?;
                let id = derive_artifact_id(
                    b"loom/evaluation-criterion-id/v1\0",
                    self.fingerprint,
                    criterion.key.as_bytes(),
                );
                Ok(RubricCriterion::new(id, CriterionWeight::new(scaled)?))
            })
            .collect::<Result<Vec<_>, EvaluationPackError>>()?;
        let rubric_id = derive_artifact_id(
            b"loom/evaluation-rubric-id/v1\0",
            self.fingerprint,
            b"rubric",
        );
        Ok(Rubric::new(rubric_id, criteria)?)
    }
}

pub fn compile_fiction_evaluation_pack(
    core: &CompiledManifest,
    genre: Option<&CompiledManifest>,
) -> Result<FictionEvaluationPack, EvaluationPackError> {
    core.verify_integrity()?;
    let ManifestDocument::CorePack(core_document) = core.document() else {
        return Err(EvaluationPackError::ExpectedCorePack);
    };
    let core_hash = core.artifact_hash().as_blob_id();
    let mut criteria = BTreeMap::<String, CompiledFictionCriterion>::new();
    for criterion in core_document.criteria() {
        criteria.insert(
            criterion.id.as_str().to_owned(),
            CompiledFictionCriterion {
                key: criterion.id.as_str().to_owned(),
                label: criterion.label.as_str().to_owned(),
                description: criterion.description.as_str().to_owned(),
                weight_bits: criterion.weight.to_bits(),
                behavioral_anchors: criterion
                    .behavioral_anchors
                    .iter()
                    .map(|anchor| anchor.as_str().to_owned())
                    .collect(),
                tags: criterion
                    .tags
                    .iter()
                    .map(|tag| tag.as_str().to_owned())
                    .collect(),
            },
        );
    }

    let mut genre_hash = None;
    let mut genre_function = None;
    if let Some(genre) = genre {
        genre.verify_integrity()?;
        let ManifestDocument::GenrePack(genre_document) = genre.document() else {
            return Err(EvaluationPackError::ExpectedGenrePack);
        };
        if genre_document.core_pack().format != ManifestFormat::CorePackV1
            || genre_document.core_pack().artifact_sha256 != core_hash
        {
            return Err(EvaluationPackError::CoreReferenceMismatch);
        }
        if genre_document.genre_functions().len() != 1 {
            return Err(EvaluationPackError::ExpectedOneGenreFunction);
        }
        genre_function = genre_document
            .genre_functions()
            .iter()
            .next()
            .map(|function| function.as_str().to_owned());
        for (key, override_spec) in genre_document.criteria().iter() {
            let key = key.as_str().to_owned();
            let multiplier = override_spec.weight_multiplier.get();
            let previous_weight = criteria
                .get(&key)
                .map_or(1.0, CompiledFictionCriterion::weight);
            let weight = previous_weight * multiplier;
            if !weight.is_finite() || weight <= 0.0 {
                return Err(EvaluationPackError::InvalidWeight);
            }
            let previous_label = criteria.get(&key).map_or_else(
                || key.replace('_', " "),
                |criterion| criterion.label.clone(),
            );
            let previous_tags = criteria
                .get(&key)
                .map_or_else(Vec::new, |criterion| criterion.tags.clone());
            criteria.insert(
                key.clone(),
                CompiledFictionCriterion {
                    key,
                    label: previous_label,
                    description: override_spec.description.as_str().to_owned(),
                    weight_bits: weight.to_bits(),
                    behavioral_anchors: override_spec
                        .behavioral_anchors
                        .iter()
                        .map(|anchor| anchor.as_str().to_owned())
                        .collect(),
                    tags: previous_tags,
                },
            );
        }
        genre_hash = Some(genre.artifact_hash().as_blob_id());
    }
    if criteria.is_empty() || criteria.len() > MAX_CRITERIA {
        return Err(EvaluationPackError::InvalidCriterionCount(criteria.len()));
    }
    let fingerprint = fingerprint_pack(core_hash, genre_hash);
    Ok(FictionEvaluationPack {
        core_artifact_hash: core_hash,
        genre_artifact_hash: genre_hash,
        genre_function,
        criteria: criteria.into_values().collect(),
        fingerprint,
    })
}

pub fn built_in_core_manifest() -> Result<CompiledManifest, ManifestCompileError> {
    compile_manifest(CORE_SOURCE)
}

pub fn built_in_genre_manifest(
    function: BuiltInGenreFunction,
    core_artifact_hash: BlobId,
) -> Result<CompiledManifest, ManifestCompileError> {
    let source = function
        .template()
        .replace(CORE_PLACEHOLDER, &core_artifact_hash.to_hex());
    compile_manifest(source.as_bytes())
}

pub fn built_in_fiction_pack(
    function: BuiltInGenreFunction,
) -> Result<FictionEvaluationPack, EvaluationPackError> {
    let core = built_in_core_manifest()?;
    let genre = built_in_genre_manifest(function, core.artifact_hash().as_blob_id())?;
    compile_fiction_evaluation_pack(&core, Some(&genre))
}

fn fingerprint_pack(core: BlobId, genre: Option<BlobId>) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/fiction-evaluation-pack/v1\0");
    digest.update(core.as_bytes());
    match genre {
        Some(genre) => {
            digest.update([1]);
            digest.update(genre.as_bytes());
        }
        None => digest.update([0]),
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn rounded_weight(value: f64) -> Result<u32, EvaluationPackError> {
    if !value.is_finite() || value < 1.0 || value > f64::from(SCORE_SCALE) {
        return Err(EvaluationPackError::InvalidWeight);
    }
    format!("{value:.0}")
        .parse::<u32>()
        .map_err(|_| EvaluationPackError::InvalidWeight)
}

fn derive_artifact_id(domain: &[u8], pack: BlobId, key: &[u8]) -> ArtifactId {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(pack.as_bytes());
    digest.update((key.len() as u64).to_be_bytes());
    digest.update(key);
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ArtifactId::from_ulid(Ulid::from_bytes(bytes))
}

#[derive(Debug, Error)]
pub enum EvaluationPackError {
    #[error(transparent)]
    ManifestCompile(#[from] ManifestCompileError),
    #[error(transparent)]
    ManifestIntegrity(#[from] loom_research_types::ManifestIntegrityError),
    #[error("evaluation pack expected a loom.core-pack.v1 artifact")]
    ExpectedCorePack,
    #[error("evaluation pack expected a loom.genre-pack.v1 artifact")]
    ExpectedGenrePack,
    #[error("genre pack does not reference the exact core-pack artifact")]
    CoreReferenceMismatch,
    #[error("a reusable built-in genre pack must declare exactly one genre function")]
    ExpectedOneGenreFunction,
    #[error("linked criterion count {0} is outside 1..={MAX_CRITERIA}")]
    InvalidCriterionCount(usize),
    #[error("linked criterion has a non-finite or non-positive weight")]
    InvalidWeight,
    #[error(transparent)]
    Rubric(#[from] loom_search::RubricError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_core_has_all_ten_general_criteria() {
        let core = built_in_core_manifest().expect("core manifest");
        let pack = compile_fiction_evaluation_pack(&core, None).expect("core pack");
        assert_eq!(pack.criteria().len(), 10);
        for expected in [
            "continuity",
            "causal_intelligibility",
            "agency_and_voice",
            "movement_and_pacing",
            "prose_precision",
            "dialogue_and_subtext",
            "emotional_credibility",
            "tension_and_payoff",
            "originality",
            "engagement",
        ] {
            assert!(pack.criterion(expected).is_some(), "missing {expected}");
        }
    }

    #[test]
    fn all_five_genre_packs_link_to_the_exact_core() {
        let core = built_in_core_manifest().expect("core");
        for function in BuiltInGenreFunction::ALL {
            let genre = built_in_genre_manifest(function, core.artifact_hash().as_blob_id())
                .expect("genre");
            let pack = compile_fiction_evaluation_pack(&core, Some(&genre)).expect("linked pack");
            assert_eq!(pack.genre_function(), Some(function.id()));
            assert!(pack.criteria().len() >= 12);
            assert!(!pack.search_rubric().expect("rubric").criteria().is_empty());
        }
    }

    #[test]
    fn wrong_core_reference_fails_closed() {
        let core = built_in_core_manifest().expect("core");
        let genre = built_in_genre_manifest(
            BuiltInGenreFunction::MysteryRevealLogic,
            BlobId::digest(b"wrong core"),
        )
        .expect("standalone genre syntax");
        assert!(matches!(
            compile_fiction_evaluation_pack(&core, Some(&genre)),
            Err(EvaluationPackError::CoreReferenceMismatch)
        ));
    }

    #[test]
    fn built_in_artifacts_are_byte_reproducible() {
        let first = built_in_core_manifest().expect("first");
        let second = built_in_core_manifest().expect("second");
        assert_eq!(first.source_bytes(), second.source_bytes());
        assert_eq!(first.artifact_hash(), second.artifact_hash());

        let left =
            built_in_fiction_pack(BuiltInGenreFunction::IntimateRomanticTension).expect("left");
        let right =
            built_in_fiction_pack(BuiltInGenreFunction::IntimateRomanticTension).expect("right");
        assert_eq!(left, right);
    }
}
