use crate::identity::{bbox_intersects, validate_simple_id};
use crate::{
    AdmittedOvertureRelease, OVERTURE_BACKEND_CONTRACT, OvertureError, OverturePartitionIdentity,
    OvertureReleaseProvenance, VerifiedOverturePartition,
};
use information_native_types::{BoundingBox, EvidenceLocator};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use thiserror::Error;
use uuid::Uuid;

const HARD_MAX_PARTITIONS: usize = 256;
const HARD_MAX_TOTAL_PARTITION_BYTES: u64 = 512 * 1024 * 1024 * 1024;
const HARD_MAX_ROW_GROUPS: u64 = 262_144;
const HARD_MAX_ROWS_DECODED: u64 = 10_000_000;
const HARD_MAX_FEATURES: usize = 10_000;
const HARD_MAX_GEOMETRY_BYTES: usize = 8 * 1024 * 1024;
const HARD_MAX_ATTRIBUTES_BYTES: usize = 2 * 1024 * 1024;
const HARD_MAX_OUTPUT_BYTES: usize = 128 * 1024 * 1024;
const MAX_ATTRIBUTE_DEPTH: usize = 16;
const MAX_ATTRIBUTE_NODES: usize = 8_192;
const MAX_ROOT_ATTRIBUTES: usize = 256;
const MAX_ENGINE_MESSAGE_BYTES: usize = 8 * 1024;

/// One mandatory and explicit query axis. There is no "all themes" or
/// backend-native escape hatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OvertureSelection {
    pub bounding_box: BoundingBox,
    pub theme: String,
    pub feature_type: String,
}

impl OvertureSelection {
    pub fn validate(&self) -> Result<(), OvertureError> {
        self.bounding_box
            .validate()
            .map_err(|_| OvertureError::InvalidSelection("bounding box"))?;
        validate_simple_id("theme", &self.theme, 64)?;
        validate_simple_id("feature type", &self.feature_type, 128)
    }
}

/// Checked ceilings passed through to the heavy engine.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OvertureQueryLimits {
    pub max_partitions: usize,
    pub max_total_partition_bytes: u64,
    pub max_row_groups: u64,
    pub max_rows_decoded: u64,
    pub max_features: usize,
    pub max_geometry_bytes_per_feature: usize,
    pub max_attributes_bytes_per_feature: usize,
    /// Maximum compact JSON byte length of the complete returned object,
    /// including its proof and all repeated feature provenance.
    pub max_output_bytes: usize,
}

impl Default for OvertureQueryLimits {
    fn default() -> Self {
        Self {
            max_partitions: 64,
            max_total_partition_bytes: 64 * 1024 * 1024 * 1024,
            max_row_groups: 65_536,
            max_rows_decoded: 1_000_000,
            max_features: 1_000,
            max_geometry_bytes_per_feature: 2 * 1024 * 1024,
            max_attributes_bytes_per_feature: 256 * 1024,
            max_output_bytes: 32 * 1024 * 1024,
        }
    }
}

impl OvertureQueryLimits {
    pub fn validate(self) -> Result<(), OvertureError> {
        if self.max_partitions == 0
            || self.max_partitions > HARD_MAX_PARTITIONS
            || self.max_total_partition_bytes == 0
            || self.max_total_partition_bytes > HARD_MAX_TOTAL_PARTITION_BYTES
            || self.max_row_groups == 0
            || self.max_row_groups > HARD_MAX_ROW_GROUPS
            || self.max_rows_decoded == 0
            || self.max_rows_decoded > HARD_MAX_ROWS_DECODED
            || self.max_features == 0
            || self.max_features > HARD_MAX_FEATURES
            || self.max_geometry_bytes_per_feature < 5
            || self.max_geometry_bytes_per_feature > HARD_MAX_GEOMETRY_BYTES
            || self.max_attributes_bytes_per_feature == 0
            || self.max_attributes_bytes_per_feature > HARD_MAX_ATTRIBUTES_BYTES
            || self.max_output_bytes == 0
            || self.max_output_bytes > HARD_MAX_OUTPUT_BYTES
            || self.max_output_bytes < self.max_geometry_bytes_per_feature
            || self.max_output_bytes < self.max_attributes_bytes_per_feature
        {
            return Err(OvertureError::InvalidLimits);
        }
        Ok(())
    }
}

/// The exact intersection predicate an engine must push into Parquet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PushdownPredicate {
    pub theme_equals: String,
    pub feature_type_equals: String,
    pub bbox_xmin_lte: f64,
    pub bbox_xmax_gte: f64,
    pub bbox_ymin_lte: f64,
    pub bbox_ymax_gte: f64,
    pub sha256: String,
}

impl PushdownPredicate {
    fn from_selection(selection: &OvertureSelection) -> Result<Self, OvertureError> {
        let mut predicate = Self {
            theme_equals: selection.theme.clone(),
            feature_type_equals: selection.feature_type.clone(),
            bbox_xmin_lte: selection.bounding_box.east,
            bbox_xmax_gte: selection.bounding_box.west,
            bbox_ymin_lte: selection.bounding_box.north,
            bbox_ymax_gte: selection.bounding_box.south,
            sha256: String::new(),
        };
        predicate.sha256 = predicate_fingerprint(&predicate)?;
        Ok(predicate)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PushdownMechanism {
    /// Bbox statistics prune row groups, then the identical predicate filters
    /// rows before feature materialization.
    ParquetStatisticsAndRowFilter,
}

/// One engine-attested physical partition scan. Row-group indexes must be
/// unique, sorted, in range, and exact for the corresponding partition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PartitionPushdownReceipt {
    pub partition_identity_sha256: String,
    pub total_row_groups: u32,
    pub selected_row_groups: Vec<u32>,
    pub rows_decoded: u64,
}

/// Machine-checkable execution proof returned by the injected engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PushdownReceipt {
    pub contract: String,
    pub predicate_sha256: String,
    pub mechanism: PushdownMechanism,
    pub partitions: Vec<PartitionPushdownReceipt>,
}

/// A single row returned by the heavy engine before trust-boundary checks.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineFeature {
    pub gers_id: String,
    pub bbox: BoundingBox,
    pub geometry_wkb: Vec<u8>,
    pub version: u64,
    pub attributes: BTreeMap<String, Value>,
    pub partition_identity_sha256: String,
    pub row_group: u32,
    pub row_index: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineOutput {
    pub proof: PushdownReceipt,
    pub features: Vec<EngineFeature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineErrorClass {
    Unsupported,
    InvalidData,
    ResourceLimit,
    Cancelled,
    Internal,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("Overture engine {class:?}: {message}")]
pub struct OvertureEngineError {
    class: EngineErrorClass,
    message: String,
}

impl OvertureEngineError {
    #[must_use]
    pub fn new(class: EngineErrorClass, message: impl AsRef<str>) -> Self {
        let message = message.as_ref();
        let bounded = if message.len() <= MAX_ENGINE_MESSAGE_BYTES {
            message.to_string()
        } else {
            let mut end = MAX_ENGINE_MESSAGE_BYTES;
            while !message.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            message[..end].to_string()
        };
        Self {
            class,
            message: bounded,
        }
    }

    #[must_use]
    pub fn class(&self) -> EngineErrorClass {
        self.class
    }
}

/// The only public engine request. It exposes exact file handles and a typed
/// predicate, never a path, mutable seek cursor, URL fetch capability, or raw
/// SQL string.
pub struct OvertureEngineRequest<'a> {
    release: &'a OvertureReleaseProvenance,
    selection: &'a OvertureSelection,
    predicate: &'a PushdownPredicate,
    partitions: &'a [VerifiedOverturePartition],
    limits: OvertureQueryLimits,
}

impl<'a> OvertureEngineRequest<'a> {
    #[must_use]
    pub fn release(&self) -> &'a OvertureReleaseProvenance {
        self.release
    }

    #[must_use]
    pub fn selection(&self) -> &'a OvertureSelection {
        self.selection
    }

    #[must_use]
    pub fn predicate(&self) -> &'a PushdownPredicate {
        self.predicate
    }

    #[must_use]
    pub fn partitions(&self) -> &'a [VerifiedOverturePartition] {
        self.partitions
    }

    #[must_use]
    pub fn limits(&self) -> OvertureQueryLimits {
        self.limits
    }
}

/// Heavy GeoParquet execution stays behind this narrow, synchronous boundary.
/// Implementations must join their own worker activity before returning.
pub trait OvertureHeavyBackend: Send + Sync {
    fn query(
        &self,
        request: OvertureEngineRequest<'_>,
    ) -> Result<EngineOutput, OvertureEngineError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OvertureFeatureProvenance {
    pub release_id: String,
    pub release_catalog_source_uri: String,
    pub release_catalog_sha256: String,
    pub item_id: String,
    pub item_source_uri: String,
    pub item_sha256: String,
    pub partition_source_uri: String,
    pub partition_bytes: u64,
    pub partition_sha256: String,
    pub partition_identity_sha256: String,
    pub row_group: u32,
    pub row_index: u64,
    pub predicate_sha256: String,
}

/// Bounded, typed feature envelope. WKB and attributes remain inert data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OvertureFeature {
    pub locator: EvidenceLocator,
    pub theme: String,
    pub feature_type: String,
    pub bbox: BoundingBox,
    pub geometry_wkb: Vec<u8>,
    pub version: u64,
    pub attributes: BTreeMap<String, Value>,
    pub provenance: OvertureFeatureProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OvertureQueryResult {
    pub release: OvertureReleaseProvenance,
    pub selection: OvertureSelection,
    pub predicate: PushdownPredicate,
    pub proof: PushdownReceipt,
    pub features: Vec<OvertureFeature>,
}

pub fn execute_overture_query<B: OvertureHeavyBackend + ?Sized>(
    release: &AdmittedOvertureRelease,
    selection: &OvertureSelection,
    mut partitions: Vec<VerifiedOverturePartition>,
    limits: OvertureQueryLimits,
    backend: &B,
) -> Result<OvertureQueryResult, OvertureError> {
    release.validate_for_query()?;
    let release_provenance = release.provenance();
    selection.validate()?;
    limits.validate()?;
    if partitions.is_empty() {
        return Err(OvertureError::NoPartitions);
    }
    if partitions.len() > limits.max_partitions {
        return Err(OvertureError::LimitExceeded("partition count"));
    }

    partitions.sort_by_cached_key(|partition| partition.identity().identity_sha256());
    let mut seen = BTreeSet::new();
    let mut partition_bytes = 0_u64;
    let mut row_groups = 0_u64;
    for partition in &mut partitions {
        validate_partition_binding(release_provenance, selection, partition.identity())?;
        let identity_sha256 = partition.identity().identity_sha256();
        if !seen.insert(identity_sha256) {
            return Err(OvertureError::InvalidPartition(
                "duplicate partition identity".to_string(),
            ));
        }
        partition_bytes = partition_bytes
            .checked_add(partition.identity().partition_bytes)
            .ok_or(OvertureError::LimitExceeded("partition bytes"))?;
        row_groups = row_groups
            .checked_add(u64::from(partition.identity().row_group_count))
            .ok_or(OvertureError::LimitExceeded("row group count"))?;
        partition.verify_unchanged()?;
    }
    if partition_bytes > limits.max_total_partition_bytes {
        return Err(OvertureError::LimitExceeded("partition bytes"));
    }
    if row_groups > limits.max_row_groups {
        return Err(OvertureError::LimitExceeded("row group count"));
    }

    let predicate = PushdownPredicate::from_selection(selection)?;
    let engine_result = backend.query(OvertureEngineRequest {
        release: release_provenance,
        selection,
        predicate: &predicate,
        partitions: &partitions,
        limits,
    });
    for partition in &mut partitions {
        partition.verify_unchanged()?;
    }
    let output = engine_result?;
    let returned_features = u64::try_from(output.features.len())
        .map_err(|_| OvertureError::LimitExceeded("feature count"))?;
    let decoded_rows = output
        .proof
        .partitions
        .iter()
        .try_fold(0_u64, |total, receipt| {
            total.checked_add(receipt.rows_decoded)
        });
    if decoded_rows.is_none_or(|rows| rows < returned_features) {
        return Err(OvertureError::InvalidEngineOutput(
            "engine returned more features than its decoded-row receipt".to_string(),
        ));
    }
    let partition_proofs = validate_proof(&output.proof, &predicate, &partitions, limits)?;
    let mut features = validate_features(
        output.features,
        release_provenance,
        selection,
        &predicate,
        &partitions,
        &partition_proofs,
        limits,
    )?;
    features.sort_by(|left, right| {
        locator_gers_id(&left.locator)
            .cmp(locator_gers_id(&right.locator))
            .then_with(|| {
                left.provenance
                    .partition_sha256
                    .cmp(&right.provenance.partition_sha256)
            })
            .then_with(|| left.provenance.row_group.cmp(&right.provenance.row_group))
            .then_with(|| left.provenance.row_index.cmp(&right.provenance.row_index))
    });

    let result = OvertureQueryResult {
        release: release_provenance.clone(),
        selection: selection.clone(),
        predicate,
        proof: output.proof,
        features,
    };
    bounded_serialized_len(&result, limits.max_output_bytes)?;
    Ok(result)
}

fn validate_partition_binding(
    release: &OvertureReleaseProvenance,
    selection: &OvertureSelection,
    partition: &OverturePartitionIdentity,
) -> Result<(), OvertureError> {
    if partition.release_id != release.release_id
        || partition.release_catalog_sha256 != release.catalog_sha256
        || partition.theme != selection.theme
        || partition.feature_type != selection.feature_type
        || !bbox_intersects(partition.advertised_bbox, selection.bounding_box)
        || partition.partition_bytes == 0
        || partition.row_count == 0
        || partition.row_group_count == 0
    {
        return Err(OvertureError::InvalidPartition(
            "partition does not match the exact release and selection".to_string(),
        ));
    }
    Ok(())
}

fn validate_proof(
    proof: &PushdownReceipt,
    predicate: &PushdownPredicate,
    partitions: &[VerifiedOverturePartition],
    limits: OvertureQueryLimits,
) -> Result<BTreeMap<String, ValidatedPartitionPushdown>, OvertureError> {
    if proof.contract != OVERTURE_BACKEND_CONTRACT
        || proof.predicate_sha256 != predicate.sha256
        || proof.mechanism != PushdownMechanism::ParquetStatisticsAndRowFilter
        || proof.partitions.len() != partitions.len()
    {
        return Err(OvertureError::InvalidEngineOutput(
            "pushdown receipt does not bind the exact predicate and partitions".to_string(),
        ));
    }
    let expected = partitions
        .iter()
        .map(|partition| (partition.identity().identity_sha256(), partition.identity()))
        .collect::<BTreeMap<_, _>>();
    let mut selected = BTreeMap::new();
    let mut rows_decoded = 0_u64;
    for receipt in &proof.partitions {
        let identity = expected
            .get(&receipt.partition_identity_sha256)
            .ok_or_else(|| {
                OvertureError::InvalidEngineOutput(
                    "pushdown receipt names an unknown partition".to_string(),
                )
            })?;
        if receipt.total_row_groups != identity.row_group_count
            || receipt.rows_decoded > identity.row_count
        {
            return Err(OvertureError::InvalidEngineOutput(
                "pushdown receipt row accounting conflicts with STAC identity".to_string(),
            ));
        }
        let mut groups = BTreeSet::new();
        let mut previous = None;
        for group in &receipt.selected_row_groups {
            if *group >= identity.row_group_count
                || previous.is_some_and(|value| value >= *group)
                || !groups.insert(*group)
            {
                return Err(OvertureError::InvalidEngineOutput(
                    "selected row groups are not unique, sorted, and in range".to_string(),
                ));
            }
            previous = Some(*group);
        }
        if groups.is_empty() && receipt.rows_decoded != 0 {
            return Err(OvertureError::InvalidEngineOutput(
                "engine decoded rows without selecting a row group".to_string(),
            ));
        }
        rows_decoded = rows_decoded
            .checked_add(receipt.rows_decoded)
            .ok_or(OvertureError::LimitExceeded("decoded row count"))?;
        if selected
            .insert(
                receipt.partition_identity_sha256.clone(),
                ValidatedPartitionPushdown {
                    selected_row_groups: groups,
                    rows_decoded: receipt.rows_decoded,
                },
            )
            .is_some()
        {
            return Err(OvertureError::InvalidEngineOutput(
                "pushdown receipt repeats a partition".to_string(),
            ));
        }
    }
    if selected.len() != expected.len() {
        return Err(OvertureError::InvalidEngineOutput(
            "pushdown receipt omits an admitted partition".to_string(),
        ));
    }
    if rows_decoded > limits.max_rows_decoded {
        return Err(OvertureError::LimitExceeded("decoded row count"));
    }
    Ok(selected)
}

struct ValidatedPartitionPushdown {
    selected_row_groups: BTreeSet<u32>,
    rows_decoded: u64,
}

#[allow(clippy::too_many_arguments)]
fn validate_features(
    engine_features: Vec<EngineFeature>,
    release: &OvertureReleaseProvenance,
    selection: &OvertureSelection,
    predicate: &PushdownPredicate,
    partitions: &[VerifiedOverturePartition],
    partition_proofs: &BTreeMap<String, ValidatedPartitionPushdown>,
    limits: OvertureQueryLimits,
) -> Result<Vec<OvertureFeature>, OvertureError> {
    if engine_features.len() > limits.max_features {
        return Err(OvertureError::LimitExceeded("feature count"));
    }
    let partition_map = partitions
        .iter()
        .map(|partition| (partition.identity().identity_sha256(), partition.identity()))
        .collect::<BTreeMap<_, _>>();
    let mut seen_ids = BTreeSet::new();
    let mut returned_by_partition = BTreeMap::<String, u64>::new();
    let mut output_bytes = 0_usize;
    let mut features = Vec::with_capacity(engine_features.len());
    for feature in engine_features {
        validate_gers_id(&feature.gers_id)?;
        if !seen_ids.insert(feature.gers_id.clone()) {
            return Err(OvertureError::InvalidEngineOutput(
                "engine returned a duplicate GERS id".to_string(),
            ));
        }
        let partition = partition_map
            .get(&feature.partition_identity_sha256)
            .ok_or_else(|| {
                OvertureError::InvalidEngineOutput("feature names an unknown partition".to_string())
            })?;
        let partition_proof = partition_proofs
            .get(&feature.partition_identity_sha256)
            .ok_or_else(|| {
                OvertureError::InvalidEngineOutput(
                    "feature partition has no pushdown receipt".to_string(),
                )
            })?;
        if !partition_proof
            .selected_row_groups
            .contains(&feature.row_group)
        {
            return Err(OvertureError::InvalidEngineOutput(
                "feature came from a row group not selected by pushdown".to_string(),
            ));
        }
        if feature.row_index >= partition.row_count {
            return Err(OvertureError::InvalidEngineOutput(
                "feature row index is outside the exact partition row count".to_string(),
            ));
        }
        let returned = returned_by_partition
            .entry(feature.partition_identity_sha256.clone())
            .or_default();
        *returned = returned
            .checked_add(1)
            .ok_or(OvertureError::LimitExceeded("returned row count"))?;
        if *returned > partition_proof.rows_decoded {
            return Err(OvertureError::InvalidEngineOutput(
                "partition returned more features than its decoded-row receipt".to_string(),
            ));
        }
        feature.bbox.validate().map_err(|_| {
            OvertureError::InvalidEngineOutput("feature bbox is invalid".to_string())
        })?;
        if !bbox_intersects(feature.bbox, selection.bounding_box)
            || !bbox_intersects(feature.bbox, partition.advertised_bbox)
        {
            return Err(OvertureError::InvalidEngineOutput(
                "feature falls outside the pushed bbox or advertised partition".to_string(),
            ));
        }
        if feature.geometry_wkb.len() < 5
            || feature.geometry_wkb.len() > limits.max_geometry_bytes_per_feature
            || !feature
                .geometry_wkb
                .first()
                .is_some_and(|byte| matches!(*byte, 0 | 1))
        {
            return Err(OvertureError::InvalidEngineOutput(
                "feature geometry is not bounded inert WKB".to_string(),
            ));
        }
        let attributes_bytes = validate_attributes(&feature.attributes)?;
        if attributes_bytes > limits.max_attributes_bytes_per_feature {
            return Err(OvertureError::LimitExceeded("feature attribute bytes"));
        }
        let feature = OvertureFeature {
            locator: EvidenceLocator::OvertureFeature {
                gers_id: feature.gers_id,
            },
            theme: selection.theme.clone(),
            feature_type: selection.feature_type.clone(),
            bbox: feature.bbox,
            geometry_wkb: feature.geometry_wkb,
            version: feature.version,
            attributes: feature.attributes,
            provenance: OvertureFeatureProvenance {
                release_id: release.release_id.clone(),
                release_catalog_source_uri: release.catalog_source_uri.clone(),
                release_catalog_sha256: release.catalog_sha256.clone(),
                item_id: partition.item_id.clone(),
                item_source_uri: partition.item_source_uri.clone(),
                item_sha256: partition.item_sha256.clone(),
                partition_source_uri: partition.partition_source_uri.clone(),
                partition_bytes: partition.partition_bytes,
                partition_sha256: partition.partition_sha256.clone(),
                partition_identity_sha256: feature.partition_identity_sha256,
                row_group: feature.row_group,
                row_index: feature.row_index,
                predicate_sha256: predicate.sha256.clone(),
            },
        };
        let remaining = limits
            .max_output_bytes
            .checked_sub(output_bytes)
            .ok_or(OvertureError::LimitExceeded("output bytes"))?;
        let feature_bytes = bounded_serialized_len(&feature, remaining)?;
        output_bytes = output_bytes
            .checked_add(feature_bytes)
            .ok_or(OvertureError::LimitExceeded("output bytes"))?;
        features.push(feature);
    }
    Ok(features)
}

fn validate_gers_id(value: &str) -> Result<(), OvertureError> {
    let parsed = Uuid::parse_str(value)
        .map_err(|_| OvertureError::InvalidEngineOutput("GERS id is not a UUID".to_string()))?;
    if parsed.to_string() != value {
        return Err(OvertureError::InvalidEngineOutput(
            "GERS id is not canonical lowercase UUID text".to_string(),
        ));
    }
    Ok(())
}

fn validate_attributes(attributes: &BTreeMap<String, Value>) -> Result<usize, OvertureError> {
    if attributes.len() > MAX_ROOT_ATTRIBUTES {
        return Err(OvertureError::LimitExceeded("root attribute count"));
    }
    let reserved = ["id", "bbox", "geometry", "theme", "type"];
    let mut bytes = 0_usize;
    let mut nodes = 0_usize;
    for (key, value) in attributes {
        if key.is_empty() || key.len() > 256 || reserved.contains(&key.as_str()) {
            return Err(OvertureError::InvalidEngineOutput(
                "feature attributes contain an invalid or reserved key".to_string(),
            ));
        }
        bytes = bytes
            .checked_add(key.len())
            .ok_or(OvertureError::LimitExceeded("feature attribute bytes"))?;
        account_json(value, 0, &mut nodes, &mut bytes)?;
    }
    Ok(bytes)
}

fn account_json(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
    bytes: &mut usize,
) -> Result<(), OvertureError> {
    if depth > MAX_ATTRIBUTE_DEPTH {
        return Err(OvertureError::LimitExceeded("feature attribute depth"));
    }
    *nodes = nodes
        .checked_add(1)
        .ok_or(OvertureError::LimitExceeded("feature attribute nodes"))?;
    if *nodes > MAX_ATTRIBUTE_NODES {
        return Err(OvertureError::LimitExceeded("feature attribute nodes"));
    }
    match value {
        Value::Null => add_bytes(bytes, 4),
        Value::Bool(_) => add_bytes(bytes, 5),
        Value::Number(number) => add_bytes(bytes, number.to_string().len()),
        Value::String(text) => add_bytes(bytes, text.len()),
        Value::Array(values) => {
            if values.len() > MAX_ATTRIBUTE_NODES {
                return Err(OvertureError::LimitExceeded("feature attribute array"));
            }
            for value in values {
                account_json(value, depth.saturating_add(1), nodes, bytes)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            if values.len() > MAX_ATTRIBUTE_NODES {
                return Err(OvertureError::LimitExceeded("feature attribute object"));
            }
            for (key, value) in values {
                add_bytes(bytes, key.len())?;
                account_json(value, depth.saturating_add(1), nodes, bytes)?;
            }
            Ok(())
        }
    }
}

fn add_bytes(total: &mut usize, additional: usize) -> Result<(), OvertureError> {
    *total = total
        .checked_add(additional)
        .ok_or(OvertureError::LimitExceeded("feature attribute bytes"))?;
    Ok(())
}

struct BoundedByteCounter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for BoundedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("serialized output byte count overflow"));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("serialized output exceeds byte limit"));
        }
        self.bytes = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn bounded_serialized_len<T: Serialize>(value: &T, limit: usize) -> Result<usize, OvertureError> {
    let mut counter = BoundedByteCounter {
        bytes: 0,
        limit,
        exceeded: false,
    };
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(counter.bytes),
        Err(_) if counter.exceeded => Err(OvertureError::LimitExceeded("output bytes")),
        Err(error) => Err(OvertureError::Json(error)),
    }
}

fn predicate_fingerprint(predicate: &PushdownPredicate) -> Result<String, OvertureError> {
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        theme_equals: &'a str,
        feature_type_equals: &'a str,
        bbox_xmin_lte: f64,
        bbox_xmax_gte: f64,
        bbox_ymin_lte: f64,
        bbox_ymax_gte: f64,
    }
    let bytes = serde_json::to_vec(&Fingerprint {
        theme_equals: &predicate.theme_equals,
        feature_type_equals: &predicate.feature_type_equals,
        bbox_xmin_lte: predicate.bbox_xmin_lte,
        bbox_xmax_gte: predicate.bbox_xmax_gte,
        bbox_ymin_lte: predicate.bbox_ymin_lte,
        bbox_ymax_gte: predicate.bbox_ymax_gte,
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn locator_gers_id(locator: &EvidenceLocator) -> &str {
    match locator {
        EvidenceLocator::OvertureFeature { gers_id } => gers_id,
        _ => "",
    }
}
