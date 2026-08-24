use crate::query::OvertureSelection;
use crate::{MAX_STAC_DOCUMENT_BYTES, OVERTURE_STAC_HOST, OVERTURE_STAC_VERSION, OvertureError};
use information_native_catalog::{StacDocument, StacObjectType, parse_stac_document_with_limit};
use information_native_types::BoundingBox;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
#[cfg(not(any(unix, windows)))]
use std::sync::Mutex;
use url::Url;

const PARQUET_MAGIC: &[u8; 4] = b"PAR1";
const MIN_PARQUET_BYTES: u64 = 12;
const MAX_ITEM_ROWS: u64 = 100_000_000_000;
const MAX_ITEM_ROW_GROUPS: u64 = 1_000_000;

/// Exact content identity supplied by a trusted acquisition boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactStacDocument {
    pub source_uri: String,
    pub expected_bytes: u64,
    pub expected_sha256: String,
}

/// Immutable identity of one release-specific Overture STAC catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OvertureReleaseIdentity {
    pub release_id: String,
    pub catalog_source_uri: String,
    pub catalog_bytes: u64,
    pub catalog_sha256: String,
    pub stac_version: String,
}

impl OvertureReleaseIdentity {
    pub(crate) fn validate_for_query(&self) -> Result<(), OvertureError> {
        validate_release_id(&self.release_id)?;
        let source = validate_stac_uri(&self.catalog_source_uri)?;
        if source.path() != format!("/{}/catalog.json", self.release_id)
            || self.catalog_bytes == 0
            || self.catalog_bytes > MAX_STAC_DOCUMENT_BYTES as u64
            || normalize_sha256(&self.catalog_sha256)? != self.catalog_sha256
            || self.stac_version != OVERTURE_STAC_VERSION
        {
            return Err(OvertureError::InvalidStacIdentity(
                "release identity is not exact and canonical".to_string(),
            ));
        }
        Ok(())
    }
}

/// Exact identity of one STAC-advertised GeoParquet partition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OverturePartitionIdentity {
    pub release_id: String,
    pub release_catalog_sha256: String,
    pub theme: String,
    pub feature_type: String,
    pub item_id: String,
    pub item_source_uri: String,
    pub item_bytes: u64,
    pub item_sha256: String,
    pub asset_key: String,
    pub partition_source_uri: String,
    pub partition_bytes: u64,
    pub partition_sha256: String,
    pub advertised_bbox: BoundingBox,
    pub row_count: u64,
    pub row_group_count: u32,
}

impl OverturePartitionIdentity {
    #[must_use]
    pub fn identity_sha256(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"information.overture.partition.identity.v1\0");
        for value in [
            &self.release_id,
            &self.release_catalog_sha256,
            &self.theme,
            &self.feature_type,
            &self.item_id,
            &self.item_source_uri,
            &self.item_sha256,
            &self.asset_key,
            &self.partition_source_uri,
            &self.partition_sha256,
        ] {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        hasher.update(self.item_bytes.to_le_bytes());
        hasher.update(self.partition_bytes.to_le_bytes());
        hasher.update(self.advertised_bbox.west.to_bits().to_le_bytes());
        hasher.update(self.advertised_bbox.south.to_bits().to_le_bytes());
        hasher.update(self.advertised_bbox.east.to_bits().to_le_bytes());
        hasher.update(self.advertised_bbox.north.to_bits().to_le_bytes());
        hasher.update(self.row_count.to_le_bytes());
        hasher.update(self.row_group_count.to_le_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// Native-only local admission request. The path is never serialized and is
/// not retained after its read-only file handle has been opened.
#[derive(Debug)]
pub struct OverturePartitionAdmission {
    pub item: ExactStacDocument,
    pub item_bytes: Vec<u8>,
    pub asset_key: String,
    pub local_partition_path: PathBuf,
    pub expected_partition_bytes: u64,
    pub expected_partition_sha256: String,
}

/// An exact, open partition lease. Engines receive bounded random-access reads,
/// not paths or mutable seek cursors.
#[derive(Debug)]
pub struct VerifiedOverturePartition {
    identity: OverturePartitionIdentity,
    file: File,
    #[cfg(not(any(unix, windows)))]
    fallback_read_lock: Mutex<()>,
}

impl VerifiedOverturePartition {
    #[must_use]
    pub fn identity(&self) -> &OverturePartitionIdentity {
        &self.identity
    }

    pub fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<(), OvertureError> {
        let buffer_len = u64::try_from(buffer.len())
            .map_err(|_| OvertureError::LimitExceeded("partition read range"))?;
        let end = offset
            .checked_add(buffer_len)
            .ok_or(OvertureError::LimitExceeded("partition read range"))?;
        if end > self.identity.partition_bytes {
            return Err(OvertureError::LimitExceeded("partition read range"));
        }
        #[cfg(not(any(unix, windows)))]
        let _guard = self
            .fallback_read_lock
            .lock()
            .map_err(|_| OvertureError::PartitionChanged)?;
        read_exact_at(&self.file, offset, buffer)
    }

    pub(crate) fn verify_unchanged(&mut self) -> Result<(), OvertureError> {
        let metadata = self
            .file
            .metadata()
            .map_err(|source| OvertureError::io("rechecking partition metadata", source))?;
        if !metadata.is_file() || metadata.len() != self.identity.partition_bytes {
            return Err(OvertureError::PartitionChanged);
        }
        let observed = hash_file(&mut self.file)?;
        if observed != self.identity.partition_sha256 {
            return Err(OvertureError::PartitionChanged);
        }
        validate_parquet_magic(&mut self.file, metadata.len())
            .map_err(|_| OvertureError::PartitionChanged)
    }
}

/// Admit only an explicitly named, release-specific catalog with exact bytes.
pub fn admit_overture_release(
    release_id: &str,
    expectation: &ExactStacDocument,
    bytes: &[u8],
) -> Result<OvertureReleaseIdentity, OvertureError> {
    validate_release_id(release_id)?;
    let source = validate_stac_uri(&expectation.source_uri)?;
    let expected_path = format!("/{release_id}/catalog.json");
    if source.path() != expected_path {
        return Err(OvertureError::InvalidStacIdentity(
            "release catalog URI is not the exact release path".to_string(),
        ));
    }
    let digest = verify_exact_bytes("release STAC catalog", expectation, bytes)?;
    let document = parse_stac_document_with_limit(bytes, &source, MAX_STAC_DOCUMENT_BYTES)?;
    if document.object_type != StacObjectType::Catalog
        || document.id.as_deref() != Some(release_id)
        || document.stac_version.as_deref() != Some(OVERTURE_STAC_VERSION)
    {
        return Err(OvertureError::InvalidStacIdentity(
            "release catalog type, id, or STAC version does not match".to_string(),
        ));
    }
    let declared_release = document
        .extensions
        .get("release:version")
        .and_then(serde_json::Value::as_str);
    if declared_release != Some(release_id) {
        return Err(OvertureError::InvalidStacIdentity(
            "release:version is absent or does not match the explicit release".to_string(),
        ));
    }
    require_exact_link(&document, "self", &expectation.source_uri)?;
    require_exact_link(&document, "root", &expectation.source_uri)?;

    Ok(OvertureReleaseIdentity {
        release_id: release_id.to_string(),
        catalog_source_uri: expectation.source_uri.clone(),
        catalog_bytes: expectation.expected_bytes,
        catalog_sha256: digest,
        stac_version: OVERTURE_STAC_VERSION.to_string(),
    })
}

/// Bind an exact STAC item to one explicit selection and one exact local file.
pub fn admit_overture_partition(
    release: &OvertureReleaseIdentity,
    selection: &OvertureSelection,
    admission: OverturePartitionAdmission,
) -> Result<VerifiedOverturePartition, OvertureError> {
    release.validate_for_query()?;
    selection.validate()?;
    let item_source = validate_stac_uri(&admission.item.source_uri)?;
    let item_sha256 = verify_exact_bytes(
        "partition STAC item",
        &admission.item,
        &admission.item_bytes,
    )?;
    let document = parse_stac_document_with_limit(
        &admission.item_bytes,
        &item_source,
        MAX_STAC_DOCUMENT_BYTES,
    )?;
    let item_id = validate_partition_item(release, selection, &document, &item_source)?;
    let advertised_bbox = overture_bbox(&document)?;
    if !bbox_intersects(advertised_bbox, selection.bounding_box) {
        return Err(OvertureError::InvalidPartition(
            "STAC item does not intersect the explicit query bbox".to_string(),
        ));
    }
    if !matches!(admission.asset_key.as_str(), "aws" | "azure") {
        return Err(OvertureError::InvalidPartition(
            "selected STAC asset key is not an admitted Overture data mirror".to_string(),
        ));
    }
    let asset = document.assets.get(&admission.asset_key).ok_or_else(|| {
        OvertureError::InvalidPartition("selected STAC asset does not exist".to_string())
    })?;
    if asset.media_type.as_deref() != Some("application/vnd.apache.parquet")
        || !asset.roles.iter().any(|role| role == "data")
    {
        return Err(OvertureError::InvalidPartition(
            "selected STAC asset is not a data GeoParquet asset".to_string(),
        ));
    }
    validate_partition_asset_uri(&admission.asset_key, &asset.href, release, selection)?;
    if admission.expected_partition_bytes < MIN_PARQUET_BYTES {
        return Err(OvertureError::InvalidExpectation(
            "partition byte length is too small for Parquet",
        ));
    }
    let partition_sha256 = normalize_sha256(&admission.expected_partition_sha256)?;
    if asset
        .expected_bytes()
        .is_some_and(|bytes| bytes != admission.expected_partition_bytes)
        || asset
            .sha256()
            .is_some_and(|sha256| sha256 != partition_sha256)
    {
        return Err(OvertureError::InvalidPartition(
            "STAC asset identity conflicts with the acquisition identity".to_string(),
        ));
    }
    let row_count = required_property_u64(&document, "num_rows", MAX_ITEM_ROWS)?;
    let row_group_count_u64 =
        required_property_u64(&document, "num_row_groups", MAX_ITEM_ROW_GROUPS)?;
    let row_group_count = u32::try_from(row_group_count_u64).map_err(|_| {
        OvertureError::InvalidPartition("row group count overflows u32".to_string())
    })?;

    let mut file = open_exact_partition(
        &admission.local_partition_path,
        admission.expected_partition_bytes,
        &partition_sha256,
    )?;
    validate_parquet_magic(&mut file, admission.expected_partition_bytes)?;

    Ok(VerifiedOverturePartition {
        identity: OverturePartitionIdentity {
            release_id: release.release_id.clone(),
            release_catalog_sha256: release.catalog_sha256.clone(),
            theme: selection.theme.clone(),
            feature_type: selection.feature_type.clone(),
            item_id,
            item_source_uri: admission.item.source_uri,
            item_bytes: admission.item.expected_bytes,
            item_sha256,
            asset_key: admission.asset_key,
            partition_source_uri: asset.href.clone(),
            partition_bytes: admission.expected_partition_bytes,
            partition_sha256,
            advertised_bbox,
            row_count,
            row_group_count,
        },
        file,
        #[cfg(not(any(unix, windows)))]
        fallback_read_lock: Mutex::new(()),
    })
}

fn validate_partition_item(
    release: &OvertureReleaseIdentity,
    selection: &OvertureSelection,
    document: &StacDocument,
    source: &Url,
) -> Result<String, OvertureError> {
    if document.object_type != StacObjectType::Item
        || document.stac_version.as_deref() != Some(OVERTURE_STAC_VERSION)
        || document.collection.as_deref() != Some(selection.feature_type.as_str())
    {
        return Err(OvertureError::InvalidPartition(
            "STAC item type, version, or collection does not match".to_string(),
        ));
    }
    let item_id = document
        .id
        .as_deref()
        .ok_or_else(|| OvertureError::InvalidPartition("STAC item has no id".to_string()))?;
    validate_simple_id("item id", item_id, 64).map_err(|_| {
        OvertureError::InvalidPartition("STAC item id is not a bounded identifier".to_string())
    })?;
    let expected_path = format!(
        "/{}/{}/{}/{}/{}.json",
        release.release_id, selection.theme, selection.feature_type, item_id, item_id
    );
    if source.path() != expected_path {
        return Err(OvertureError::InvalidPartition(
            "STAC item URI does not bind release, theme, type, and item id".to_string(),
        ));
    }
    require_exact_link(document, "self", source.as_str())?;
    require_exact_link(document, "root", &release.catalog_source_uri)?;
    let collection_path = format!(
        "/{}/{}/{}/collection.json",
        release.release_id, selection.theme, selection.feature_type
    );
    let collection_link = document
        .links
        .iter()
        .find(|link| link.rel == "collection")
        .ok_or_else(|| {
            OvertureError::InvalidPartition("STAC item has no collection link".to_string())
        })?;
    let collection_uri = validate_stac_uri(&collection_link.href)?;
    if collection_uri.path() != collection_path {
        return Err(OvertureError::InvalidPartition(
            "STAC collection link does not bind the explicit theme and type".to_string(),
        ));
    }
    Ok(item_id.to_string())
}

fn validate_partition_asset_uri(
    asset_key: &str,
    uri: &str,
    release: &OvertureReleaseIdentity,
    selection: &OvertureSelection,
) -> Result<(), OvertureError> {
    let url = Url::parse(uri).map_err(|_| {
        OvertureError::InvalidPartition("partition asset URI is invalid".to_string())
    })?;
    let allowed_host = matches!(
        (asset_key, url.host_str()),
        (
            "aws",
            Some("overturemaps-us-west-2.s3.us-west-2.amazonaws.com")
        ) | ("azure", Some("overturemapswestus2.blob.core.windows.net"))
    );
    if url.scheme() != "https"
        || !allowed_host
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != uri
    {
        return Err(OvertureError::InvalidPartition(
            "partition asset is not a canonical official HTTPS URI".to_string(),
        ));
    }
    let prefix = format!(
        "/release/{}/theme={}/type={}/",
        release.release_id, selection.theme, selection.feature_type
    );
    let Some(file_name) = url.path().strip_prefix(&prefix) else {
        return Err(OvertureError::InvalidPartition(
            "partition asset path does not bind release, theme, and type".to_string(),
        ));
    };
    if file_name.is_empty() || file_name.contains('/') || !file_name.ends_with(".parquet") {
        return Err(OvertureError::InvalidPartition(
            "partition asset path is not one GeoParquet file".to_string(),
        ));
    }
    Ok(())
}

fn validate_stac_uri(value: &str) -> Result<Url, OvertureError> {
    let url = Url::parse(value)
        .map_err(|_| OvertureError::InvalidStacIdentity("STAC URI is not valid".to_string()))?;
    if url.scheme() != "https"
        || url.host_str() != Some(OVERTURE_STAC_HOST)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != value
    {
        return Err(OvertureError::InvalidStacIdentity(
            "STAC URI is not a canonical official HTTPS URI".to_string(),
        ));
    }
    Ok(url)
}

fn require_exact_link(
    document: &StacDocument,
    relation: &str,
    expected: &str,
) -> Result<(), OvertureError> {
    if document
        .links
        .iter()
        .any(|link| link.rel == relation && link.href == expected)
    {
        return Ok(());
    }
    Err(OvertureError::InvalidStacIdentity(format!(
        "STAC {relation} link does not match the exact expected document"
    )))
}

fn required_property_u64(
    document: &StacDocument,
    name: &str,
    maximum: u64,
) -> Result<u64, OvertureError> {
    let value = document
        .properties
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            OvertureError::InvalidPartition(format!("STAC item has no integer {name}"))
        })?;
    if value == 0 || value > maximum {
        return Err(OvertureError::InvalidPartition(format!(
            "STAC item {name} is outside supported bounds"
        )));
    }
    Ok(value)
}

fn overture_bbox(document: &StacDocument) -> Result<BoundingBox, OvertureError> {
    let bbox = document
        .bbox
        .as_deref()
        .ok_or_else(|| OvertureError::InvalidPartition("STAC item has no bbox".to_string()))?;
    let [west, south, east, north] = bbox else {
        return Err(OvertureError::InvalidPartition(
            "Overture STAC item bbox must have four axes".to_string(),
        ));
    };
    let bbox = BoundingBox {
        west: *west,
        south: *south,
        east: *east,
        north: *north,
    };
    bbox.validate()
        .map_err(|_| OvertureError::InvalidPartition("STAC item bbox is invalid".to_string()))?;
    Ok(bbox)
}

pub(crate) fn bbox_intersects(left: BoundingBox, right: BoundingBox) -> bool {
    left.west <= right.east
        && left.east >= right.west
        && left.south <= right.north
        && left.north >= right.south
}

fn verify_exact_bytes(
    kind: &'static str,
    expectation: &ExactStacDocument,
    bytes: &[u8],
) -> Result<String, OvertureError> {
    if expectation.expected_bytes == 0
        || expectation.expected_bytes > MAX_STAC_DOCUMENT_BYTES as u64
    {
        return Err(OvertureError::InvalidExpectation(
            "STAC byte length is zero or exceeds the hard limit",
        ));
    }
    let actual = u64::try_from(bytes.len())
        .map_err(|_| OvertureError::InvalidExpectation("STAC byte length overflows u64"))?;
    if actual != expectation.expected_bytes {
        return Err(OvertureError::ByteLengthMismatch {
            kind,
            expected: expectation.expected_bytes,
            actual,
        });
    }
    let expected = normalize_sha256(&expectation.expected_sha256)?;
    let observed = sha256_hex(bytes);
    if observed != expected {
        return Err(OvertureError::Sha256Mismatch(kind));
    }
    Ok(observed)
}

fn open_exact_partition(
    path: &PathBuf,
    expected_bytes: u64,
    expected_sha256: &str,
) -> Result<File, OvertureError> {
    let path_metadata = fs::symlink_metadata(path)
        .map_err(|source| OvertureError::io("reading partition path metadata", source))?;
    if path_metadata.file_type().is_symlink() {
        return Err(OvertureError::SymlinkPartition);
    }
    if !path_metadata.is_file() {
        return Err(OvertureError::NotRegularPartition);
    }
    let mut file = File::open(path)
        .map_err(|source| OvertureError::io("opening partition read-only", source))?;
    let opened_metadata = file
        .metadata()
        .map_err(|source| OvertureError::io("reading opened partition metadata", source))?;
    if !opened_metadata.is_file() {
        return Err(OvertureError::NotRegularPartition);
    }
    if !same_opened_file(&path_metadata, &opened_metadata) {
        return Err(OvertureError::PartitionChanged);
    }
    if opened_metadata.len() != expected_bytes {
        return Err(OvertureError::ByteLengthMismatch {
            kind: "GeoParquet partition",
            expected: expected_bytes,
            actual: opened_metadata.len(),
        });
    }
    if hash_file(&mut file)? != expected_sha256 {
        return Err(OvertureError::Sha256Mismatch("GeoParquet partition"));
    }
    let current_path_metadata = fs::symlink_metadata(path)
        .map_err(|source| OvertureError::io("rechecking partition path metadata", source))?;
    if current_path_metadata.file_type().is_symlink()
        || !same_opened_file(&current_path_metadata, &opened_metadata)
    {
        return Err(OvertureError::PartitionChanged);
    }
    Ok(file)
}

#[cfg(unix)]
fn same_opened_file(path_metadata: &Metadata, opened_metadata: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    path_metadata.dev() == opened_metadata.dev()
        && path_metadata.ino() == opened_metadata.ino()
        && path_metadata.len() == opened_metadata.len()
}

#[cfg(not(unix))]
fn same_opened_file(path_metadata: &Metadata, opened_metadata: &Metadata) -> bool {
    path_metadata.len() == opened_metadata.len()
        && path_metadata.modified().ok() == opened_metadata.modified().ok()
}

fn validate_parquet_magic(file: &mut File, len: u64) -> Result<(), OvertureError> {
    if len < MIN_PARQUET_BYTES {
        return Err(OvertureError::InvalidPartition(
            "partition is too small for a Parquet envelope".to_string(),
        ));
    }
    let mut leading = [0_u8; 4];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut leading))
        .map_err(|source| OvertureError::io("reading Parquet leading magic", source))?;
    let mut trailing = [0_u8; 4];
    file.seek(SeekFrom::End(-4))
        .and_then(|_| file.read_exact(&mut trailing))
        .map_err(|source| OvertureError::io("reading Parquet trailing magic", source))?;
    if &leading != PARQUET_MAGIC || &trailing != PARQUET_MAGIC {
        return Err(OvertureError::InvalidPartition(
            "partition does not have a Parquet envelope".to_string(),
        ));
    }
    Ok(())
}

fn hash_file(file: &mut File) -> Result<String, OvertureError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| OvertureError::io("seeking partition for hashing", source))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| OvertureError::io("hashing partition", source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn read_exact_at(file: &File, offset: u64, buffer: &mut [u8]) -> Result<(), OvertureError> {
    use std::os::unix::fs::FileExt;

    let mut filled = 0_usize;
    while filled < buffer.len() {
        let relative = u64::try_from(filled)
            .map_err(|_| OvertureError::LimitExceeded("partition read range"))?;
        let current = offset
            .checked_add(relative)
            .ok_or(OvertureError::LimitExceeded("partition read range"))?;
        let read = file
            .read_at(&mut buffer[filled..], current)
            .map_err(|source| OvertureError::io("reading verified partition range", source))?;
        if read == 0 {
            return Err(OvertureError::PartitionChanged);
        }
        filled = filled
            .checked_add(read)
            .ok_or(OvertureError::LimitExceeded("partition read range"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, offset: u64, buffer: &mut [u8]) -> Result<(), OvertureError> {
    use std::os::windows::fs::FileExt;

    let mut filled = 0_usize;
    while filled < buffer.len() {
        let relative = u64::try_from(filled)
            .map_err(|_| OvertureError::LimitExceeded("partition read range"))?;
        let current = offset
            .checked_add(relative)
            .ok_or(OvertureError::LimitExceeded("partition read range"))?;
        let read = file
            .seek_read(&mut buffer[filled..], current)
            .map_err(|source| OvertureError::io("reading verified partition range", source))?;
        if read == 0 {
            return Err(OvertureError::PartitionChanged);
        }
        filled = filled
            .checked_add(read)
            .ok_or(OvertureError::LimitExceeded("partition read range"))?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_exact_at(file: &File, offset: u64, buffer: &mut [u8]) -> Result<(), OvertureError> {
    let mut reader = file
        .try_clone()
        .map_err(|source| OvertureError::io("cloning verified partition handle", source))?;
    reader
        .seek(SeekFrom::Start(offset))
        .and_then(|_| reader.read_exact(buffer))
        .map_err(|source| OvertureError::io("reading verified partition range", source))
}

fn normalize_sha256(value: &str) -> Result<String, OvertureError> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(OvertureError::InvalidExpectation(
            "SHA-256 must contain exactly 64 hexadecimal digits",
        ));
    }
    Ok(digest.to_ascii_lowercase())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_release_id(value: &str) -> Result<(), OvertureError> {
    let Some((date, revision)) = value.split_once('.') else {
        return Err(OvertureError::InvalidStacIdentity(
            "release id is not YYYY-MM-DD.revision".to_string(),
        ));
    };
    let date_bytes = date.as_bytes();
    if date_bytes.len() != 10
        || date_bytes[4] != b'-'
        || date_bytes[7] != b'-'
        || !date_bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
        || revision.is_empty()
        || !revision.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(OvertureError::InvalidStacIdentity(
            "release id is not YYYY-MM-DD.revision".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_simple_id(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), OvertureError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(OvertureError::InvalidSelection(field));
    }
    Ok(())
}
