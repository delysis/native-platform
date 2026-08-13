use crate::{
    CatalogError, DEFAULT_MAX_DISCOVERY_BYTES, DiscoveryAsset, DiscoveryProvider, DiscoveryRecord,
    resolve_provider_href, validate_provider_source_uri,
};
use chrono::{DateTime, Utc};
use information_native_types::ResourceKind;
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

const MAX_FIELD_BYTES: usize = 256 * 1024;
const MAX_ENTRIES: usize = 50_000;
const MAX_LINKS: usize = 100_000;
const MAX_TAGS: usize = 4_096;
const MAX_ATTRIBUTES: usize = 64;
const MAX_XML_DEPTH: usize = 128;
const ACQUISITION_REL_FRAGMENT: &str = "opds-spec.org/acquisition";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KiwixOpdsFeed {
    pub source_uri: String,
    pub id: String,
    pub title: String,
    pub updated: Option<DateTime<Utc>>,
    pub total_results: Option<u64>,
    pub start_index: Option<u64>,
    pub items_per_page: Option<u64>,
    #[serde(default)]
    pub links: Vec<OpdsLink>,
    #[serde(default)]
    pub entries: Vec<KiwixOpdsEntry>,
}

impl KiwixOpdsFeed {
    #[must_use]
    pub fn next_page_uri(&self) -> Option<&str> {
        self.links
            .iter()
            .find(|link| link.rel == "next")
            .map(|link| link.href.as_str())
    }

    #[must_use]
    pub fn discovery_records(&self) -> Vec<DiscoveryRecord> {
        self.entries
            .iter()
            .map(KiwixOpdsEntry::to_discovery_record)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KiwixOpdsEntry {
    pub id: String,
    pub title: String,
    pub updated: Option<DateTime<Utc>>,
    pub issued: Option<DateTime<Utc>>,
    pub summary: String,
    pub language: Option<String>,
    pub name: Option<String>,
    pub flavour: Option<String>,
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub article_count: Option<u64>,
    pub media_count: Option<u64>,
    pub author: Option<String>,
    pub publisher: Option<String>,
    #[serde(default)]
    pub links: Vec<OpdsLink>,
}

impl KiwixOpdsEntry {
    pub fn acquisition_links(&self) -> impl Iterator<Item = &OpdsLink> {
        self.links.iter().filter(|link| link.is_acquisition())
    }

    #[must_use]
    pub fn to_discovery_record(&self) -> DiscoveryRecord {
        let category = self.category.as_deref().unwrap_or_default();
        let name = self.name.as_deref().unwrap_or_default();
        let kind = if category.eq_ignore_ascii_case("wikipedia")
            || name.to_ascii_lowercase().starts_with("wikipedia_")
        {
            ResourceKind::Encyclopedia
        } else {
            ResourceKind::WebArchive
        };
        let subjects = self
            .category
            .iter()
            .chain(self.tags.iter().filter(|tag| !tag.starts_with('_')))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        let assets = self
            .acquisition_links()
            .enumerate()
            .map(|(index, link)| {
                let requires_resolution = link.href.to_ascii_lowercase().ends_with(".meta4")
                    || link
                        .media_type
                        .as_deref()
                        .is_some_and(|media_type| media_type.contains("metalink"));
                DiscoveryAsset {
                    key: format!("acquisition-{index}"),
                    href: link.href.clone(),
                    media_type: link.media_type.clone(),
                    expected_bytes: link.length,
                    sha256: link.sha256.clone(),
                    roles: vec!["data".to_string()],
                    requires_resolution,
                }
            })
            .collect();
        let landing_uri = self
            .links
            .iter()
            .find(|link| link.media_type.as_deref() == Some("text/html"))
            .map(|link| link.href.clone());
        let mut metadata = BTreeMap::new();
        if let Some(name) = &self.name {
            metadata.insert("kiwix_name".to_string(), Value::String(name.clone()));
        }
        if let Some(flavour) = &self.flavour {
            metadata.insert("kiwix_flavour".to_string(), Value::String(flavour.clone()));
        }
        if let Some(author) = &self.author {
            metadata.insert("author".to_string(), Value::String(author.clone()));
        }
        if let Some(publisher) = &self.publisher {
            metadata.insert("publisher".to_string(), Value::String(publisher.clone()));
        }
        if let Some(article_count) = self.article_count {
            metadata.insert("article_count".to_string(), Value::from(article_count));
        }
        if let Some(media_count) = self.media_count {
            metadata.insert("media_count".to_string(), Value::from(media_count));
        }
        DiscoveryRecord {
            provider: DiscoveryProvider::KiwixOpds,
            upstream_id: self.id.clone(),
            title: self.title.clone(),
            summary: self.summary.clone(),
            kind,
            languages: self.language.iter().cloned().collect(),
            subjects,
            published_at: self.issued.or(self.updated),
            landing_uri,
            assets,
            metadata,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpdsLink {
    pub rel: String,
    pub href: String,
    pub media_type: Option<String>,
    pub title: Option<String>,
    pub length: Option<u64>,
    pub sha256: Option<String>,
}

impl OpdsLink {
    #[must_use]
    pub fn is_acquisition(&self) -> bool {
        self.rel
            .split_ascii_whitespace()
            .any(|relation| relation.contains(ACQUISITION_REL_FRAGMENT))
    }
}

pub fn parse_kiwix_opds(bytes: &[u8], source_uri: &Url) -> Result<KiwixOpdsFeed, CatalogError> {
    parse_kiwix_opds_with_limit(bytes, source_uri, DEFAULT_MAX_DISCOVERY_BYTES)
}

pub fn parse_kiwix_opds_with_limit(
    bytes: &[u8],
    source_uri: &Url,
    max_bytes: usize,
) -> Result<KiwixOpdsFeed, CatalogError> {
    if bytes.len() > max_bytes {
        return Err(CatalogError::InputTooLarge {
            actual: bytes.len(),
            limit: max_bytes,
        });
    }
    validate_provider_source_uri(source_uri)?;

    let mut reader = Reader::from_reader(bytes);
    // Entity references are separate events in quick-xml. Trimming each text
    // event would silently turn `A &amp; B` into `A&B`.
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = true;
    let mut path = Vec::<String>::new();
    let mut root_seen = false;
    let mut links_seen = 0_usize;
    let mut feed = FeedBuilder::new(source_uri.to_string());
    let mut entry: Option<EntryBuilder> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = local_name(start.name().as_ref())?;
                if path.is_empty() {
                    if name != "feed" || root_seen {
                        return Err(CatalogError::Provider(
                            "OPDS root element must be exactly one feed".to_string(),
                        ));
                    }
                    root_seen = true;
                }
                if name == "entry" {
                    if path.last().map(String::as_str) != Some("feed") {
                        return Err(CatalogError::Provider(
                            "OPDS entries must be direct children of the feed".to_string(),
                        ));
                    }
                    if entry.is_some() {
                        return Err(CatalogError::Provider(
                            "nested OPDS entries are invalid".to_string(),
                        ));
                    }
                    entry = Some(EntryBuilder::default());
                }
                if name == "link"
                    && matches!(path.last().map(String::as_str), Some("feed" | "entry"))
                {
                    record_link(&mut links_seen)?;
                    let link = parse_link(&start, &reader, source_uri)?;
                    if let Some(current) = entry.as_mut() {
                        current.links.push(link);
                    } else {
                        feed.links.push(link);
                    }
                }
                path.push(name);
                if path.len() > MAX_XML_DEPTH {
                    return Err(CatalogError::Provider(format!(
                        "OPDS XML nesting exceeds {MAX_XML_DEPTH} elements"
                    )));
                }
            }
            Ok(Event::Empty(start)) => {
                let name = local_name(start.name().as_ref())?;
                if path.is_empty() || name == "entry" {
                    return Err(CatalogError::Provider(
                        "OPDS feed and entry elements cannot be empty".to_string(),
                    ));
                }
                if name == "link"
                    && matches!(path.last().map(String::as_str), Some("feed" | "entry"))
                {
                    record_link(&mut links_seen)?;
                    let link = parse_link(&start, &reader, source_uri)?;
                    if let Some(current) = entry.as_mut() {
                        current.links.push(link);
                    } else {
                        feed.links.push(link);
                    }
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|error| CatalogError::Provider(error.to_string()))?;
                validate_xml_10_text(decoded.as_ref())?;
                apply_text(&path, decoded.as_ref(), &mut feed, entry.as_mut())?;
            }
            Ok(Event::CData(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|error| CatalogError::Provider(error.to_string()))?;
                validate_xml_10_text(decoded.as_ref())?;
                apply_text(&path, decoded.as_ref(), &mut feed, entry.as_mut())?;
            }
            Ok(Event::GeneralRef(reference)) => {
                apply_general_reference(&path, &reference, &mut feed, entry.as_mut())?;
            }
            Ok(Event::End(end)) => {
                let name = local_name(end.name().as_ref())?;
                if path.last().is_none_or(|open| open != &name) {
                    return Err(CatalogError::Provider(
                        "OPDS element nesting is invalid".to_string(),
                    ));
                }
                if name == "entry" {
                    let completed = entry.take().ok_or_else(|| {
                        CatalogError::Provider("OPDS entry end without start".to_string())
                    })?;
                    feed.entries.push(completed.finish()?);
                    if feed.entries.len() > MAX_ENTRIES {
                        return Err(CatalogError::Provider(format!(
                            "OPDS feed exceeds {MAX_ENTRIES} entries"
                        )));
                    }
                }
                path.pop();
            }
            Ok(Event::DocType(_)) => {
                return Err(CatalogError::Provider(
                    "OPDS document types are forbidden".to_string(),
                ));
            }
            Ok(Event::Eof) => break,
            Ok(Event::Decl(_)) | Ok(Event::PI(_)) | Ok(Event::Comment(_)) => {}
            Err(error) => return Err(CatalogError::Provider(error.to_string())),
        }
    }

    if !root_seen || entry.is_some() || !path.is_empty() {
        return Err(CatalogError::Provider(
            "OPDS document ended inside an element".to_string(),
        ));
    }
    feed.finish()
}

#[derive(Debug)]
struct FeedBuilder {
    source_uri: String,
    id: String,
    title: String,
    updated: String,
    total_results: String,
    start_index: String,
    items_per_page: String,
    links: Vec<OpdsLink>,
    entries: Vec<KiwixOpdsEntry>,
}

impl FeedBuilder {
    fn new(source_uri: String) -> Self {
        Self {
            source_uri,
            id: String::new(),
            title: String::new(),
            updated: String::new(),
            total_results: String::new(),
            start_index: String::new(),
            items_per_page: String::new(),
            links: Vec::new(),
            entries: Vec::new(),
        }
    }

    fn finish(self) -> Result<KiwixOpdsFeed, CatalogError> {
        require_provider_field("feed.id", &self.id)?;
        require_provider_field("feed.title", &self.title)?;
        Ok(KiwixOpdsFeed {
            source_uri: self.source_uri,
            id: self.id,
            title: self.title,
            updated: parse_date("feed.updated", &self.updated)?,
            total_results: parse_number("feed.totalResults", &self.total_results)?,
            start_index: parse_number("feed.startIndex", &self.start_index)?,
            items_per_page: parse_number("feed.itemsPerPage", &self.items_per_page)?,
            links: self.links,
            entries: self.entries,
        })
    }
}

#[derive(Debug, Default)]
struct EntryBuilder {
    id: String,
    title: String,
    updated: String,
    issued: String,
    summary: String,
    language: String,
    name: String,
    flavour: String,
    category: String,
    tags: String,
    article_count: String,
    media_count: String,
    author: String,
    publisher: String,
    links: Vec<OpdsLink>,
}

impl EntryBuilder {
    fn finish(self) -> Result<KiwixOpdsEntry, CatalogError> {
        require_provider_field("entry.id", &self.id)?;
        require_provider_field("entry.title", &self.title)?;
        let tags: Vec<_> = self
            .tags
            .split(';')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(ToString::to_string)
            .collect();
        if tags.len() > MAX_TAGS {
            return Err(CatalogError::Provider(format!(
                "OPDS entry exceeds {MAX_TAGS} tags"
            )));
        }
        Ok(KiwixOpdsEntry {
            id: self.id,
            title: self.title,
            updated: parse_date("entry.updated", &self.updated)?,
            issued: parse_date("entry.issued", &self.issued)?,
            summary: self.summary,
            language: optional_text(self.language),
            name: optional_text(self.name),
            flavour: optional_text(self.flavour),
            category: optional_text(self.category),
            tags,
            article_count: parse_number("entry.articleCount", &self.article_count)?,
            media_count: parse_number("entry.mediaCount", &self.media_count)?,
            author: optional_text(self.author),
            publisher: optional_text(self.publisher),
            links: self.links,
        })
    }
}

fn apply_text(
    path: &[String],
    text: &str,
    feed: &mut FeedBuilder,
    entry: Option<&mut EntryBuilder>,
) -> Result<(), CatalogError> {
    let Some(field) = path.last().map(String::as_str) else {
        return Ok(());
    };
    if let Some(entry) = entry {
        let parent = path.get(path.len().saturating_sub(2)).map(String::as_str);
        let target = match (parent, field) {
            (Some("author"), "name") => Some(&mut entry.author),
            (Some("publisher"), "name") => Some(&mut entry.publisher),
            (Some("entry"), "id") => Some(&mut entry.id),
            (Some("entry"), "title") => Some(&mut entry.title),
            (Some("entry"), "updated") => Some(&mut entry.updated),
            (Some("entry"), "issued") => Some(&mut entry.issued),
            (Some("entry"), "summary") => Some(&mut entry.summary),
            (Some("entry"), "language") => Some(&mut entry.language),
            (Some("entry"), "name") => Some(&mut entry.name),
            (Some("entry"), "flavour") => Some(&mut entry.flavour),
            (Some("entry"), "category") => Some(&mut entry.category),
            (Some("entry"), "tags") => Some(&mut entry.tags),
            (Some("entry"), "articleCount") => Some(&mut entry.article_count),
            (Some("entry"), "mediaCount") => Some(&mut entry.media_count),
            _ => None,
        };
        if let Some(target) = target {
            append_field(target, text, field)?;
        }
    } else {
        let parent = path.get(path.len().saturating_sub(2)).map(String::as_str);
        let target = match (parent, field) {
            (Some("feed"), "id") => Some(&mut feed.id),
            (Some("feed"), "title") => Some(&mut feed.title),
            (Some("feed"), "updated") => Some(&mut feed.updated),
            (Some("feed"), "totalResults") => Some(&mut feed.total_results),
            (Some("feed"), "startIndex") => Some(&mut feed.start_index),
            (Some("feed"), "itemsPerPage") => Some(&mut feed.items_per_page),
            _ => None,
        };
        if let Some(target) = target {
            append_field(target, text, field)?;
        }
    }
    Ok(())
}

fn apply_general_reference(
    path: &[String],
    reference: &BytesRef<'_>,
    feed: &mut FeedBuilder,
    entry: Option<&mut EntryBuilder>,
) -> Result<(), CatalogError> {
    let name = reference
        .decode()
        .map_err(|error| CatalogError::Provider(error.to_string()))?;
    if let Some(replacement) = resolve_xml_entity(name.as_ref()) {
        return apply_text(path, replacement, feed, entry);
    }

    let character = reference
        .resolve_char_ref()
        .map_err(|error| CatalogError::Provider(error.to_string()))?
        .ok_or_else(|| {
            CatalogError::Provider("OPDS custom general entities are forbidden".to_string())
        })?;
    if !is_legal_xml_10_character(character) {
        return Err(CatalogError::Provider(
            "OPDS character reference is not legal in XML 1.0".to_string(),
        ));
    }

    let mut encoded = [0_u8; 4];
    apply_text(path, character.encode_utf8(&mut encoded), feed, entry)
}

fn validate_xml_10_text(value: &str) -> Result<(), CatalogError> {
    if value.chars().all(is_legal_xml_10_character) {
        return Ok(());
    }
    Err(CatalogError::Provider(
        "OPDS text contains a character that is not legal in XML 1.0".to_string(),
    ))
}

fn is_legal_xml_10_character(character: char) -> bool {
    matches!(character, '\u{9}' | '\u{A}' | '\u{D}')
        || ('\u{20}'..='\u{D7FF}').contains(&character)
        || ('\u{E000}'..='\u{FFFD}').contains(&character)
        || ('\u{10000}'..='\u{10FFFF}').contains(&character)
}

fn append_field(target: &mut String, text: &str, field: &str) -> Result<(), CatalogError> {
    let new_len = target
        .len()
        .checked_add(text.len())
        .ok_or(CatalogError::IntegerOverflow)?;
    if new_len > MAX_FIELD_BYTES {
        return Err(CatalogError::Provider(format!(
            "OPDS field {field:?} exceeds {MAX_FIELD_BYTES} bytes"
        )));
    }
    target.push_str(text);
    Ok(())
}

fn parse_link(
    start: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
    source_uri: &Url,
) -> Result<OpdsLink, CatalogError> {
    let mut attributes = BTreeMap::new();
    let mut attribute_count = 0_usize;
    for attribute in start.attributes() {
        attribute_count = attribute_count
            .checked_add(1)
            .ok_or(CatalogError::IntegerOverflow)?;
        if attribute_count > MAX_ATTRIBUTES {
            return Err(CatalogError::Provider(format!(
                "OPDS link exceeds {MAX_ATTRIBUTES} attributes"
            )));
        }
        let attribute = attribute.map_err(|error| CatalogError::Provider(error.to_string()))?;
        let name = local_name(attribute.key.as_ref())?;
        let value = attribute
            .decoded_and_normalized_value_with(
                XmlVersion::Implicit1_0,
                reader.decoder(),
                1,
                resolve_xml_entity,
            )
            .map_err(|error| CatalogError::Provider(error.to_string()))?
            .into_owned();
        validate_xml_10_text(&value)?;
        if value.len() > MAX_FIELD_BYTES {
            return Err(CatalogError::Provider(format!(
                "OPDS link attribute {name:?} exceeds {MAX_FIELD_BYTES} bytes"
            )));
        }
        if attributes.insert(name.clone(), value).is_some() {
            return Err(CatalogError::Provider(format!(
                "OPDS link attribute {name:?} is duplicated"
            )));
        }
    }
    let href = attributes
        .remove("href")
        .ok_or_else(|| CatalogError::Provider("OPDS link has no href".to_string()))?;
    let href = resolve_provider_href(source_uri, &href)?;
    let length = attributes
        .remove("length")
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                CatalogError::Provider("OPDS link length is not an unsigned integer".to_string())
            })
        })
        .transpose()?;
    let sha256 = attributes
        .remove("hash")
        .or_else(|| attributes.remove("checksum"))
        .and_then(|value| normalize_sha256(&value));
    Ok(OpdsLink {
        rel: attributes.remove("rel").unwrap_or_default(),
        href,
        media_type: attributes.remove("type"),
        title: attributes.remove("title"),
        length,
        sha256,
    })
}

fn record_link(links_seen: &mut usize) -> Result<(), CatalogError> {
    *links_seen = links_seen
        .checked_add(1)
        .ok_or(CatalogError::IntegerOverflow)?;
    if *links_seen > MAX_LINKS {
        return Err(CatalogError::Provider(format!(
            "OPDS feed exceeds {MAX_LINKS} links"
        )));
    }
    Ok(())
}

fn normalize_sha256(value: &str) -> Option<String> {
    let value = value
        .strip_prefix("sha256:")
        .or_else(|| value.strip_prefix("sha-256:"))
        .unwrap_or(value);
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn local_name(qualified: &[u8]) -> Result<String, CatalogError> {
    let local = qualified
        .rsplit(|byte| *byte == b':')
        .next()
        .unwrap_or(qualified);
    std::str::from_utf8(local)
        .map(ToString::to_string)
        .map_err(|_| CatalogError::Provider("OPDS element name is not UTF-8".to_string()))
}

fn require_provider_field(field: &str, value: &str) -> Result<(), CatalogError> {
    if value.trim().is_empty() {
        return Err(CatalogError::Provider(format!(
            "required OPDS field {field} is empty"
        )));
    }
    Ok(())
}

fn optional_text(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn parse_date(field: &str, value: &str) -> Result<Option<DateTime<Utc>>, CatalogError> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(value.trim())
        .map(|date| Some(date.with_timezone(&Utc)))
        .map_err(|_| CatalogError::Provider(format!("OPDS {field} is not RFC 3339")))
}

fn parse_number(field: &str, value: &str) -> Result<Option<u64>, CatalogError> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .trim()
        .parse::<u64>()
        .map(Some)
        .map_err(|_| CatalogError::Provider(format!("OPDS {field} is not an unsigned integer")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIWIX_SAMPLE: &[u8] = include_bytes!("../tests/fixtures/kiwix-opds.xml");

    #[test]
    fn parses_live_kiwix_shape_without_inventing_a_digest() -> Result<(), CatalogError> {
        let source = Url::parse("https://opds.library.kiwix.org/catalog/v2/entries?count=1")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let feed = parse_kiwix_opds(KIWIX_SAMPLE, &source)?;
        assert_eq!(feed.entries.len(), 1);
        let entry = &feed.entries[0];
        assert_eq!(entry.name.as_deref(), Some("wikipedia_bg_all"));
        assert_eq!(entry.article_count, Some(452_996));
        let records = feed.discovery_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, ResourceKind::Encyclopedia);
        assert!(records[0].assets[0].requires_resolution);
        assert_eq!(records[0].assets[0].sha256, None);
        assert!(!records[0].has_exact_install_metadata());
        Ok(())
    }

    #[test]
    fn rejects_doctype_even_when_the_document_is_otherwise_small() -> Result<(), CatalogError> {
        let source = Url::parse("https://example.invalid/feed.xml")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let xml = br#"<?xml version="1.0"?><!DOCTYPE feed><feed><id>x</id><title>x</title></feed>"#;
        assert!(matches!(
            parse_kiwix_opds(xml, &source),
            Err(CatalogError::Provider(_))
        ));
        Ok(())
    }

    #[test]
    fn accepts_predefined_entities_and_legal_character_references() -> Result<(), CatalogError> {
        let source = Url::parse("https://example.invalid/feed.xml")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let xml = br#"<feed>
            <id>x</id>
            <title>Fish &amp; Chips &#38; &#x1F642;</title>
            <link rel="next" href="page?one=1&amp;two=2" title="A &quot;quoted&quot; title" />
        </feed>"#;

        let feed = parse_kiwix_opds(xml, &source)?;
        assert_eq!(feed.title, "Fish & Chips & 🙂");
        assert_eq!(
            feed.links[0].href,
            "https://example.invalid/page?one=1&two=2"
        );
        assert_eq!(feed.links[0].title.as_deref(), Some("A \"quoted\" title"));
        Ok(())
    }

    #[test]
    fn rejects_unknown_general_entities_without_a_doctype() -> Result<(), CatalogError> {
        let source = Url::parse("https://example.invalid/feed.xml")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let xml = b"<feed><id>x</id><title>x &kiwix;</title></feed>";
        assert!(matches!(
            parse_kiwix_opds(xml, &source),
            Err(CatalogError::Provider(message))
                if message.contains("custom general entities are forbidden")
        ));

        let attribute =
            b"<feed><id>x</id><title>x</title><link href=\"https://example.invalid/&kiwix;\" /></feed>";
        assert!(matches!(
            parse_kiwix_opds(attribute, &source),
            Err(CatalogError::Provider(_))
        ));
        Ok(())
    }

    #[test]
    fn rejects_illegal_xml_10_character_references() -> Result<(), CatalogError> {
        let source = Url::parse("https://example.invalid/feed.xml")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        for reference in ["&#1;", "&#xB;", "&#xFFFE;", "&#xD800;", "&#x110000;"] {
            let xml = format!("<feed><id>x</id><title>x{reference}</title></feed>");
            assert!(matches!(
                parse_kiwix_opds(xml.as_bytes(), &source),
                Err(CatalogError::Provider(_))
            ));
        }

        let attribute =
            b"<feed><id>x</id><title>x</title><link href=\"https://example.invalid/&#1;\" /></feed>";
        assert!(matches!(
            parse_kiwix_opds(attribute, &source),
            Err(CatalogError::Provider(_))
        ));
        Ok(())
    }

    #[test]
    fn enforces_explicit_byte_and_structural_value_limits() -> Result<(), CatalogError> {
        let source = Url::parse("https://example.invalid/feed.xml")
            .map_err(|error| CatalogError::Provider(error.to_string()))?;
        let xml = b"<feed><id>x</id><title>x</title></feed>";
        assert!(matches!(
            parse_kiwix_opds_with_limit(xml, &source, xml.len() - 1),
            Err(CatalogError::InputTooLarge { .. })
        ));

        let tags = std::iter::repeat_n("tag", MAX_TAGS + 1)
            .collect::<Vec<_>>()
            .join(";");
        let oversized = format!(
            "<feed><id>x</id><title>x</title><entry><id>e</id><title>e</title><tags>{tags}</tags></entry></feed>"
        );
        assert!(matches!(
            parse_kiwix_opds(oversized.as_bytes(), &source),
            Err(CatalogError::Provider(_))
        ));
        Ok(())
    }
}
