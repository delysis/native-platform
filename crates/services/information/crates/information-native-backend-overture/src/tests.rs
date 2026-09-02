use crate::*;
use information_native_types::{BoundingBox, EvidenceLocator};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

const RELEASE: &str = "2026-08-19.0";
const GERS_ID: &str = "0501822b-a8f0-445d-9a32-20ba346291cd";

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn exact(source_uri: String, bytes: &[u8]) -> ExactStacDocument {
    ExactStacDocument {
        source_uri,
        expected_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        expected_sha256: digest(bytes),
    }
}

fn release_fixture() -> (ExactStacDocument, Vec<u8>) {
    let source_uri = format!("https://stac.overturemaps.org/{RELEASE}/catalog.json");
    let bytes = serde_json::to_vec(&json!({
        "type": "Catalog",
        "id": RELEASE,
        "stac_version": "1.1.0",
        "description": "Exact Overture release fixture",
        "links": [
            {"rel": "root", "href": source_uri, "type": "application/json"},
            {"rel": "self", "href": source_uri, "type": "application/json"}
        ],
        "release:version": RELEASE,
        "latest": true
    }))
    .unwrap_or_else(|error| panic!("serialize release fixture: {error}"));
    (exact(source_uri, &bytes), bytes)
}

fn admitted_release() -> AdmittedOvertureRelease {
    let (expectation, bytes) = release_fixture();
    admit_overture_release(RELEASE, &expectation, &bytes)
        .unwrap_or_else(|error| panic!("release fixture must be valid: {error}"))
}

fn selection() -> OvertureSelection {
    OvertureSelection {
        bounding_box: BoundingBox {
            west: -123.0,
            south: 37.0,
            east: -121.0,
            north: 39.0,
        },
        theme: "places".to_string(),
        feature_type: "place".to_string(),
    }
}

fn item_fixture() -> (ExactStacDocument, Vec<u8>) {
    item_fixture_named("00000", "part-00000-fixture-c000.zstd.parquet")
}

fn item_fixture_named(item_id: &str, partition_file: &str) -> (ExactStacDocument, Vec<u8>) {
    let source_uri =
        format!("https://stac.overturemaps.org/{RELEASE}/places/place/{item_id}/{item_id}.json");
    let root_uri = format!("https://stac.overturemaps.org/{RELEASE}/catalog.json");
    let collection_uri =
        format!("https://stac.overturemaps.org/{RELEASE}/places/place/collection.json");
    let partition_uri = format!(
        "https://overturemaps-us-west-2.s3.us-west-2.amazonaws.com/release/{RELEASE}/theme=places/type=place/{partition_file}"
    );
    let bytes = serde_json::to_vec(&json!({
        "type": "Feature",
        "stac_version": "1.1.0",
        "id": item_id,
        "geometry": {"type": "Polygon", "coordinates": []},
        "bbox": [-130.0, 30.0, -110.0, 45.0],
        "properties": {
            "num_rows": 1000,
            "num_row_groups": 4,
            "datetime": "2026-08-19T00:00:00Z"
        },
        "links": [
            {"rel": "root", "href": root_uri, "type": "application/json"},
            {"rel": "collection", "href": collection_uri, "type": "application/json"},
            {"rel": "self", "href": source_uri, "type": "application/json"}
        ],
        "assets": {
            "aws": {
                "href": partition_uri,
                "type": "application/vnd.apache.parquet",
                "roles": ["data"]
            }
        },
        "collection": "place"
    }))
    .unwrap_or_else(|error| panic!("serialize item fixture: {error}"));
    (exact(source_uri, &bytes), bytes)
}

fn parquet_bytes() -> Vec<u8> {
    b"PAR1bounded-overture-fixturePAR1".to_vec()
}

fn admitted_partition(temp: &TempDir) -> VerifiedOverturePartition {
    let bytes = parquet_bytes();
    admitted_partition_named(
        temp,
        "00000",
        "part-00000-fixture-c000.zstd.parquet",
        "partition.parquet",
        &bytes,
    )
}

fn admitted_partition_named(
    temp: &TempDir,
    item_id: &str,
    partition_file: &str,
    local_file: &str,
    bytes: &[u8],
) -> VerifiedOverturePartition {
    let path = temp.path().join(local_file);
    fs::write(&path, bytes).unwrap_or_else(|error| panic!("write fixture: {error}"));
    let (item, item_bytes) = item_fixture_named(item_id, partition_file);
    admit_overture_partition(
        &admitted_release(),
        &selection(),
        OverturePartitionAdmission {
            item,
            item_bytes,
            asset_key: "aws".to_string(),
            local_partition_path: path,
            expected_partition_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            expected_partition_sha256: digest(bytes),
        },
    )
    .unwrap_or_else(|error| panic!("partition fixture must be valid: {error}"))
}

fn point_wkb() -> Vec<u8> {
    let mut wkb = vec![1, 1, 0, 0, 0];
    wkb.extend_from_slice(&(-122.0_f64).to_le_bytes());
    wkb.extend_from_slice(&(38.0_f64).to_le_bytes());
    wkb
}

#[derive(Debug, Clone, Copy)]
enum MockMode {
    Good,
    WrongPredicate,
    UnselectedRowGroup,
    OutsideBbox,
    OutOfRangeRowIndex,
    DuplicateId,
    DeepAttributes,
}

struct MockBackend {
    mode: MockMode,
}

impl OvertureHeavyBackend for MockBackend {
    fn query(
        &self,
        request: OvertureEngineRequest<'_>,
    ) -> Result<EngineOutput, OvertureEngineError> {
        let partition = request
            .partitions()
            .first()
            .ok_or_else(|| OvertureEngineError::new(EngineErrorClass::Internal, "no fixture"))?;
        let mut magic = [0_u8; 4];
        partition.read_exact_at(0, &mut magic).map_err(|error| {
            OvertureEngineError::new(EngineErrorClass::InvalidData, error.to_string())
        })?;
        if magic != *b"PAR1" {
            return Err(OvertureEngineError::new(
                EngineErrorClass::InvalidData,
                "bad Parquet envelope",
            ));
        }

        let identity_sha256 = partition.identity().identity_sha256();
        let predicate_sha256 = if matches!(self.mode, MockMode::WrongPredicate) {
            "0".repeat(64)
        } else {
            request.predicate().sha256.clone()
        };
        let row_group = if matches!(self.mode, MockMode::UnselectedRowGroup) {
            2
        } else {
            1
        };
        let bbox = if matches!(self.mode, MockMode::OutsideBbox) {
            BoundingBox {
                west: 10.0,
                south: 10.0,
                east: 11.0,
                north: 11.0,
            }
        } else {
            BoundingBox {
                west: -122.1,
                south: 37.9,
                east: -121.9,
                north: 38.1,
            }
        };
        let attributes = if matches!(self.mode, MockMode::DeepAttributes) {
            let mut value = Value::String("bottom".to_string());
            for _ in 0..20 {
                value = json!([value]);
            }
            BTreeMap::from([("nested".to_string(), value)])
        } else {
            BTreeMap::from([("name".to_string(), json!("Local fixture"))])
        };
        let feature = EngineFeature {
            gers_id: GERS_ID.to_string(),
            bbox,
            geometry_wkb: point_wkb(),
            version: 7,
            attributes,
            partition_identity_sha256: identity_sha256.clone(),
            row_group,
            row_index: if matches!(self.mode, MockMode::OutOfRangeRowIndex) {
                partition.identity().row_count
            } else {
                9
            },
        };
        let mut features = vec![feature.clone()];
        if matches!(self.mode, MockMode::DuplicateId) {
            features.push(feature);
        }
        Ok(EngineOutput {
            proof: PushdownReceipt {
                contract: OVERTURE_BACKEND_CONTRACT.to_string(),
                predicate_sha256,
                mechanism: PushdownMechanism::ParquetStatisticsAndRowFilter,
                partitions: vec![PartitionPushdownReceipt {
                    partition_identity_sha256: identity_sha256,
                    total_row_groups: partition.identity().row_group_count,
                    selected_row_groups: vec![1],
                    rows_decoded: u64::try_from(features.len()).unwrap_or(u64::MAX),
                }],
            },
            features,
        })
    }
}

struct MutatingBackend {
    path: PathBuf,
}

struct DecodedOnOtherPartitionBackend;

impl OvertureHeavyBackend for DecodedOnOtherPartitionBackend {
    fn query(
        &self,
        request: OvertureEngineRequest<'_>,
    ) -> Result<EngineOutput, OvertureEngineError> {
        let [feature_partition, other_partition] = request.partitions() else {
            return Err(OvertureEngineError::new(
                EngineErrorClass::Internal,
                "expected exactly two fixture partitions",
            ));
        };
        let feature_partition_sha256 = feature_partition.identity().identity_sha256();
        let other_partition_sha256 = other_partition.identity().identity_sha256();
        Ok(EngineOutput {
            proof: PushdownReceipt {
                contract: OVERTURE_BACKEND_CONTRACT.to_string(),
                predicate_sha256: request.predicate().sha256.clone(),
                mechanism: PushdownMechanism::ParquetStatisticsAndRowFilter,
                partitions: vec![
                    PartitionPushdownReceipt {
                        partition_identity_sha256: feature_partition_sha256.clone(),
                        total_row_groups: feature_partition.identity().row_group_count,
                        selected_row_groups: vec![1],
                        rows_decoded: 0,
                    },
                    PartitionPushdownReceipt {
                        partition_identity_sha256: other_partition_sha256,
                        total_row_groups: other_partition.identity().row_group_count,
                        selected_row_groups: vec![1],
                        rows_decoded: 1,
                    },
                ],
            },
            features: vec![EngineFeature {
                gers_id: GERS_ID.to_string(),
                bbox: BoundingBox {
                    west: -122.1,
                    south: 37.9,
                    east: -121.9,
                    north: 38.1,
                },
                geometry_wkb: point_wkb(),
                version: 7,
                attributes: BTreeMap::new(),
                partition_identity_sha256: feature_partition_sha256,
                row_group: 1,
                row_index: 9,
            }],
        })
    }
}

impl OvertureHeavyBackend for MutatingBackend {
    fn query(
        &self,
        request: OvertureEngineRequest<'_>,
    ) -> Result<EngineOutput, OvertureEngineError> {
        let output = MockBackend {
            mode: MockMode::Good,
        }
        .query(request)?;
        let mut changed = parquet_bytes();
        changed[6] ^= 0x10;
        fs::write(&self.path, changed).map_err(|error| {
            OvertureEngineError::new(EngineErrorClass::Internal, error.to_string())
        })?;
        Ok(output)
    }
}

#[test]
fn admitted_release_is_opaque_while_provenance_is_serializable() {
    let (expectation, bytes) = release_fixture();
    let identity = admit_overture_release(RELEASE, &expectation, &bytes)
        .unwrap_or_else(|error| panic!("release admission: {error}"));
    assert_eq!(identity.provenance().release_id, RELEASE);
    assert_eq!(identity.provenance().catalog_sha256, digest(&bytes));
    let snapshot_bytes = serde_json::to_vec(identity.provenance())
        .unwrap_or_else(|error| panic!("serialize release provenance: {error}"));
    let snapshot: OvertureReleaseProvenance = serde_json::from_slice(&snapshot_bytes)
        .unwrap_or_else(|error| panic!("deserialize release provenance: {error}"));
    assert_eq!(&snapshot, identity.provenance());

    let mut changed = bytes.clone();
    changed.push(b' ');
    assert!(matches!(
        admit_overture_release(RELEASE, &expectation, &changed),
        Err(OvertureError::ByteLengthMismatch { .. })
    ));

    let mut latest = expectation;
    latest.source_uri = "https://stac.overturemaps.org/catalog.json".to_string();
    assert!(matches!(
        admit_overture_release(RELEASE, &latest, &bytes),
        Err(OvertureError::InvalidStacIdentity(_))
    ));
}

#[test]
fn selection_requires_all_three_explicit_valid_axes() {
    let mut invalid = selection();
    invalid.theme.clear();
    assert!(matches!(
        invalid.validate(),
        Err(OvertureError::InvalidSelection("theme"))
    ));
    let mut invalid = selection();
    invalid.feature_type = "place/*".to_string();
    assert!(invalid.validate().is_err());
    let mut invalid = selection();
    invalid.bounding_box.east = invalid.bounding_box.west;
    assert!(invalid.validate().is_err());
}

#[test]
fn partition_admission_binds_stac_item_asset_and_exact_file() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let partition = admitted_partition(&temp);
    assert_eq!(partition.identity().release_id, RELEASE);
    assert_eq!(partition.identity().theme, "places");
    assert_eq!(partition.identity().feature_type, "place");
    assert_eq!(partition.identity().row_group_count, 4);
    assert_eq!(
        partition.identity().partition_sha256,
        digest(&parquet_bytes())
    );
    assert_eq!(partition.identity().identity_sha256().len(), 64);
    let mut trailing_magic = [0_u8; 4];
    partition
        .read_exact_at(
            partition.identity().partition_bytes - 4,
            &mut trailing_magic,
        )
        .unwrap_or_else(|error| panic!("bounded random read: {error}"));
    assert_eq!(trailing_magic, *b"PAR1");
    assert!(matches!(
        partition.read_exact_at(
            partition.identity().partition_bytes - 3,
            &mut trailing_magic
        ),
        Err(OvertureError::LimitExceeded("partition read range"))
    ));
}

#[test]
fn partition_admission_rejects_changed_item_and_partition_bytes() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let bytes = parquet_bytes();
    let path = temp.path().join("partition.parquet");
    fs::write(&path, &bytes).unwrap_or_else(|error| panic!("write fixture: {error}"));
    let (item, mut item_bytes) = item_fixture();
    item_bytes.push(b' ');
    let result = admit_overture_partition(
        &admitted_release(),
        &selection(),
        OverturePartitionAdmission {
            item,
            item_bytes,
            asset_key: "aws".to_string(),
            local_partition_path: path.clone(),
            expected_partition_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            expected_partition_sha256: digest(&bytes),
        },
    );
    assert!(matches!(
        result,
        Err(OvertureError::ByteLengthMismatch { .. })
    ));

    let (item, item_bytes) = item_fixture();
    let result = admit_overture_partition(
        &admitted_release(),
        &selection(),
        OverturePartitionAdmission {
            item,
            item_bytes,
            asset_key: "aws".to_string(),
            local_partition_path: path,
            expected_partition_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            expected_partition_sha256: "0".repeat(64),
        },
    );
    assert!(matches!(
        result,
        Err(OvertureError::Sha256Mismatch("GeoParquet partition"))
    ));
}

#[cfg(unix)]
#[test]
fn partition_admission_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let bytes = parquet_bytes();
    let target = temp.path().join("target.parquet");
    let alias = temp.path().join("alias.parquet");
    fs::write(&target, &bytes).unwrap_or_else(|error| panic!("write fixture: {error}"));
    symlink(&target, &alias).unwrap_or_else(|error| panic!("symlink fixture: {error}"));
    let (item, item_bytes) = item_fixture();
    let result = admit_overture_partition(
        &admitted_release(),
        &selection(),
        OverturePartitionAdmission {
            item,
            item_bytes,
            asset_key: "aws".to_string(),
            local_partition_path: alias,
            expected_partition_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            expected_partition_sha256: digest(&bytes),
        },
    );
    assert!(matches!(result, Err(OvertureError::SymlinkPartition)));
}

#[test]
fn typed_query_returns_gers_locator_and_full_provenance() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let release = admitted_release();
    let result = execute_overture_query(
        &release,
        &selection(),
        vec![admitted_partition(&temp)],
        OvertureQueryLimits::default(),
        &MockBackend {
            mode: MockMode::Good,
        },
    )
    .unwrap_or_else(|error| panic!("typed query: {error}"));
    assert_eq!(result.features.len(), 1);
    assert_eq!(
        result.features[0].locator,
        EvidenceLocator::OvertureFeature {
            gers_id: GERS_ID.to_string()
        }
    );
    assert_eq!(result.features[0].provenance.release_id, RELEASE);
    assert_eq!(
        result.features[0].provenance.predicate_sha256,
        result.predicate.sha256
    );
    assert_eq!(
        result.features[0].provenance.partition_sha256,
        digest(&parquet_bytes())
    );
    assert_eq!(result.proof.partitions[0].selected_row_groups, vec![1]);
}

#[test]
fn output_limit_counts_complete_serialized_result_and_repeated_provenance() {
    let mut limits = OvertureQueryLimits {
        max_geometry_bytes_per_feature: 64,
        max_attributes_bytes_per_feature: 64,
        max_output_bytes: 1024 * 1024,
        ..OvertureQueryLimits::default()
    };
    let first_temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let result = execute_overture_query(
        &admitted_release(),
        &selection(),
        vec![admitted_partition(&first_temp)],
        limits,
        &MockBackend {
            mode: MockMode::Good,
        },
    )
    .unwrap_or_else(|error| panic!("unbounded fixture query: {error}"));
    let serialized = serde_json::to_vec(&result)
        .unwrap_or_else(|error| panic!("serialize bounded result: {error}"));
    let serialized_text = std::str::from_utf8(&serialized)
        .unwrap_or_else(|error| panic!("result JSON is UTF-8: {error}"));
    assert!(
        serialized_text
            .matches("https://stac.overturemaps.org/")
            .count()
            >= 3
    );
    assert!(serialized.len() > limits.max_attributes_bytes_per_feature);

    limits.max_output_bytes = serialized.len();
    let exact_temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let exact = execute_overture_query(
        &admitted_release(),
        &selection(),
        vec![admitted_partition(&exact_temp)],
        limits,
        &MockBackend {
            mode: MockMode::Good,
        },
    )
    .unwrap_or_else(|error| panic!("exact output byte limit: {error}"));
    assert_eq!(
        serde_json::to_vec(&exact)
            .unwrap_or_else(|error| panic!("serialize exact bounded result: {error}"))
            .len(),
        serialized.len()
    );

    limits.max_output_bytes = serialized.len().saturating_sub(1);
    let second_temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let result = execute_overture_query(
        &admitted_release(),
        &selection(),
        vec![admitted_partition(&second_temp)],
        limits,
        &MockBackend {
            mode: MockMode::Good,
        },
    );
    assert!(matches!(
        result,
        Err(OvertureError::LimitExceeded("output bytes"))
    ));
}

#[test]
fn query_rejects_mismatched_pushdown_proof_and_unselected_rows() {
    for mode in [MockMode::WrongPredicate, MockMode::UnselectedRowGroup] {
        let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let result = execute_overture_query(
            &admitted_release(),
            &selection(),
            vec![admitted_partition(&temp)],
            OvertureQueryLimits::default(),
            &MockBackend { mode },
        );
        assert!(matches!(result, Err(OvertureError::InvalidEngineOutput(_))));
    }
}

#[test]
fn query_rejects_out_of_bbox_duplicates_and_hostile_attributes() {
    for mode in [
        MockMode::OutsideBbox,
        MockMode::DuplicateId,
        MockMode::DeepAttributes,
    ] {
        let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let result = execute_overture_query(
            &admitted_release(),
            &selection(),
            vec![admitted_partition(&temp)],
            OvertureQueryLimits::default(),
            &MockBackend { mode },
        );
        assert!(result.is_err());
    }
}

#[test]
fn query_rejects_row_index_at_partition_row_count() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let result = execute_overture_query(
        &admitted_release(),
        &selection(),
        vec![admitted_partition(&temp)],
        OvertureQueryLimits::default(),
        &MockBackend {
            mode: MockMode::OutOfRangeRowIndex,
        },
    );
    assert!(matches!(
        result,
        Err(OvertureError::InvalidEngineOutput(message))
            if message.contains("row index")
    ));
}

#[test]
fn query_enforces_decoded_rows_per_partition_not_only_in_aggregate() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let first_bytes = parquet_bytes();
    let mut second_bytes = parquet_bytes();
    second_bytes[8] ^= 0x10;
    let partitions = vec![
        admitted_partition_named(
            &temp,
            "00000",
            "part-00000-fixture-c000.zstd.parquet",
            "first.parquet",
            &first_bytes,
        ),
        admitted_partition_named(
            &temp,
            "00001",
            "part-00001-fixture-c000.zstd.parquet",
            "second.parquet",
            &second_bytes,
        ),
    ];
    let result = execute_overture_query(
        &admitted_release(),
        &selection(),
        partitions,
        OvertureQueryLimits::default(),
        &DecodedOnOtherPartitionBackend,
    );
    assert!(matches!(
        result,
        Err(OvertureError::InvalidEngineOutput(message))
            if message.contains("partition returned more features")
    ));
}

#[test]
fn query_rehashes_partition_before_engine_admission() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let partition = admitted_partition(&temp);
    let path = temp.path().join("partition.parquet");
    let mut changed = parquet_bytes();
    changed[5] ^= 0x20;
    fs::write(path, changed).unwrap_or_else(|error| panic!("mutate fixture: {error}"));
    let result = execute_overture_query(
        &admitted_release(),
        &selection(),
        vec![partition],
        OvertureQueryLimits::default(),
        &MockBackend {
            mode: MockMode::Good,
        },
    );
    assert!(matches!(result, Err(OvertureError::PartitionChanged)));
}

#[test]
fn query_rehashes_partition_after_engine_returns() {
    let temp = TempDir::new().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let partition = admitted_partition(&temp);
    let path = temp.path().join("partition.parquet");
    let result = execute_overture_query(
        &admitted_release(),
        &selection(),
        vec![partition],
        OvertureQueryLimits::default(),
        &MutatingBackend { path },
    );
    assert!(matches!(result, Err(OvertureError::PartitionChanged)));
}
