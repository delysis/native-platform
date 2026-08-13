use crate::{
    CatalogError, DEFAULT_MAX_DISCOVERY_BYTES, DiscoveryAsset, DiscoveryProvider, DiscoveryRecord,
    resolve_provider_href, validate_provider_source_uri,
};
use chrono::{DateTime, Utc};
use information_native_types::ResourceKind;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use url::Url;

const MAX_FEATURES: usize = 50_000;
const MAX_FEATURE_DEPTH: usize = 4;
const MAX_LINKS: usize = 50_000;
const MAX_ASSETS: usize = 50_000;
const MAX_STRING_VALUES: usize = 4_096;
const MAX_OBJECT_MEMBERS: usize = 100_000;
const MAX_FIELD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StacObjectType {
    Catalog,
    Collection,
    Item,
    FeatureCollection,
}

/// A normalized, typed STAC document. Unknown extension members are retained
/// as data and never interpreted as install authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StacDocument {
    pub source_uri: String,
    pub stac_version: Option<String>,
    pub object_type: StacObjectType,
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub collection: Option<String>,
    pub license: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub bbox: Option<Vec<f64>>,
    pub geometry: Option<Value>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
    #[serde(default)]
    pub links: Vec<StacLink>,
    #[serde(default)]
    pub assets: BTreeMap<String, StacAsset>,
    #[serde(default)]
    pub features: Vec<StacDocument>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

impl StacDocument {
    pub fn child_links(&self) -> impl Iterator<Item = &StacLink> {
        self.links.iter().filter(|link| link.rel == "child")
    }

    /// Overture and some other catalogues annotate one child with
    /// `latest: true`. Multiple such links are resolved deterministically.
    #[must_use]
    pub fn latest_child(&self) -> Option<&StacLink> {
        let mut candidates: Vec<_> = self.child_links().filter(|link| link.is_latest()).collect();
        candidates.sort_by(|left, right| left.href.cmp(&right.href));
        candidates.into_iter().next()
    }

    /// Convert navigational catalogues, collections, items, and item-search
    /// pages into provider-neutral discovery records.
    #[must_use]
    pub fn discovery_records(&self) -> Vec<DiscoveryRecord> {
        match self.object_type {
            StacObjectType::Catalog => self
                .child_links()
                .map(|link| self.link_discovery(link))
                .collect(),
            StacObjectType::FeatureCollection => self
                .features
                .iter()
                .flat_map(StacDocument::discovery_records)
                .collect(),
            StacObjectType::Collection | StacObjectType::Item => {
                vec![self.object_discovery()]
            }
        }
    }

    fn link_discovery(&self, link: &StacLink) -> DiscoveryRecord {
        let upstream_id = link
            .title
            .clone()
            .or_else(|| id_from_href(&link.href))
            .unwrap_or_else(|| link.href.clone());
        let mut metadata = BTreeMap::new();
        if link.is_latest() {
            metadata.insert("latest".to_string(), Value::Bool(true));
        }
        DiscoveryRecord {
            provider: DiscoveryProvider::Stac,
            upstream_id: upstream_id.clone(),
            title: link.title.clone().unwrap_or(upstream_id),
            summary: self.description.clone().unwrap_or_default(),
            kind: ResourceKind::Map,
            languages: Vec::new(),
            subjects: self.keywords.clone(),
            published_at: None,
            landing_uri: Some(link.href.clone()),
            assets: Vec::new(),
            metadata,
        }
    }

    fn object_discovery(&self) -> DiscoveryRecord {
        let upstream_id = self.id.clone().unwrap_or_else(|| self.source_uri.clone());
        let title = self.title.clone().unwrap_or_else(|| upstream_id.clone());
        let assets = self
            .assets
            .iter()
            .map(|(key, asset)| DiscoveryAsset {
                key: key.clone(),
                href: asset.href.clone(),
                media_type: asset.media_type.clone(),
                expected_bytes: asset.expected_bytes(),
                sha256: asset.sha256(),
                roles: asset.roles.clone(),
                requires_resolution: Url::parse(&asset.href)
                    .ok()
                    .is_none_or(|url| !matches!(url.scheme(), "http" | "https" | "file")),
            })
            .collect();
        let published_at = self
            .properties
            .get("datetime")
            .or_else(|| self.properties.get("start_datetime"))
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|date| date.with_timezone(&Utc));
        let languages = property_strings(&self.properties, "languages")
            .or_else(|| property_strings(&self.properties, "language"))
            .unwrap_or_default();
        let mut metadata = self.properties.clone();
        if let Some(license) = &self.license {
            metadata.insert("license".to_string(), Value::String(license.clone()));
        }
        DiscoveryRecord {
            provider: DiscoveryProvider::Stac,
            upstream_id,
            title,
            summary: self.description.clone().unwrap_or_default(),
            kind: ResourceKind::Map,
            languages,
            subjects: self.keywords.clone(),
            published_at,
            landing_uri: Some(self.source_uri.clone()),
            assets,
            metadata,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StacLink {
    pub rel: String,
    /// Absolute URI resolved against the document URI.
    pub href: String,
    /// The source spelling retained for auditing relative-link resolution.
    pub original_href: String,
    pub media_type: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

impl StacLink {
    #[must_use]
    pub fn is_latest(&self) -> bool {
        self.extensions
            .get("latest")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StacAsset {
    /// Absolute URI resolved against the document URI.
    pub href: String,
    pub original_href: String,
    pub media_type: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

impl StacAsset {
    #[must_use]
    pub fn expected_bytes(&self) -> Option<u64> {
        self.extensions
            .get("file:size")
            .or_else(|| self.extensions.get("file_size"))
            .and_then(Value::as_u64)
    }

    /// Only explicit hexadecimal SHA-256 spellings are accepted. STAC
    /// multihashes are retained in `extensions` until a multihash resolver is
    /// intentionally added.
    #[must_use]
    pub fn sha256(&self) -> Option<String> {
        self.extensions
            .get("file:checksum")
            .or_else(|| self.extensions.get("sha256"))
            .and_then(Value::as_str)
            .and_then(normalize_sha256)
    }
}

pub fn parse_stac_document(bytes: &[u8], source_uri: &Url) -> Result<StacDocument, CatalogError> {
    parse_stac_document_with_limit(bytes, source_uri, DEFAULT_MAX_DISCOVERY_BYTES)
}

pub fn parse_stac_document_with_limit(
    bytes: &[u8],
    source_uri: &Url,
    max_bytes: usize,
) -> Result<StacDocument, CatalogError> {
    if bytes.len() > max_bytes {
        return Err(CatalogError::InputTooLarge {
            actual: bytes.len(),
            limit: max_bytes,
        });
    }
    validate_provider_source_uri(source_uri)?;
    let value: Value = serde_json::from_slice(bytes)?;
    parse_document(value, source_uri, 0)
}

fn parse_document(
    value: Value,
    source_uri: &Url,
    depth: usize,
) -> Result<StacDocument, CatalogError> {
    if depth > MAX_FEATURE_DEPTH {
        return Err(CatalogError::Provider(format!(
            "STAC feature nesting exceeds {MAX_FEATURE_DEPTH} levels"
        )));
    }
    let mut object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(CatalogError::Provider(
                "STAC document must be a JSON object".to_string(),
            ));
        }
    };
    if object.len() > MAX_OBJECT_MEMBERS {
        return Err(CatalogError::Provider(format!(
            "STAC object exceeds {MAX_OBJECT_MEMBERS} members"
        )));
    }
    let type_name = take_required_string(&mut object, "type")?;
    let object_type = match type_name.as_str() {
        "Catalog" => StacObjectType::Catalog,
        "Collection" => StacObjectType::Collection,
        "Feature" => StacObjectType::Item,
        "FeatureCollection" => StacObjectType::FeatureCollection,
        _ => {
            return Err(CatalogError::Provider(format!(
                "unsupported STAC object type {type_name:?}"
            )));
        }
    };
    let stac_version = take_optional_string(&mut object, "stac_version")?;
    if object_type != StacObjectType::FeatureCollection && stac_version.is_none() {
        return Err(CatalogError::Provider(
            "STAC object has no stac_version".to_string(),
        ));
    }
    let id = take_optional_string(&mut object, "id")?;
    if object_type != StacObjectType::FeatureCollection && id.is_none() {
        return Err(CatalogError::Provider("STAC object has no id".to_string()));
    }
    let title = take_optional_string(&mut object, "title")?;
    let description = take_optional_string(&mut object, "description")?;
    if matches!(
        object_type,
        StacObjectType::Catalog | StacObjectType::Collection
    ) && description.is_none()
    {
        return Err(CatalogError::Provider(
            "STAC catalog or collection has no description".to_string(),
        ));
    }
    let collection = take_optional_string(&mut object, "collection")?;
    let license = take_optional_string(&mut object, "license")?;
    let keywords = take_string_array(&mut object, "keywords")?.unwrap_or_default();
    let bbox = take_bbox(&mut object)?;
    let has_geometry = object.contains_key("geometry");
    let geometry = object.remove("geometry");
    let has_properties = object.contains_key("properties");
    let properties = take_object(&mut object, "properties")?.unwrap_or_default();
    if properties.len() > MAX_OBJECT_MEMBERS {
        return Err(CatalogError::Provider(format!(
            "STAC properties exceed {MAX_OBJECT_MEMBERS} members"
        )));
    }
    validate_property_string_values(&properties, "languages")?;
    validate_property_string_values(&properties, "language")?;
    if object_type == StacObjectType::Item && (!has_geometry || !has_properties) {
        return Err(CatalogError::Provider(
            "STAC item must contain geometry and properties".to_string(),
        ));
    }
    let links = parse_links(object.remove("links"), source_uri)?;
    let assets = parse_assets(object.remove("assets"), source_uri)?;
    let feature_value = object.remove("features");
    if object_type != StacObjectType::FeatureCollection && feature_value.is_some() {
        return Err(CatalogError::Provider(
            "only a STAC FeatureCollection may contain features".to_string(),
        ));
    }
    let features = parse_features(feature_value, source_uri, depth)?;

    if object_type == StacObjectType::FeatureCollection && features.len() > MAX_FEATURES {
        return Err(CatalogError::Provider(format!(
            "STAC response exceeds {MAX_FEATURES} features"
        )));
    }

    Ok(StacDocument {
        source_uri: source_uri.to_string(),
        stac_version,
        object_type,
        id,
        title,
        description,
        collection,
        license,
        keywords,
        bbox,
        geometry,
        properties: properties.into_iter().collect(),
        links,
        assets,
        features,
        extensions: object.into_iter().collect(),
    })
}

fn parse_links(value: Option<Value>, source_uri: &Url) -> Result<Vec<StacLink>, CatalogError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let links = match value {
        Value::Array(links) => links,
        _ => {
            return Err(CatalogError::Provider(
                "STAC links must be an array".to_string(),
            ));
        }
    };
    if links.len() > MAX_LINKS {
        return Err(CatalogError::Provider(format!(
            "STAC document exceeds {MAX_LINKS} links"
        )));
    }
    let mut parsed = Vec::with_capacity(links.len());
    for value in links {
        let mut object = match value {
            Value::Object(object) => object,
            _ => {
                return Err(CatalogError::Provider(
                    "STAC link must be an object".to_string(),
                ));
            }
        };
        let rel = take_required_string(&mut object, "rel")?;
        let original_href = take_required_string(&mut object, "href")?;
        let href = resolve_provider_href(source_uri, &original_href)?;
        parsed.push(StacLink {
            rel,
            href,
            original_href,
            media_type: take_optional_string(&mut object, "type")?,
            title: take_optional_string(&mut object, "title")?,
            extensions: object.into_iter().collect(),
        });
    }
    Ok(parsed)
}

fn parse_assets(
    value: Option<Value>,
    source_uri: &Url,
) -> Result<BTreeMap<String, StacAsset>, CatalogError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let assets = match value {
        Value::Object(assets) => assets,
        _ => {
            return Err(CatalogError::Provider(
                "STAC assets must be an object".to_string(),
            ));
        }
    };
    if assets.len() > MAX_ASSETS {
        return Err(CatalogError::Provider(format!(
            "STAC document exceeds {MAX_ASSETS} assets"
        )));
    }
    let mut parsed = BTreeMap::new();
    for (key, value) in assets {
        validate_string_value("asset key", &key)?;
        let mut object = match value {
            Value::Object(object) => object,
            _ => {
                return Err(CatalogError::Provider(
                    "STAC asset must be an object".to_string(),
                ));
            }
        };
        let original_href = take_required_string(&mut object, "href")?;
        let href = resolve_provider_href(source_uri, &original_href)?;
        parsed.insert(
            key,
            StacAsset {
                href,
                original_href,
                media_type: take_optional_string(&mut object, "type")?,
                title: take_optional_string(&mut object, "title")?,
                description: take_optional_string(&mut object, "description")?,
                roles: take_string_array(&mut object, "roles")?.unwrap_or_default(),
                extensions: object.into_iter().collect(),
            },
        );
    }
    Ok(parsed)
}

fn parse_features(
    value: Option<Value>,
    source_uri: &Url,
    depth: usize,
) -> Result<Vec<StacDocument>, CatalogError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let features = match value {
        Value::Array(features) => features,
        _ => {
            return Err(CatalogError::Provider(
                "STAC features must be an array".to_string(),
            ));
        }
    };
    if features.len() > MAX_FEATURES {
        return Err(CatalogError::Provider(format!(
            "STAC response exceeds {MAX_FEATURES} features"
        )));
    }
    let parsed: Vec<_> = features
        .into_iter()
        .map(|feature| parse_document(feature, source_uri, depth.saturating_add(1)))
        .collect::<Result<_, _>>()?;
    if parsed
        .iter()
        .any(|feature| feature.object_type != StacObjectType::Item)
    {
        return Err(CatalogError::Provider(
            "STAC FeatureCollection contains a non-Feature object".to_string(),
        ));
    }
    Ok(parsed)
}

fn take_required_string(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<String, CatalogError> {
    take_optional_string(object, field)?
        .ok_or_else(|| CatalogError::Provider(format!("required STAC field {field:?} is missing")))
}

fn take_optional_string(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<Option<String>, CatalogError> {
    let Some(value) = object.remove(field) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| CatalogError::Provider(format!("STAC field {field:?} must be a string")))?;
    if value.trim().is_empty() {
        return Err(CatalogError::Provider(format!(
            "STAC field {field:?} cannot be empty"
        )));
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(CatalogError::Provider(format!(
            "STAC field {field:?} exceeds {MAX_FIELD_BYTES} bytes"
        )));
    }
    Ok(Some(value.to_string()))
}

fn take_string_array(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<Option<Vec<String>>, CatalogError> {
    let Some(value) = object.remove(field) else {
        return Ok(None);
    };
    let values = match value {
        Value::String(single) => {
            validate_string_value(field, &single)?;
            return Ok(Some(vec![single]));
        }
        Value::Array(values) => values,
        _ => {
            return Err(CatalogError::Provider(format!(
                "STAC field {field:?} must be a string array"
            )));
        }
    };
    if values.len() > MAX_STRING_VALUES {
        return Err(CatalogError::Provider(format!(
            "STAC field {field:?} exceeds {MAX_STRING_VALUES} values"
        )));
    }
    values
        .into_iter()
        .map(|value| {
            let Value::String(value) = value else {
                return Err(CatalogError::Provider(format!(
                    "STAC field {field:?} contains a non-string value"
                )));
            };
            validate_string_value(field, &value)?;
            Ok(value)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn take_object(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<Option<Map<String, Value>>, CatalogError> {
    let Some(value) = object.remove(field) else {
        return Ok(None);
    };
    match value {
        Value::Object(object) => Ok(Some(object)),
        _ => Err(CatalogError::Provider(format!(
            "STAC field {field:?} must be an object"
        ))),
    }
}

fn validate_string_value(field: &str, value: &str) -> Result<(), CatalogError> {
    if value.len() > MAX_FIELD_BYTES {
        return Err(CatalogError::Provider(format!(
            "STAC field {field:?} contains a string longer than {MAX_FIELD_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_property_string_values(
    properties: &Map<String, Value>,
    field: &str,
) -> Result<(), CatalogError> {
    let Some(value) = properties.get(field) else {
        return Ok(());
    };
    match value {
        Value::String(value) => validate_string_value(field, value),
        Value::Array(values) => {
            if values.len() > MAX_STRING_VALUES {
                return Err(CatalogError::Provider(format!(
                    "STAC property {field:?} exceeds {MAX_STRING_VALUES} values"
                )));
            }
            for value in values {
                let value = value.as_str().ok_or_else(|| {
                    CatalogError::Provider(format!(
                        "STAC property {field:?} contains a non-string value"
                    ))
                })?;
                validate_string_value(field, value)?;
            }
            Ok(())
        }
        _ => Err(CatalogError::Provider(format!(
            "STAC property {field:?} must be a string or string array"
        ))),
    }
}

fn take_bbox(object: &mut Map<String, Value>) -> Result<Option<Vec<f64>>, CatalogError> {
    let Some(value) = object.remove("bbox") else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .ok_or_else(|| CatalogError::Provider("STAC bbox must be an array".to_string()))?;
    if !matches!(values.len(), 4 | 6) {
        return Err(CatalogError::Provider(
            "STAC bbox must contain four or six numbers".to_string(),
        ));
    }
    let bbox: Vec<f64> = values
        .iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    CatalogError::Provider("STAC bbox contains a non-finite number".to_string())
                })
        })
        .collect::<Result<_, _>>()?;
    let axes_are_ordered = match bbox.as_slice() {
        [west, south, east, north] => west < east && south < north,
        [west, south, _minimum_z, east, north, _maximum_z] => west < east && south < north,
        _ => false,
    };
    if !axes_are_ordered {
        return Err(CatalogError::Provider(
            "STAC bbox has inverted horizontal axes".to_string(),
        ));
    }
    Ok(Some(bbox))
}

fn normalize_sha256(value: &str) -> Option<String> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

fn id_from_href(href: &str) -> Option<String> {
    Url::parse(href).ok().and_then(|url| {
        url.path_segments().and_then(|mut segments| {
            segments
                .rfind(|segment| !segment.is_empty() && *segment != "catalog.json")
                .map(ToString::to_string)
        })
    })
}

fn property_strings(properties: &BTreeMap<String, Value>, field: &str) -> Option<Vec<String>> {
    let value = properties.get(field)?;
    if let Some(single) = value.as_str() {
        return Some(vec![single.to_string()]);
    }
    value.as_array().and_then(|values| {
        values
            .iter()
            .map(Value::as_str)
            .map(|value| value.map(ToString::to_string))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OVERTURE_ROOT: &[u8] = include_bytes!("../tests/fixtures/stac-overture-root.json");

    #[test]
    fn resolves_relative_children_and_selects_latest_deterministically() -> Result<(), CatalogError>
    {
        let source = Url::parse("https://stac.overturemaps.org/catalog.json")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let document = parse_stac_document(OVERTURE_ROOT, &source)?;
        assert_eq!(document.stac_version.as_deref(), Some("1.1.0"));
        let latest = document
            .latest_child()
            .ok_or_else(|| CatalogError::Provider("latest child missing".to_string()))?;
        assert_eq!(
            latest.href,
            "https://stac.overturemaps.org/2026-07-22.0/catalog.json"
        );
        assert_eq!(document.discovery_records().len(), 2);
        Ok(())
    }

    #[test]
    fn parses_item_assets_without_treating_multihash_as_plain_sha256() -> Result<(), CatalogError> {
        let source = Url::parse("https://example.invalid/items/item.json")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let item = format!(
            r#"{{
                "stac_version":"1.1.0","type":"Feature","id":"places",
                "bbox":[-10,-10,10,10],"geometry":null,
                "properties":{{"datetime":"2026-08-08T00:00:00Z"}},
                "links":[],"assets":{{
                    "data":{{"href":"../places.parquet","type":"application/vnd.apache.parquet","roles":["data"],"file:size":12,"file:checksum":"sha256:{}"}},
                    "other":{{"href":"s3://bucket/other.parquet","checksum:multihash":"1220{}"}}
                }}
            }}"#,
            "a".repeat(64),
            "b".repeat(64)
        );
        let document = parse_stac_document(item.as_bytes(), &source)?;
        let data = document
            .assets
            .get("data")
            .ok_or_else(|| CatalogError::Provider("data asset missing".to_string()))?;
        assert_eq!(data.expected_bytes(), Some(12));
        assert_eq!(data.sha256(), Some("a".repeat(64)));
        let other = document
            .assets
            .get("other")
            .ok_or_else(|| CatalogError::Provider("other asset missing".to_string()))?;
        assert_eq!(other.sha256(), None);
        Ok(())
    }

    #[test]
    fn enforces_explicit_byte_and_structural_value_limits() -> Result<(), CatalogError> {
        let source = Url::parse("https://example.invalid/catalog.json")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let document = br#"{"stac_version":"1.1.0","type":"Catalog","id":"x","description":"x"}"#;
        assert!(matches!(
            parse_stac_document_with_limit(document, &source, document.len() - 1),
            Err(CatalogError::InputTooLarge { .. })
        ));

        let oversized = serde_json::to_vec(&serde_json::json!({
            "stac_version": "1.1.0",
            "type": "Catalog",
            "id": "x",
            "description": "x",
            "keywords": std::iter::repeat_n("tag", MAX_STRING_VALUES + 1).collect::<Vec<_>>(),
        }))?;
        assert!(matches!(
            parse_stac_document(&oversized, &source),
            Err(CatalogError::Provider(_))
        ));
        Ok(())
    }
}
