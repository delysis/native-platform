use crate::CatalogError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

/// Exact schema identifier accepted by the v1 source-registry parser.
pub const SOURCE_REGISTRY_SCHEMA: &str = "information_native.catalog_sources.v1";
/// Defensive ceiling for an operator-supplied source registry.
pub const DEFAULT_MAX_SOURCE_REGISTRY_BYTES: usize = 1024 * 1024;

const MAX_SOURCE_REGISTRY_ENTRIES: usize = 4_096;
const MAX_SOURCE_ID_BYTES: usize = 128;
const MAX_SOURCE_URL_BYTES: usize = 16 * 1024;
const MAX_SOURCE_NOTES_CHARS: usize = 8_192;
const KIWIX_OPDS_URL: &str = "https://opds.library.kiwix.org/catalog/v2/entries";
const OVERTURE_STAC_URL: &str = "https://stac.overturemaps.org/catalog.json";

/// A validated list of upstream discovery leads and their shipped-support
/// status. Registry membership never grants network or installation authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceRegistry {
    pub schema: String,
    pub sources: Vec<SourceRegistryEntry>,
}

impl SourceRegistry {
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.schema != SOURCE_REGISTRY_SCHEMA {
            return Err(invalid_registry(format!(
                "schema must be {SOURCE_REGISTRY_SCHEMA:?}"
            )));
        }
        if self.sources.len() > MAX_SOURCE_REGISTRY_ENTRIES {
            return Err(invalid_registry(format!(
                "sources contains {} entries, exceeding the v1 limit of {MAX_SOURCE_REGISTRY_ENTRIES}",
                self.sources.len()
            )));
        }

        let mut source_ids = BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            if !source_ids.insert(source.id.as_str()) {
                return Err(invalid_registry(format!(
                    "source id {:?} occurs more than once",
                    source.id
                )));
            }
        }
        Ok(())
    }
}

/// One provider endpoint. `support` states only what this release ships; it is
/// not an instruction to execute the endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceRegistryEntry {
    pub id: String,
    pub kind: SourceRegistryKind,
    pub url: Url,
    pub support: SourceRegistrySupport,
    pub trust: SourceRegistryTrust,
    pub notes: String,
}

impl SourceRegistryEntry {
    pub fn validate(&self) -> Result<(), CatalogError> {
        validate_source_id(&self.id)?;
        validate_source_url(&self.url)?;
        match self.support {
            SourceRegistrySupport::ShippedDiscoveryAndExactMetalink
                if self.id != "kiwix"
                    || self.kind != SourceRegistryKind::Opds12
                    || self.url.as_str() != KIWIX_OPDS_URL =>
            {
                return Err(invalid_registry(format!(
                    "source {:?} overclaims shipped Kiwix OPDS and Metalink support",
                    self.id
                )));
            }
            SourceRegistrySupport::ShippedBoundedDiscovery
                if self.id != "overture-maps"
                    || self.kind != SourceRegistryKind::Stac11
                    || self.url.as_str() != OVERTURE_STAC_URL =>
            {
                return Err(invalid_registry(format!(
                    "source {:?} overclaims shipped Overture STAC discovery support",
                    self.id
                )));
            }
            _ => {}
        }
        let note_chars = self.notes.chars().count();
        if self.notes.trim().is_empty()
            || self.notes.trim() != self.notes
            || note_chars > MAX_SOURCE_NOTES_CHARS
        {
            return Err(invalid_registry(format!(
                "source {:?} notes must contain 1 to {MAX_SOURCE_NOTES_CHARS} characters with no surrounding whitespace",
                self.id
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceRegistryKind {
    Opds,
    #[serde(rename = "opds_1_2")]
    Opds12,
    #[serde(rename = "stac_1_1")]
    Stac11,
    JsonIndex,
    ProviderAdapter,
    PartitionManifest,
    DirectoryIndex,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceRegistrySupport {
    /// Bounded OPDS discovery plus exact Metalink 4 payload resolution ships.
    ShippedDiscoveryAndExactMetalink,
    /// Bounded discovery ships, but materialization and querying do not.
    ShippedBoundedDiscovery,
    /// This is an operator-visible lead with no shipped provider adapter.
    RegistryOnly,
}

impl SourceRegistrySupport {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ShippedDiscoveryAndExactMetalink => "shipped_discovery_and_exact_metalink",
            Self::ShippedBoundedDiscovery => "shipped_bounded_discovery",
            Self::RegistryOnly => "registry_only",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceRegistryTrust {
    /// Upstream listing bytes are discovery evidence, not installation authority.
    UpstreamUnsigned,
}

pub fn parse_source_registry(bytes: &[u8]) -> Result<SourceRegistry, CatalogError> {
    parse_source_registry_with_limit(bytes, DEFAULT_MAX_SOURCE_REGISTRY_BYTES)
}

pub fn parse_source_registry_with_limit(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<SourceRegistry, CatalogError> {
    if bytes.len() > max_bytes {
        return Err(CatalogError::SourceRegistryInputTooLarge {
            actual: bytes.len(),
            limit: max_bytes,
        });
    }
    let registry: SourceRegistry =
        serde_json::from_slice(bytes).map_err(CatalogError::SourceRegistryJson)?;
    registry.validate()?;
    Ok(registry)
}

fn validate_source_id(id: &str) -> Result<(), CatalogError> {
    let bytes = id.as_bytes();
    let valid_edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if bytes.is_empty()
        || bytes.len() > MAX_SOURCE_ID_BYTES
        || !valid_edge(bytes[0])
        || !valid_edge(bytes[bytes.len() - 1])
        || !bytes.iter().all(|byte| valid_edge(*byte) || *byte == b'-')
    {
        return Err(invalid_registry(format!(
            "source id {id:?} must be 1 to {MAX_SOURCE_ID_BYTES} bytes of lowercase ASCII letters, digits, or interior hyphens"
        )));
    }
    Ok(())
}

fn validate_source_url(url: &Url) -> Result<(), CatalogError> {
    if url.as_str().len() > MAX_SOURCE_URL_BYTES {
        return Err(invalid_registry(format!(
            "source URL exceeds the v1 limit of {MAX_SOURCE_URL_BYTES} bytes"
        )));
    }
    if url.scheme() != "https" || url.host_str().is_none() || url.cannot_be_a_base() {
        return Err(invalid_registry(
            "source URL must be an absolute HTTPS URL with a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_registry(
            "source URL must not contain embedded credentials",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid_registry(
            "source URL must not persist a query or fragment",
        ));
    }
    Ok(())
}

fn invalid_registry(message: impl Into<String>) -> CatalogError {
    CatalogError::InvalidSourceRegistry(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
      "schema":"information_native.catalog_sources.v1",
      "sources":[
        {
          "id":"kiwix",
          "kind":"opds_1_2",
          "url":"https://opds.library.kiwix.org/catalog/v2/entries",
          "support":"shipped_discovery_and_exact_metalink",
          "trust":"upstream_unsigned",
          "notes":"Bounded discovery only."
        },
        {
          "id":"future-source",
          "kind":"provider_adapter",
          "url":"https://example.invalid/future",
          "support":"registry_only",
          "trust":"upstream_unsigned",
          "notes":"No shipped adapter."
        }
      ]
    }"#;

    #[test]
    fn parses_all_support_levels_from_the_shipped_registry() -> Result<(), CatalogError> {
        let bytes = include_bytes!("../../../catalogues/default-sources.json");
        let registry = parse_source_registry(bytes)?;
        let supports = registry
            .sources
            .iter()
            .map(|source| source.support)
            .collect::<BTreeSet<_>>();

        assert!(supports.contains(&SourceRegistrySupport::ShippedDiscoveryAndExactMetalink));
        assert!(supports.contains(&SourceRegistrySupport::ShippedBoundedDiscovery));
        assert!(supports.contains(&SourceRegistrySupport::RegistryOnly));
        Ok(())
    }

    #[test]
    fn rejects_unknown_fields_duplicate_ids_and_duplicate_object_fields() {
        let unknown = VALID.replace(
            "\"schema\":\"information_native.catalog_sources.v1\"",
            "\"schema\":\"information_native.catalog_sources.v1\",\"extra\":true",
        );
        assert!(matches!(
            parse_source_registry(unknown.as_bytes()),
            Err(CatalogError::SourceRegistryJson(_))
        ));
        let unknown_entry = VALID.replace(
            "\"notes\":\"Bounded discovery only.\"",
            "\"notes\":\"Bounded discovery only.\",\"extra\":true",
        );
        assert!(matches!(
            parse_source_registry(unknown_entry.as_bytes()),
            Err(CatalogError::SourceRegistryJson(_))
        ));

        let duplicate_id = VALID.replace("future-source", "kiwix");
        assert!(matches!(
            parse_source_registry(duplicate_id.as_bytes()),
            Err(CatalogError::InvalidSourceRegistry(_))
        ));

        let duplicate_field =
            VALID.replace("\"id\":\"kiwix\"", "\"id\":\"kiwix\",\"id\":\"other\"");
        assert!(matches!(
            parse_source_registry(duplicate_field.as_bytes()),
            Err(CatalogError::SourceRegistryJson(_))
        ));
    }

    #[test]
    fn rejects_non_https_relative_and_credentialed_urls() {
        for invalid_url in [
            "http://example.invalid/catalog",
            "relative/catalog.json",
            "https://user:secret@example.invalid/catalog",
            "https://example.invalid/catalog?signature=secret",
            "https://example.invalid/catalog#section",
        ] {
            let document = VALID.replace(KIWIX_OPDS_URL, invalid_url);
            assert!(parse_source_registry(document.as_bytes()).is_err());
        }
    }

    #[test]
    fn shipped_support_is_bound_to_the_implemented_endpoint() {
        let spoofed = VALID.replace(KIWIX_OPDS_URL, "https://example.invalid/catalog.xml");
        assert!(matches!(
            parse_source_registry(spoofed.as_bytes()),
            Err(CatalogError::InvalidSourceRegistry(_))
        ));
    }

    #[test]
    fn rejects_unknown_support_and_input_over_limit() {
        let wrong_schema = VALID.replace(
            SOURCE_REGISTRY_SCHEMA,
            "information_native.catalog_sources.v2",
        );
        assert!(matches!(
            parse_source_registry(wrong_schema.as_bytes()),
            Err(CatalogError::InvalidSourceRegistry(_))
        ));
        let unknown_support =
            VALID.replace("shipped_discovery_and_exact_metalink", "fully_executable");
        assert!(matches!(
            parse_source_registry(unknown_support.as_bytes()),
            Err(CatalogError::SourceRegistryJson(_))
        ));
        assert!(matches!(
            parse_source_registry_with_limit(VALID.as_bytes(), VALID.len() - 1),
            Err(CatalogError::SourceRegistryInputTooLarge { .. })
        ));
    }

    #[test]
    fn registry_only_provider_cannot_overclaim_a_shipped_adapter() {
        let overclaimed = VALID.replacen(
            "\"support\":\"registry_only\"",
            "\"support\":\"shipped_bounded_discovery\"",
            1,
        );
        assert!(matches!(
            parse_source_registry(overclaimed.as_bytes()),
            Err(CatalogError::InvalidSourceRegistry(_))
        ));
    }
}
