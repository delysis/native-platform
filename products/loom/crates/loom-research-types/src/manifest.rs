use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
};

use loom_types::BlobId;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, SeqAccess, Visitor},
    ser::{SerializeMap, SerializeSeq},
};
use thiserror::Error;

use crate::{
    BoundError, BoundedText, BoundedVec, MAX_BASE_WRITER_BATCH_CASES, MAX_GENERATED_TOKENS,
    NonEmptyBoundedVec, deserialize_blob_id, deserialize_optional_blob_id,
};

/// Maximum accepted size of one exact human-authored TOML manifest.
pub const MAX_MANIFEST_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_MANIFEST_NAME_BYTES: usize = 128;
pub const MAX_MANIFEST_KEY_BYTES: usize = 64;
pub const MAX_MANIFEST_DESCRIPTION_BYTES: usize = 2_048;
pub const MAX_MANIFEST_VALUE_BYTES: usize = 4_096;
pub const MAX_CRITERIA: usize = 128;
pub const MAX_BEHAVIORAL_ANCHORS: usize = 32;
pub const MAX_PROMPT_ROLES: usize = 32;
pub const MAX_GENRE_FUNCTIONS: usize = 32;
pub const MAX_GENRE_OVERRIDES: usize = 128;
pub const MAX_PROJECT_ANCHORS: usize = 64;
pub const MAX_MODEL_BINDINGS: usize = 32;
pub const MAX_MODEL_CAPABILITIES: usize = 64;
pub const MAX_ADAPTERS: usize = 16;
pub const MAX_CAMPAIGN_CASES: usize = 1_024;
pub const MAX_TREATMENTS: usize = 256;
pub const MAX_CONTROL_PARAMETERS: usize = 64;
/// Per-call output cannot exceed the shared exact-evidence token bound.
pub const MAX_TREATMENT_OUTPUT_TOKENS: u32 = MAX_GENERATED_TOKENS;
/// `top_k` is transported to the native sampler as a non-negative `i32`.
/// The tighter evidence ceiling also prevents nonsensical multi-billion-entry
/// requests before a model vocabulary is available.
pub const MAX_SAMPLER_TOP_K: u32 = MAX_GENERATED_TOKENS;
/// Global token ceiling shared with the search budget contract.
pub const MAX_CAMPAIGN_TOKEN_BUDGET: u64 = 10_000_000_000;
/// Global evaluation ceiling shared with the search branch budget contract.
pub const MAX_CAMPAIGN_EVALUATIONS: u32 = 100_000;
pub const MAX_BENCHMARK_CONTENDERS: usize = 32;
pub const MAX_BENCHMARK_FUNCTIONS: usize = 32;
pub const MAX_BENCHMARK_CASES_PER_FUNCTION: usize = 256;
pub const MAX_N_CURVE_POINTS: usize = 16;

const CANONICAL_MANIFEST_DOMAIN: &[u8] = b"loom.manifest.semantic.v1\0";

const _: () = assert!(MAX_BASE_WRITER_BATCH_CASES <= u16::MAX as usize);
const _: () = assert!(MAX_SAMPLER_TOP_K <= i32::MAX as u32);

pub type ManifestName = BoundedText<MAX_MANIFEST_NAME_BYTES>;
pub type ManifestKey = BoundedText<MAX_MANIFEST_KEY_BYTES>;
pub type ManifestDescription = BoundedText<MAX_MANIFEST_DESCRIPTION_BYTES>;
pub type ManifestValue = BoundedText<MAX_MANIFEST_VALUE_BYTES>;

/// A map whose entry limit is enforced by every Serde deserialization path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedMap<K, V, const MAX: usize>(BTreeMap<K, V>);

impl<K: Ord, V, const MAX: usize> BoundedMap<K, V, MAX> {
    pub fn new(values: BTreeMap<K, V>) -> Result<Self, BoundError> {
        if values.len() > MAX {
            return Err(BoundError::TooMany {
                actual: values.len(),
                maximum: MAX,
            });
        }
        Ok(Self(values))
    }

    pub fn into_inner(self) -> BTreeMap<K, V> {
        self.0
    }
}

impl<K, V, const MAX: usize> Deref for BoundedMap<K, V, MAX> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Serialize, V: Serialize, const MAX: usize> Serialize for BoundedMap<K, V, MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, K, V, const MAX: usize> Deserialize<'de> for BoundedMap<K, V, MAX>
where
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(BoundedMapVisitor::<K, V, MAX>(std::marker::PhantomData))
    }
}

struct BoundedMapVisitor<K, V, const MAX: usize>(std::marker::PhantomData<(K, V)>);

impl<'de, K, V, const MAX: usize> Visitor<'de> for BoundedMapVisitor<K, V, MAX>
where
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    type Value = BoundedMap<K, V, MAX>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a map containing at most {MAX} entries")
    }

    fn visit_map<A>(self, mut input: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        if input.size_hint().is_some_and(|size| size > MAX) {
            return Err(de::Error::invalid_length(
                input.size_hint().unwrap_or(MAX.saturating_add(1)),
                &self,
            ));
        }
        let mut values = BTreeMap::new();
        while values.len() < MAX {
            let Some((key, value)) = input.next_entry()? else {
                return Ok(BoundedMap(values));
            };
            if values.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate map key"));
            }
        }
        if input
            .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
            .is_some()
        {
            return Err(de::Error::invalid_length(MAX.saturating_add(1), &self));
        }
        Ok(BoundedMap(values))
    }
}

/// A bounded, duplicate-free collection whose order is explicitly semantic-free.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedSet<T, const MAX: usize>(BTreeSet<T>);

impl<T: Ord, const MAX: usize> BoundedSet<T, MAX> {
    pub fn new(values: impl IntoIterator<Item = T>) -> Result<Self, BoundError> {
        let values = values.into_iter().collect::<BTreeSet<_>>();
        if values.len() > MAX {
            return Err(BoundError::TooMany {
                actual: values.len(),
                maximum: MAX,
            });
        }
        Ok(Self(values))
    }

    pub fn into_inner(self) -> BTreeSet<T> {
        self.0
    }
}

impl<T, const MAX: usize> Deref for BoundedSet<T, MAX> {
    type Target = BTreeSet<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Serialize, const MAX: usize> Serialize for BoundedSet<T, MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for value in &self.0 {
            sequence.serialize_element(value)?;
        }
        sequence.end()
    }
}

impl<'de, T, const MAX: usize> Deserialize<'de> for BoundedSet<T, MAX>
where
    T: Deserialize<'de> + Ord,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = deserializer.deserialize_seq(BoundedSetVisitor::<T, MAX> {
            require_non_empty: false,
            marker: std::marker::PhantomData,
        })?;
        Ok(Self(values))
    }
}

/// A non-empty bounded set. Source order is discarded by design.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonEmptyBoundedSet<T, const MAX: usize>(BTreeSet<T>);

impl<T: Ord, const MAX: usize> NonEmptyBoundedSet<T, MAX> {
    pub fn new(values: impl IntoIterator<Item = T>) -> Result<Self, BoundError> {
        let values = values.into_iter().collect::<BTreeSet<_>>();
        if values.is_empty() {
            return Err(BoundError::Empty);
        }
        if values.len() > MAX {
            return Err(BoundError::TooMany {
                actual: values.len(),
                maximum: MAX,
            });
        }
        Ok(Self(values))
    }

    pub fn into_inner(self) -> BTreeSet<T> {
        self.0
    }
}

impl<T, const MAX: usize> Deref for NonEmptyBoundedSet<T, MAX> {
    type Target = BTreeSet<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Serialize, const MAX: usize> Serialize for NonEmptyBoundedSet<T, MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for value in &self.0 {
            sequence.serialize_element(value)?;
        }
        sequence.end()
    }
}

impl<'de, T, const MAX: usize> Deserialize<'de> for NonEmptyBoundedSet<T, MAX>
where
    T: Deserialize<'de> + Ord,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = deserializer.deserialize_seq(BoundedSetVisitor::<T, MAX> {
            require_non_empty: true,
            marker: std::marker::PhantomData,
        })?;
        Ok(Self(values))
    }
}

struct BoundedSetVisitor<T, const MAX: usize> {
    require_non_empty: bool,
    marker: std::marker::PhantomData<T>,
}

impl<'de, T, const MAX: usize> Visitor<'de> for BoundedSetVisitor<T, MAX>
where
    T: Deserialize<'de> + Ord,
{
    type Value = BTreeSet<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.require_non_empty {
            write!(formatter, "one to {MAX} unique entries")
        } else {
            write!(formatter, "at most {MAX} unique entries")
        }
    }

    fn visit_seq<A>(self, mut input: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if input.size_hint().is_some_and(|size| size > MAX) {
            return Err(de::Error::invalid_length(
                input.size_hint().unwrap_or(MAX.saturating_add(1)),
                &self,
            ));
        }
        let mut values = BTreeSet::new();
        while values.len() < MAX {
            let Some(value) = input.next_element()? else {
                if self.require_non_empty && values.is_empty() {
                    return Err(de::Error::invalid_length(0, &self));
                }
                return Ok(values);
            };
            if !values.insert(value) {
                return Err(de::Error::custom("duplicate set entry"));
            }
        }
        if input.next_element::<de::IgnoredAny>()?.is_some() {
            return Err(de::Error::invalid_length(MAX.saturating_add(1), &self));
        }
        Ok(values)
    }
}

/// A finite `f64` whose equality and hashing preserve its exact IEEE-754 bits.
#[derive(Clone, Copy)]
pub struct FiniteF64(f64);

impl FiniteF64 {
    pub fn new(value: f64) -> Result<Self, ManifestCompileError> {
        if !value.is_finite() {
            return Err(ManifestCompileError::NonFiniteFloat { location: None });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> f64 {
        self.0
    }

    pub const fn to_bits(self) -> u64 {
        self.0.to_bits()
    }
}

impl fmt::Debug for FiniteF64 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FiniteF64")
            .field("value", &self.0)
            .field("bits", &format_args!("{:#018x}", self.to_bits()))
            .finish()
    }
}

impl PartialEq for FiniteF64 {
    fn eq(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl Eq for FiniteF64 {}

impl PartialOrd for FiniteF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FiniteF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for FiniteF64 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.to_bits().hash(state);
    }
}

impl Serialize for FiniteF64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for FiniteF64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_f64(FiniteF64Visitor)
    }
}

struct FiniteF64Visitor;

impl Visitor<'_> for FiniteF64Visitor {
    type Value = FiniteF64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a finite IEEE-754 double")
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.is_finite() {
            Ok(FiniteF64(value))
        } else {
            Err(E::custom("non-finite floating-point value"))
        }
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_type(de::Unexpected::Signed(value), &self))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_type(de::Unexpected::Unsigned(value), &self))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ManifestFormat {
    #[serde(rename = "loom.core-pack.v1")]
    CorePackV1,
    #[serde(rename = "loom.genre-pack.v1")]
    GenrePackV1,
    #[serde(rename = "loom.model-bindings.v1")]
    ModelBindingsV1,
    #[serde(rename = "loom.campaign.v1")]
    CampaignV1,
    #[serde(rename = "loom.benchmark.v1")]
    BenchmarkV1,
}

impl ManifestFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CorePackV1 => "loom.core-pack.v1",
            Self::GenrePackV1 => "loom.genre-pack.v1",
            Self::ModelBindingsV1 => "loom.model-bindings.v1",
            Self::CampaignV1 => "loom.campaign.v1",
            Self::BenchmarkV1 => "loom.benchmark.v1",
        }
    }
}

impl fmt::Display for ManifestFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    pub format: ManifestFormat,
    #[serde(deserialize_with = "deserialize_blob_id")]
    pub artifact_sha256: BlobId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionSpec {
    pub id: ManifestKey,
    pub label: ManifestName,
    pub description: ManifestDescription,
    pub weight: FiniteF64,
    pub behavioral_anchors: NonEmptyBoundedVec<ManifestValue, MAX_BEHAVIORAL_ANCHORS>,
    pub tags: BoundedSet<ManifestKey, MAX_GENRE_FUNCTIONS>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromptRoleSpec {
    pub description: ManifestDescription,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CorePackManifestWire {
    format: ManifestFormat,
    name: ManifestName,
    description: ManifestDescription,
    criteria: NonEmptyBoundedVec<CriterionSpec, MAX_CRITERIA>,
    prompt_roles: BoundedMap<ManifestKey, PromptRoleSpec, MAX_PROMPT_ROLES>,
}

/// A validated core-pack compiler output.
///
/// Top-level manifests deliberately do not implement `Deserialize`; parsing
/// must pass through [`compile_manifest`] so exact-source limits and semantic
/// validation cannot be skipped.
///
/// ```compile_fail
/// use loom_research_types::CorePackManifest;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<CorePackManifest>();
/// ```
///
/// Its fields are also not publicly constructible or destructurable.
///
/// ```compile_fail
/// use loom_research_types::CorePackManifest;
/// fn bypass(value: CorePackManifest) {
///     let CorePackManifest(inner) = value;
///     let _ = inner;
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CorePackManifest(CorePackManifestWire);

impl CorePackManifest {
    pub const fn format(&self) -> ManifestFormat {
        self.0.format
    }

    pub const fn name(&self) -> &ManifestName {
        &self.0.name
    }

    pub const fn description(&self) -> &ManifestDescription {
        &self.0.description
    }

    pub fn criteria(&self) -> &[CriterionSpec] {
        &self.0.criteria
    }

    pub const fn prompt_roles(&self) -> &BoundedMap<ManifestKey, PromptRoleSpec, MAX_PROMPT_ROLES> {
        &self.0.prompt_roles
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionOverride {
    pub description: ManifestDescription,
    pub weight_multiplier: FiniteF64,
    pub behavioral_anchors: NonEmptyBoundedVec<ManifestValue, MAX_BEHAVIORAL_ANCHORS>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAnchorSpec {
    pub description: ManifestDescription,
    pub retrieval_limit: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GenrePackManifestWire {
    format: ManifestFormat,
    name: ManifestName,
    description: ManifestDescription,
    core_pack: ArtifactReference,
    genre_functions: NonEmptyBoundedSet<ManifestKey, MAX_GENRE_FUNCTIONS>,
    criteria: BoundedMap<ManifestKey, CriterionOverride, MAX_GENRE_OVERRIDES>,
    project_anchors: BoundedMap<ManifestKey, ProjectAnchorSpec, MAX_PROJECT_ANCHORS>,
}

/// A validated genre-pack compiler output.
///
/// ```compile_fail
/// use loom_research_types::GenrePackManifest;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<GenrePackManifest>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct GenrePackManifest(GenrePackManifestWire);

impl GenrePackManifest {
    pub const fn format(&self) -> ManifestFormat {
        self.0.format
    }

    pub const fn name(&self) -> &ManifestName {
        &self.0.name
    }

    pub const fn description(&self) -> &ManifestDescription {
        &self.0.description
    }

    pub const fn core_pack(&self) -> &ArtifactReference {
        &self.0.core_pack
    }

    pub const fn genre_functions(&self) -> &NonEmptyBoundedSet<ManifestKey, MAX_GENRE_FUNCTIONS> {
        &self.0.genre_functions
    }

    pub const fn criteria(
        &self,
    ) -> &BoundedMap<ManifestKey, CriterionOverride, MAX_GENRE_OVERRIDES> {
        &self.0.criteria
    }

    pub const fn project_anchors(
        &self,
    ) -> &BoundedMap<ManifestKey, ProjectAnchorSpec, MAX_PROJECT_ANCHORS> {
        &self.0.project_anchors
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    BaseWriter,
    Controller,
    Critic,
    Embedder,
    Ranker,
    RewardModel,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterBinding {
    #[serde(deserialize_with = "deserialize_blob_id")]
    pub artifact_sha256: BlobId,
    pub scale: FiniteF64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBindingSpec {
    pub id: ManifestKey,
    pub role: ModelRole,
    #[serde(deserialize_with = "deserialize_blob_id")]
    pub model_sha256: BlobId,
    pub model_bytes: u64,
    #[serde(deserialize_with = "deserialize_blob_id")]
    pub tokenizer_sha256: BlobId,
    #[serde(default, deserialize_with = "deserialize_optional_blob_id")]
    pub multimodal_projector_sha256: Option<BlobId>,
    pub architecture: ManifestKey,
    pub context_tokens: u32,
    pub capabilities: NonEmptyBoundedSet<ManifestKey, MAX_MODEL_CAPABILITIES>,
    pub adapters: BoundedVec<AdapterBinding, MAX_ADAPTERS>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ModelBindingsManifestWire {
    format: ManifestFormat,
    name: ManifestName,
    description: ManifestDescription,
    bindings: NonEmptyBoundedVec<ModelBindingSpec, MAX_MODEL_BINDINGS>,
}

/// A validated model-bindings compiler output.
///
/// ```compile_fail
/// use loom_research_types::ModelBindingsManifest;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<ModelBindingsManifest>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ModelBindingsManifest(ModelBindingsManifestWire);

impl ModelBindingsManifest {
    pub const fn format(&self) -> ManifestFormat {
        self.0.format
    }

    pub const fn name(&self) -> &ManifestName {
        &self.0.name
    }

    pub const fn description(&self) -> &ManifestDescription {
        &self.0.description
    }

    pub fn bindings(&self) -> &[ModelBindingSpec] {
        &self.0.bindings
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignCaseSpec {
    pub id: ManifestKey,
    pub genre_function: ManifestKey,
    #[serde(deserialize_with = "deserialize_blob_id")]
    pub source_sha256: BlobId,
    pub max_context_tokens: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTopology {
    ExactDirectContinuation,
    NaturalBookfrontContinuation,
    EventLedgerOperatorPair,
    NearestProjectAnchor,
    RawSceneApprenticeship,
    GraphRawPairedApprenticeship,
    StagedMovementAssembly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SamplerTreatment {
    pub temperature: FiniteF64,
    pub top_k: u32,
    pub top_p: FiniteF64,
    pub min_p: FiniteF64,
    pub typical_p: FiniteF64,
    pub repetition_penalty: FiniteF64,
    pub cfg_scale: Option<FiniteF64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TreatmentSpec {
    pub id: ManifestKey,
    pub prompt_topology: PromptTopology,
    pub samples_per_case: u16,
    pub max_output_tokens: u32,
    pub sampler: SamplerTreatment,
    pub control_parameters: BoundedMap<ManifestKey, FiniteF64, MAX_CONTROL_PARAMETERS>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignBudget {
    pub max_writer_tokens: u64,
    pub max_controller_tokens: u64,
    pub max_evaluations: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    CompleteSegmentRerank,
    SuccessiveHalving,
    MapElites,
    SealedComparison,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CampaignManifestWire {
    format: ManifestFormat,
    name: ManifestName,
    description: ManifestDescription,
    core_pack: ArtifactReference,
    genre_pack: ArtifactReference,
    model_bindings: ArtifactReference,
    seed: u64,
    cases: NonEmptyBoundedVec<CampaignCaseSpec, MAX_CAMPAIGN_CASES>,
    treatments: NonEmptyBoundedVec<TreatmentSpec, MAX_TREATMENTS>,
    budget: CampaignBudget,
    selection: SelectionPolicy,
}

/// A validated campaign compiler output.
///
/// ```compile_fail
/// use loom_research_types::CampaignManifest;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<CampaignManifest>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CampaignManifest(CampaignManifestWire);

impl CampaignManifest {
    pub const fn format(&self) -> ManifestFormat {
        self.0.format
    }

    pub const fn name(&self) -> &ManifestName {
        &self.0.name
    }

    pub const fn description(&self) -> &ManifestDescription {
        &self.0.description
    }

    pub const fn core_pack(&self) -> &ArtifactReference {
        &self.0.core_pack
    }

    pub const fn genre_pack(&self) -> &ArtifactReference {
        &self.0.genre_pack
    }

    pub const fn model_bindings(&self) -> &ArtifactReference {
        &self.0.model_bindings
    }

    pub const fn seed(&self) -> u64 {
        self.0.seed
    }

    pub fn cases(&self) -> &[CampaignCaseSpec] {
        &self.0.cases
    }

    pub fn treatments(&self) -> &[TreatmentSpec] {
        &self.0.treatments
    }

    pub const fn budget(&self) -> &CampaignBudget {
        &self.0.budget
    }

    pub const fn selection(&self) -> SelectionPolicy {
        self.0.selection
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkContender {
    pub id: ManifestKey,
    #[serde(deserialize_with = "deserialize_blob_id")]
    pub profile_sha256: BlobId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkFunction {
    pub id: ManifestKey,
    pub case_ids: NonEmptyBoundedSet<ManifestKey, MAX_BENCHMARK_CASES_PER_FUNCTION>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkReview {
    pub frontier_model: ManifestKey,
    pub fresh_runs: u8,
    pub order_permutation_cells: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkManifestWire {
    format: ManifestFormat,
    name: ManifestName,
    description: ManifestDescription,
    campaign: ArtifactReference,
    seed: u64,
    nested_n: NonEmptyBoundedSet<u32, MAX_N_CURVE_POINTS>,
    contenders: NonEmptyBoundedVec<BenchmarkContender, MAX_BENCHMARK_CONTENDERS>,
    functions: NonEmptyBoundedVec<BenchmarkFunction, MAX_BENCHMARK_FUNCTIONS>,
    review: BenchmarkReview,
}

/// A validated benchmark compiler output.
///
/// ```compile_fail
/// use loom_research_types::BenchmarkManifest;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<BenchmarkManifest>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct BenchmarkManifest(BenchmarkManifestWire);

impl BenchmarkManifest {
    pub const fn format(&self) -> ManifestFormat {
        self.0.format
    }

    pub const fn name(&self) -> &ManifestName {
        &self.0.name
    }

    pub const fn description(&self) -> &ManifestDescription {
        &self.0.description
    }

    pub const fn campaign(&self) -> &ArtifactReference {
        &self.0.campaign
    }

    pub const fn seed(&self) -> u64 {
        self.0.seed
    }

    pub const fn nested_n(&self) -> &NonEmptyBoundedSet<u32, MAX_N_CURVE_POINTS> {
        &self.0.nested_n
    }

    pub fn contenders(&self) -> &[BenchmarkContender] {
        &self.0.contenders
    }

    pub fn functions(&self) -> &[BenchmarkFunction] {
        &self.0.functions
    }

    pub const fn review(&self) -> &BenchmarkReview {
        &self.0.review
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ManifestDocument {
    CorePack(CorePackManifest),
    GenrePack(GenrePackManifest),
    ModelBindings(ModelBindingsManifest),
    Campaign(CampaignManifest),
    Benchmark(BenchmarkManifest),
}

impl ManifestDocument {
    pub const fn format(&self) -> ManifestFormat {
        match self {
            Self::CorePack(_) => ManifestFormat::CorePackV1,
            Self::GenrePack(_) => ManifestFormat::GenrePackV1,
            Self::ModelBindings(_) => ManifestFormat::ModelBindingsV1,
            Self::Campaign(_) => ManifestFormat::CampaignV1,
            Self::Benchmark(_) => ManifestFormat::BenchmarkV1,
        }
    }

    fn validate(&self) -> Result<(), ManifestCompileError> {
        match self {
            Self::CorePack(manifest) => validate_core_pack(manifest),
            Self::GenrePack(manifest) => validate_genre_pack(manifest),
            Self::ModelBindings(manifest) => validate_model_bindings(manifest),
            Self::Campaign(manifest) => validate_campaign(manifest),
            Self::Benchmark(manifest) => validate_benchmark(manifest),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ManifestSourceHash(BlobId);

impl ManifestSourceHash {
    pub const fn as_blob_id(self) -> BlobId {
        self.0
    }
}

impl fmt::Display for ManifestSourceHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ManifestArtifactHash(BlobId);

impl ManifestArtifactHash {
    pub const fn as_blob_id(self) -> BlobId {
        self.0
    }
}

impl fmt::Display for ManifestArtifactHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone)]
pub struct CompiledManifest {
    source_bytes: Vec<u8>,
    source_hash: ManifestSourceHash,
    document: ManifestDocument,
    canonical_bytes: Vec<u8>,
    artifact_hash: ManifestArtifactHash,
}

impl fmt::Debug for CompiledManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledManifest")
            .field("format", &self.format())
            .field("source_bytes", &self.source_bytes.len())
            .field("source_hash", &self.source_hash)
            .field("canonical_bytes", &self.canonical_bytes.len())
            .field("artifact_hash", &self.artifact_hash)
            .finish_non_exhaustive()
    }
}

impl CompiledManifest {
    pub fn compile(source_bytes: &[u8]) -> Result<Self, ManifestCompileError> {
        compile_manifest(source_bytes)
    }

    pub fn format(&self) -> ManifestFormat {
        self.document.format()
    }

    pub fn source_bytes(&self) -> &[u8] {
        &self.source_bytes
    }

    pub const fn source_hash(&self) -> ManifestSourceHash {
        self.source_hash
    }

    pub const fn document(&self) -> &ManifestDocument {
        &self.document
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    pub const fn artifact_hash(&self) -> ManifestArtifactHash {
        self.artifact_hash
    }

    /// Recompiles the preserved source and rejects any internal byte, document,
    /// canonical encoding, or digest mismatch.
    pub fn verify_integrity(&self) -> Result<(), ManifestIntegrityError> {
        if ManifestSourceHash(BlobId::digest(&self.source_bytes)) != self.source_hash {
            return Err(ManifestIntegrityError::SourceHash);
        }
        let rebuilt = compile_manifest(&self.source_bytes)
            .map_err(|error| ManifestIntegrityError::Recompile(Box::new(error)))?;
        if rebuilt.document != self.document {
            return Err(ManifestIntegrityError::Document);
        }
        if rebuilt.canonical_bytes != self.canonical_bytes {
            return Err(ManifestIntegrityError::CanonicalBytes);
        }
        if rebuilt.artifact_hash != self.artifact_hash {
            return Err(ManifestIntegrityError::ArtifactHash);
        }
        Ok(())
    }
}

/// One-based source coordinates retained without retaining an error's source
/// excerpt or attacker-controlled diagnostic message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestSourceLocation {
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for ManifestSourceLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}, column {}", self.line, self.column)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ManifestTomlErrorCategory {
    #[error("syntax error")]
    Syntax,
    #[error("missing required field")]
    MissingField,
    #[error("unknown field")]
    UnknownField,
    #[error("duplicate field")]
    DuplicateField,
    #[error("invalid field type")]
    InvalidType,
    #[error("invalid field value")]
    InvalidValue,
    #[error("schema constraint violation")]
    ConstraintViolation,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ManifestCanonicalErrorCategory {
    #[error("unsupported value type")]
    UnsupportedType,
    #[error("unsupported absent value")]
    UnsupportedNone,
    #[error("non-string map key")]
    NonStringKey,
    #[error("invalid date value")]
    InvalidDate,
    #[error("encoding failure")]
    Other,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ManifestFieldViolation {
    #[error("expected format {expected}, received {actual}")]
    Format {
        expected: ManifestFormat,
        actual: ManifestFormat,
    },
    #[error("expected reference to {expected}, received {actual}")]
    ReferenceFormat {
        expected: ManifestFormat,
        actual: ManifestFormat,
    },
    #[error("contains a duplicate identifier")]
    DuplicateIdentifier,
    #[error("must be between zero and one inclusive")]
    Probability,
    #[error("must be greater than zero")]
    Positive,
    #[error("must be non-negative")]
    NonNegative,
    #[error("must be non-zero")]
    NonZero,
    #[error("value {actual} exceeds maximum {maximum}")]
    ExceedsMaximum { actual: u64, maximum: u64 },
    #[error("checked campaign demand arithmetic overflowed")]
    ArithmeticOverflow,
    #[error("writer budget {available} cannot cover declared maximum demand {required}")]
    InsufficientWriterBudget { required: u64, available: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestCompileError {
    EmptySource,
    SourceTooLarge {
        actual: usize,
        maximum: usize,
    },
    InvalidUtf8 {
        valid_up_to: usize,
        error_len: Option<usize>,
    },
    Toml {
        category: ManifestTomlErrorCategory,
        location: Option<ManifestSourceLocation>,
    },
    Canonical {
        category: ManifestCanonicalErrorCategory,
    },
    NonFiniteFloat {
        location: Option<ManifestSourceLocation>,
    },
    InvalidField {
        field: &'static str,
        violation: ManifestFieldViolation,
    },
}

impl fmt::Display for ManifestCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySource => formatter.write_str("manifest source is empty"),
            Self::SourceTooLarge { actual, maximum } => write!(
                formatter,
                "manifest source has {actual} bytes; maximum is {maximum}"
            ),
            Self::InvalidUtf8 {
                valid_up_to,
                error_len,
            } => {
                write!(
                    formatter,
                    "manifest source is not UTF-8 at byte {valid_up_to}"
                )?;
                if let Some(error_len) = error_len {
                    write!(formatter, " (invalid sequence length {error_len})")?;
                }
                Ok(())
            }
            Self::Toml { category, location } => {
                write!(formatter, "invalid TOML manifest: {category}")?;
                write_location(formatter, *location)
            }
            Self::Canonical { category } => {
                write!(formatter, "cannot encode canonical manifest: {category}")
            }
            Self::NonFiniteFloat { location } => {
                formatter.write_str("manifest contains a non-finite floating-point value")?;
                write_location(formatter, *location)
            }
            Self::InvalidField { field, violation } => {
                write!(formatter, "invalid manifest field {field}: {violation}")
            }
        }
    }
}

impl std::error::Error for ManifestCompileError {}

fn write_location(
    formatter: &mut fmt::Formatter<'_>,
    location: Option<ManifestSourceLocation>,
) -> fmt::Result {
    if let Some(location) = location {
        write!(formatter, " at {location}")
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ManifestIntegrityError {
    #[error("exact source digest mismatch")]
    SourceHash,
    #[error("preserved source no longer compiles: {0}")]
    Recompile(Box<ManifestCompileError>),
    #[error("typed document differs from the preserved source")]
    Document,
    #[error("canonical semantic bytes differ from the preserved source")]
    CanonicalBytes,
    #[error("semantic artifact digest mismatch")]
    ArtifactHash,
}

#[derive(Deserialize)]
struct FormatProbe {
    format: ManifestFormat,
}

fn parse_toml<T>(source: &str) -> Result<T, ManifestCompileError>
where
    T: serde::de::DeserializeOwned,
{
    toml::from_str(source).map_err(|error| redact_toml_error(source, &error))
}

fn redact_toml_error(source: &str, error: &toml::de::Error) -> ManifestCompileError {
    let location = error
        .span()
        .map(|span| manifest_source_location(source, span.start));
    let message = error.message();
    if message.contains("non-finite floating-point value") {
        return ManifestCompileError::NonFiniteFloat { location };
    }
    let category = if message.starts_with("missing field") {
        ManifestTomlErrorCategory::MissingField
    } else if message.starts_with("unknown field") {
        ManifestTomlErrorCategory::UnknownField
    } else if message.starts_with("duplicate field")
        || message.starts_with("duplicate key")
        || message.starts_with("duplicate map key")
    {
        ManifestTomlErrorCategory::DuplicateField
    } else if message.starts_with("invalid type") {
        ManifestTomlErrorCategory::InvalidType
    } else if message.starts_with("invalid value") || message.starts_with("unknown variant") {
        ManifestTomlErrorCategory::InvalidValue
    } else if message.starts_with("invalid length")
        || message.starts_with("collection ")
        || message.starts_with("text ")
        || message.starts_with("duplicate set entry")
    {
        ManifestTomlErrorCategory::ConstraintViolation
    } else {
        ManifestTomlErrorCategory::Syntax
    };
    ManifestCompileError::Toml { category, location }
}

fn manifest_source_location(source: &str, byte_offset: usize) -> ManifestSourceLocation {
    let mut byte_offset = byte_offset.min(source.len());
    while !source.is_char_boundary(byte_offset) {
        byte_offset -= 1;
    }
    let prefix = &source[..byte_offset];
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    ManifestSourceLocation {
        line: prefix.bytes().filter(|byte| *byte == b'\n').count() + 1,
        column: source[line_start..byte_offset].chars().count() + 1,
    }
}

pub fn compile_manifest(source_bytes: &[u8]) -> Result<CompiledManifest, ManifestCompileError> {
    if source_bytes.is_empty() {
        return Err(ManifestCompileError::EmptySource);
    }
    if source_bytes.len() > MAX_MANIFEST_SOURCE_BYTES {
        return Err(ManifestCompileError::SourceTooLarge {
            actual: source_bytes.len(),
            maximum: MAX_MANIFEST_SOURCE_BYTES,
        });
    }
    let source =
        std::str::from_utf8(source_bytes).map_err(|error| ManifestCompileError::InvalidUtf8 {
            valid_up_to: error.valid_up_to(),
            error_len: error.error_len(),
        })?;
    let probe = parse_toml::<FormatProbe>(source)?;
    let document =
        match probe.format {
            ManifestFormat::CorePackV1 => ManifestDocument::CorePack(CorePackManifest(
                parse_toml::<CorePackManifestWire>(source)?,
            )),
            ManifestFormat::GenrePackV1 => ManifestDocument::GenrePack(GenrePackManifest(
                parse_toml::<GenrePackManifestWire>(source)?,
            )),
            ManifestFormat::ModelBindingsV1 => {
                ManifestDocument::ModelBindings(ModelBindingsManifest(parse_toml::<
                    ModelBindingsManifestWire,
                >(source)?))
            }
            ManifestFormat::CampaignV1 => ManifestDocument::Campaign(CampaignManifest(
                parse_toml::<CampaignManifestWire>(source)?,
            )),
            ManifestFormat::BenchmarkV1 => ManifestDocument::Benchmark(BenchmarkManifest(
                parse_toml::<BenchmarkManifestWire>(source)?,
            )),
        };
    document.validate()?;
    let canonical_bytes = canonical_manifest_bytes(&document)?;
    Ok(CompiledManifest {
        source_bytes: source_bytes.to_vec(),
        source_hash: ManifestSourceHash(BlobId::digest(source_bytes)),
        artifact_hash: ManifestArtifactHash(BlobId::digest(&canonical_bytes)),
        canonical_bytes,
        document,
    })
}

fn validate_core_pack(manifest: &CorePackManifest) -> Result<(), ManifestCompileError> {
    let manifest = &manifest.0;
    require_format(manifest.format, ManifestFormat::CorePackV1)?;
    unique_ids(
        "criteria.id",
        manifest.criteria.iter().map(|value| &value.id),
    )?;
    for criterion in manifest.criteria.iter() {
        require_positive("criteria.weight", criterion.weight)?;
    }
    for role in manifest.prompt_roles.values() {
        require_nonzero("prompt_roles.max_tokens", &role.max_tokens)?;
    }
    Ok(())
}

fn validate_genre_pack(manifest: &GenrePackManifest) -> Result<(), ManifestCompileError> {
    let manifest = &manifest.0;
    require_format(manifest.format, ManifestFormat::GenrePackV1)?;
    require_reference("core_pack", &manifest.core_pack, ManifestFormat::CorePackV1)?;
    for criterion in manifest.criteria.values() {
        require_positive("criteria.weight_multiplier", criterion.weight_multiplier)?;
    }
    for anchor in manifest.project_anchors.values() {
        require_nonzero("project_anchors.retrieval_limit", &anchor.retrieval_limit)?;
    }
    Ok(())
}

fn validate_model_bindings(manifest: &ModelBindingsManifest) -> Result<(), ManifestCompileError> {
    let manifest = &manifest.0;
    require_format(manifest.format, ManifestFormat::ModelBindingsV1)?;
    unique_ids(
        "bindings.id",
        manifest.bindings.iter().map(|value| &value.id),
    )?;
    for binding in manifest.bindings.iter() {
        require_nonzero("bindings.model_bytes", &binding.model_bytes)?;
        require_nonzero("bindings.context_tokens", &binding.context_tokens)?;
        for adapter in binding.adapters.iter() {
            require_positive("bindings.adapters.scale", adapter.scale)?;
        }
    }
    Ok(())
}

fn validate_campaign(manifest: &CampaignManifest) -> Result<(), ManifestCompileError> {
    let manifest = &manifest.0;
    require_format(manifest.format, ManifestFormat::CampaignV1)?;
    require_reference("core_pack", &manifest.core_pack, ManifestFormat::CorePackV1)?;
    require_reference(
        "genre_pack",
        &manifest.genre_pack,
        ManifestFormat::GenrePackV1,
    )?;
    require_reference(
        "model_bindings",
        &manifest.model_bindings,
        ManifestFormat::ModelBindingsV1,
    )?;
    unique_ids("cases.id", manifest.cases.iter().map(|value| &value.id))?;
    unique_ids(
        "treatments.id",
        manifest.treatments.iter().map(|value| &value.id),
    )?;
    for case in manifest.cases.iter() {
        require_nonzero("cases.max_context_tokens", &case.max_context_tokens)?;
    }
    for treatment in manifest.treatments.iter() {
        require_nonzero("treatments.samples_per_case", &treatment.samples_per_case)?;
        require_maximum(
            "treatments.samples_per_case",
            u64::from(treatment.samples_per_case),
            u64::try_from(MAX_BASE_WRITER_BATCH_CASES)
                .expect("writer batch case bound fits in u64"),
        )?;
        require_nonzero("treatments.max_output_tokens", &treatment.max_output_tokens)?;
        require_maximum(
            "treatments.max_output_tokens",
            u64::from(treatment.max_output_tokens),
            u64::from(MAX_TREATMENT_OUTPUT_TOKENS),
        )?;
        validate_sampler(&treatment.sampler)?;
    }
    require_nonzero(
        "budget.max_writer_tokens",
        &manifest.budget.max_writer_tokens,
    )?;
    require_nonzero("budget.max_evaluations", &manifest.budget.max_evaluations)?;
    require_maximum(
        "budget.max_writer_tokens",
        manifest.budget.max_writer_tokens,
        MAX_CAMPAIGN_TOKEN_BUDGET,
    )?;
    require_maximum(
        "budget.max_controller_tokens",
        manifest.budget.max_controller_tokens,
        MAX_CAMPAIGN_TOKEN_BUDGET,
    )?;
    require_maximum(
        "budget.max_evaluations",
        u64::from(manifest.budget.max_evaluations),
        u64::from(MAX_CAMPAIGN_EVALUATIONS),
    )?;
    let aggregate_token_budget = manifest
        .budget
        .max_writer_tokens
        .checked_add(manifest.budget.max_controller_tokens)
        .ok_or(ManifestCompileError::InvalidField {
            field: "budget.aggregate_token_budget",
            violation: ManifestFieldViolation::ArithmeticOverflow,
        })?;
    require_maximum(
        "budget.aggregate_token_budget",
        aggregate_token_budget,
        MAX_CAMPAIGN_TOKEN_BUDGET,
    )?;

    let maximum_writer_demand = maximum_declared_writer_tokens(manifest)?;
    require_maximum(
        "campaign.maximum_declared_writer_tokens",
        maximum_writer_demand,
        MAX_CAMPAIGN_TOKEN_BUDGET,
    )?;
    if manifest.budget.max_writer_tokens < maximum_writer_demand {
        return Err(ManifestCompileError::InvalidField {
            field: "budget.max_writer_tokens",
            violation: ManifestFieldViolation::InsufficientWriterBudget {
                required: maximum_writer_demand,
                available: manifest.budget.max_writer_tokens,
            },
        });
    }
    Ok(())
}

fn validate_sampler(sampler: &SamplerTreatment) -> Result<(), ManifestCompileError> {
    require_nonnegative("sampler.temperature", sampler.temperature)?;
    require_probability("sampler.top_p", sampler.top_p)?;
    require_probability("sampler.min_p", sampler.min_p)?;
    require_probability("sampler.typical_p", sampler.typical_p)?;
    require_positive("sampler.repetition_penalty", sampler.repetition_penalty)?;
    if let Some(scale) = sampler.cfg_scale {
        require_nonnegative("sampler.cfg_scale", scale)?;
    }
    require_maximum(
        "sampler.top_k",
        u64::from(sampler.top_k),
        u64::from(MAX_SAMPLER_TOP_K),
    )?;
    Ok(())
}

fn maximum_declared_writer_tokens(
    manifest: &CampaignManifestWire,
) -> Result<u64, ManifestCompileError> {
    let tokens_per_case = manifest
        .treatments
        .iter()
        .try_fold(0_u64, |total, treatment| {
            let treatment_tokens = u64::from(treatment.samples_per_case)
                .checked_mul(u64::from(treatment.max_output_tokens))
                .ok_or(ManifestCompileError::InvalidField {
                    field: "campaign.maximum_declared_writer_tokens",
                    violation: ManifestFieldViolation::ArithmeticOverflow,
                })?;
            total
                .checked_add(treatment_tokens)
                .ok_or(ManifestCompileError::InvalidField {
                    field: "campaign.maximum_declared_writer_tokens",
                    violation: ManifestFieldViolation::ArithmeticOverflow,
                })
        })?;
    u64::try_from(manifest.cases.len())
        .ok()
        .and_then(|cases| cases.checked_mul(tokens_per_case))
        .ok_or(ManifestCompileError::InvalidField {
            field: "campaign.maximum_declared_writer_tokens",
            violation: ManifestFieldViolation::ArithmeticOverflow,
        })
}

fn validate_benchmark(manifest: &BenchmarkManifest) -> Result<(), ManifestCompileError> {
    let manifest = &manifest.0;
    require_format(manifest.format, ManifestFormat::BenchmarkV1)?;
    require_reference("campaign", &manifest.campaign, ManifestFormat::CampaignV1)?;
    unique_ids(
        "contenders.id",
        manifest.contenders.iter().map(|value| &value.id),
    )?;
    unique_ids(
        "functions.id",
        manifest.functions.iter().map(|value| &value.id),
    )?;
    if manifest.nested_n.iter().any(|value| *value == 0) {
        return invalid("nested_n", ManifestFieldViolation::NonZero);
    }
    require_nonzero("review.fresh_runs", &manifest.review.fresh_runs)?;
    require_nonzero(
        "review.order_permutation_cells",
        &manifest.review.order_permutation_cells,
    )?;
    Ok(())
}

fn require_format(
    actual: ManifestFormat,
    expected: ManifestFormat,
) -> Result<(), ManifestCompileError> {
    if actual == expected {
        Ok(())
    } else {
        invalid(
            "format",
            ManifestFieldViolation::Format { expected, actual },
        )
    }
}

fn require_reference(
    field: &'static str,
    reference: &ArtifactReference,
    expected: ManifestFormat,
) -> Result<(), ManifestCompileError> {
    if reference.format == expected {
        Ok(())
    } else {
        invalid(
            field,
            ManifestFieldViolation::ReferenceFormat {
                expected,
                actual: reference.format,
            },
        )
    }
}

fn unique_ids<'a>(
    field: &'static str,
    values: impl IntoIterator<Item = &'a ManifestKey>,
) -> Result<(), ManifestCompileError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return invalid(field, ManifestFieldViolation::DuplicateIdentifier);
        }
    }
    Ok(())
}

fn require_probability(field: &'static str, value: FiniteF64) -> Result<(), ManifestCompileError> {
    if (0.0..=1.0).contains(&value.get()) {
        Ok(())
    } else {
        invalid(field, ManifestFieldViolation::Probability)
    }
}

fn require_positive(field: &'static str, value: FiniteF64) -> Result<(), ManifestCompileError> {
    if value.get() > 0.0 {
        Ok(())
    } else {
        invalid(field, ManifestFieldViolation::Positive)
    }
}

fn require_nonnegative(field: &'static str, value: FiniteF64) -> Result<(), ManifestCompileError> {
    if value.get() >= 0.0 {
        Ok(())
    } else {
        invalid(field, ManifestFieldViolation::NonNegative)
    }
}

fn require_nonzero<T>(field: &'static str, value: &T) -> Result<(), ManifestCompileError>
where
    T: Default + PartialEq,
{
    if value == &T::default() {
        invalid(field, ManifestFieldViolation::NonZero)
    } else {
        Ok(())
    }
}

fn require_maximum(
    field: &'static str,
    actual: u64,
    maximum: u64,
) -> Result<(), ManifestCompileError> {
    if actual <= maximum {
        Ok(())
    } else {
        invalid(
            field,
            ManifestFieldViolation::ExceedsMaximum { actual, maximum },
        )
    }
}

fn invalid<T>(
    field: &'static str,
    violation: ManifestFieldViolation,
) -> Result<T, ManifestCompileError> {
    Err(ManifestCompileError::InvalidField { field, violation })
}

fn canonical_manifest_bytes(document: &ManifestDocument) -> Result<Vec<u8>, ManifestCompileError> {
    let value = toml::Value::try_from(document).map_err(|error| redact_canonical_error(&error))?;
    let mut output = Vec::with_capacity(1_024);
    output.extend_from_slice(CANONICAL_MANIFEST_DOMAIN);
    encode_canonical_value(&mut output, &value);
    Ok(output)
}

fn redact_canonical_error(error: &toml::ser::Error) -> ManifestCompileError {
    let message = error.to_string();
    let category = if message.contains("unsupported") && message.contains("type") {
        ManifestCanonicalErrorCategory::UnsupportedType
    } else if message.contains("unsupported None") {
        ManifestCanonicalErrorCategory::UnsupportedNone
    } else if message.contains("key") && message.contains("string") {
        ManifestCanonicalErrorCategory::NonStringKey
    } else if message.contains("date") && message.contains("invalid") {
        ManifestCanonicalErrorCategory::InvalidDate
    } else {
        ManifestCanonicalErrorCategory::Other
    };
    ManifestCompileError::Canonical { category }
}

fn encode_canonical_value(output: &mut Vec<u8>, value: &toml::Value) {
    match value {
        toml::Value::String(value) => {
            output.push(0x01);
            append_bytes(output, value.as_bytes());
        }
        toml::Value::Integer(value) => {
            output.push(0x02);
            output.extend_from_slice(&value.to_be_bytes());
        }
        toml::Value::Float(value) => {
            output.push(0x03);
            output.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        toml::Value::Boolean(value) => {
            output.push(0x04);
            output.push(u8::from(*value));
        }
        toml::Value::Datetime(value) => {
            output.push(0x05);
            append_bytes(output, value.to_string().as_bytes());
        }
        toml::Value::Array(values) => {
            output.push(0x06);
            append_len(output, values.len());
            for value in values {
                encode_canonical_value(output, value);
            }
        }
        toml::Value::Table(values) => {
            output.push(0x07);
            append_len(output, values.len());
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (key, value) in entries {
                append_bytes(output, key.as_bytes());
                encode_canonical_value(output, value);
            }
        }
    }
}

fn append_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    append_len(output, bytes.len());
    output.extend_from_slice(bytes);
}

fn append_len(output: &mut Vec<u8>, len: usize) {
    let len = u64::try_from(len).expect("bounded manifest length fits in u64");
    output.extend_from_slice(&len.to_be_bytes());
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
