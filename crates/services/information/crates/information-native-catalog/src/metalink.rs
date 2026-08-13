use crate::{CatalogError, DiscoveryAsset};
use information_native_types::{ArtifactDescriptor, ArtifactId, ArtifactMirror, ArtifactRole};
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

pub const DEFAULT_MAX_METALINK_BYTES: usize = 16 * 1024 * 1024;
const MAX_FIELD_BYTES: usize = 256 * 1024;
const MAX_MIRRORS: usize = 256;
const MAX_XML_DEPTH: usize = 128;
const MAX_ATTRIBUTE_BYTES: usize = 16 * 1024;

/// The install-authoritative subset of a Metalink 4 document. Piece hashes and
/// weaker whole-file hashes are intentionally ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetalinkFile {
    pub file_name: String,
    pub expected_bytes: u64,
    pub sha256: String,
    pub mirrors: Vec<MetalinkMirror>,
}

impl MetalinkFile {
    #[must_use]
    pub fn to_discovery_asset(
        &self,
        key: impl Into<String>,
        media_type: Option<String>,
    ) -> DiscoveryAsset {
        DiscoveryAsset {
            key: key.into(),
            href: self
                .mirrors
                .first()
                .map(|mirror| mirror.uri.clone())
                .unwrap_or_default(),
            media_type,
            expected_bytes: Some(self.expected_bytes),
            sha256: Some(self.sha256.clone()),
            roles: vec!["data".to_string()],
            requires_resolution: false,
        }
    }

    pub fn to_artifact_descriptor(
        &self,
        id: ArtifactId,
        role: ArtifactRole,
        media_type: impl Into<String>,
    ) -> Result<ArtifactDescriptor, CatalogError> {
        let artifact = ArtifactDescriptor {
            id,
            role,
            file_name: self.file_name.clone(),
            media_type: media_type.into(),
            expected_bytes: self.expected_bytes,
            sha256: self.sha256.clone(),
            mirrors: self
                .mirrors
                .iter()
                .map(|mirror| ArtifactMirror {
                    uri: mirror.uri.clone(),
                    priority: mirror.priority,
                })
                .collect(),
        };
        artifact.validate()?;
        Ok(artifact)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetalinkMirror {
    pub uri: String,
    pub priority: u16,
    pub location: Option<String>,
}

pub fn parse_metalink4(bytes: &[u8]) -> Result<MetalinkFile, CatalogError> {
    parse_metalink4_with_limit(bytes, DEFAULT_MAX_METALINK_BYTES)
}

pub fn parse_metalink4_with_limit(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<MetalinkFile, CatalogError> {
    if bytes.len() > max_bytes {
        return Err(CatalogError::InputTooLarge {
            actual: bytes.len(),
            limit: max_bytes,
        });
    }
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = true;
    let mut path = Vec::<String>::new();
    let mut root_seen = false;
    let mut completed: Option<MetalinkFile> = None;
    let mut file: Option<FileBuilder> = None;
    let mut mirror: Option<MirrorBuilder> = None;
    let mut collect_sha256 = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = local_name(start.name().as_ref())?;
                if path.is_empty() {
                    if name != "metalink" || root_seen {
                        return Err(provider_error(
                            "Metalink root element must be exactly one metalink",
                        ));
                    }
                    root_seen = true;
                }
                let parent = path.last().map(String::as_str);
                match (parent, name.as_str()) {
                    (Some("metalink"), "file") => {
                        if file.is_some() || completed.is_some() {
                            return Err(provider_error("Metalink must contain exactly one file"));
                        }
                        file = Some(FileBuilder::new(parse_file_name(&start, &reader)?)?);
                    }
                    (Some("file"), "size") => {
                        file.as_mut()
                            .ok_or_else(|| provider_error("Metalink size is outside a file"))?
                            .begin_size()?;
                    }
                    (Some("file"), "hash") => {
                        let hash_type = attribute(&start, &reader, "type")?
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        collect_sha256 = matches!(hash_type.as_str(), "sha-256" | "sha256");
                        if collect_sha256 {
                            file.as_mut()
                                .ok_or_else(|| provider_error("Metalink hash is outside a file"))?
                                .begin_sha256()?;
                        }
                    }
                    (Some("file"), "url") => {
                        if mirror.is_some() {
                            return Err(provider_error("nested Metalink URLs are invalid"));
                        }
                        mirror = Some(MirrorBuilder::from_start(&start, &reader)?);
                    }
                    _ => {}
                }
                path.push(name);
                if path.len() > MAX_XML_DEPTH {
                    return Err(provider_error(&format!(
                        "Metalink XML nesting exceeds {MAX_XML_DEPTH} elements"
                    )));
                }
            }
            Ok(Event::Empty(start)) => {
                let name = local_name(start.name().as_ref())?;
                let parent = path.last().map(String::as_str);
                if parent == Some("metalink") && name == "file" {
                    return Err(provider_error(
                        "Metalink file has no size, hash, or mirrors",
                    ));
                }
                if parent == Some("file") && name == "url" {
                    return Err(provider_error("Metalink mirror URL is empty"));
                }
                if parent == Some("file") && matches!(name.as_str(), "size" | "hash") {
                    return Err(provider_error(
                        "Metalink file size and hash elements cannot be empty",
                    ));
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|error| CatalogError::Provider(error.to_string()))?;
                validate_xml_10_text(decoded.as_ref())?;
                apply_text(
                    &path,
                    decoded.as_ref(),
                    file.as_mut(),
                    mirror.as_mut(),
                    collect_sha256,
                )?;
            }
            Ok(Event::CData(text)) => {
                let decoded = text
                    .decode()
                    .map_err(|error| CatalogError::Provider(error.to_string()))?;
                validate_xml_10_text(decoded.as_ref())?;
                apply_text(
                    &path,
                    decoded.as_ref(),
                    file.as_mut(),
                    mirror.as_mut(),
                    collect_sha256,
                )?;
            }
            Ok(Event::End(end)) => {
                let name = local_name(end.name().as_ref())?;
                if path.last().is_none_or(|open| open != &name) {
                    return Err(provider_error("Metalink element nesting is invalid"));
                }
                let parent = path.get(path.len().saturating_sub(2)).map(String::as_str);
                if name == "url" && parent == Some("file") {
                    let finished = mirror
                        .take()
                        .ok_or_else(|| provider_error("Metalink URL end without start"))?
                        .finish()?;
                    let current = file
                        .as_mut()
                        .ok_or_else(|| provider_error("Metalink URL is outside a file"))?;
                    current.mirrors.push(finished);
                    if current.mirrors.len() > MAX_MIRRORS {
                        return Err(provider_error(&format!(
                            "Metalink exceeds {MAX_MIRRORS} mirrors"
                        )));
                    }
                }
                if name == "hash" && parent == Some("file") {
                    collect_sha256 = false;
                }
                if name == "file" && parent == Some("metalink") {
                    completed = Some(
                        file.take()
                            .ok_or_else(|| provider_error("Metalink file end without start"))?
                            .finish()?,
                    );
                }
                path.pop();
            }
            Ok(Event::GeneralRef(reference)) => {
                apply_general_reference(
                    &path,
                    &reference,
                    file.as_mut(),
                    mirror.as_mut(),
                    collect_sha256,
                )?;
            }
            Ok(Event::DocType(_)) => {
                return Err(provider_error("Metalink document types are forbidden"));
            }
            Ok(Event::Eof) => break,
            Ok(Event::Decl(_)) | Ok(Event::PI(_)) | Ok(Event::Comment(_)) => {}
            Err(error) => return Err(CatalogError::Provider(error.to_string())),
        }
    }

    if !root_seen || !path.is_empty() || file.is_some() || mirror.is_some() {
        return Err(provider_error("Metalink document is incomplete"));
    }
    completed.ok_or_else(|| provider_error("Metalink contains no file"))
}

#[derive(Debug)]
struct FileBuilder {
    file_name: String,
    size: Option<String>,
    sha256: Option<String>,
    mirrors: Vec<MetalinkMirror>,
}

impl FileBuilder {
    fn new(file_name: String) -> Result<Self, CatalogError> {
        validate_file_name(&file_name)?;
        Ok(Self {
            file_name,
            size: None,
            sha256: None,
            mirrors: Vec::new(),
        })
    }

    fn begin_size(&mut self) -> Result<(), CatalogError> {
        if self.size.is_some() {
            return Err(provider_error("Metalink contains duplicate file sizes"));
        }
        self.size = Some(String::new());
        Ok(())
    }

    fn begin_sha256(&mut self) -> Result<(), CatalogError> {
        if self.sha256.is_some() {
            return Err(provider_error("Metalink contains duplicate SHA-256 hashes"));
        }
        self.sha256 = Some(String::new());
        Ok(())
    }

    fn finish(mut self) -> Result<MetalinkFile, CatalogError> {
        let expected_bytes = self
            .size
            .take()
            .ok_or_else(|| provider_error("Metalink file size is missing"))?
            .trim()
            .parse::<u64>()
            .map_err(|_| provider_error("Metalink file size is missing or invalid"))?;
        if expected_bytes == 0 {
            return Err(provider_error(
                "Metalink file size must be greater than zero",
            ));
        }
        let sha256 = validate_sha256(
            &self
                .sha256
                .take()
                .ok_or_else(|| provider_error("Metalink file has no SHA-256 hash"))?,
        )?;
        if self.mirrors.is_empty() {
            return Err(provider_error("Metalink file has no HTTPS mirrors"));
        }
        let mut seen = BTreeSet::new();
        for mirror in &self.mirrors {
            if !seen.insert(&mirror.uri) {
                return Err(provider_error("Metalink contains a duplicate mirror URI"));
            }
        }
        self.mirrors
            .sort_by_key(|mirror| (mirror.priority, mirror.uri.clone()));
        Ok(MetalinkFile {
            file_name: self.file_name,
            expected_bytes,
            sha256,
            mirrors: self.mirrors,
        })
    }
}

#[derive(Debug)]
struct MirrorBuilder {
    uri: String,
    priority: u16,
    location: Option<String>,
}

impl MirrorBuilder {
    fn from_start(start: &BytesStart<'_>, reader: &Reader<&[u8]>) -> Result<Self, CatalogError> {
        let priority = attribute(start, reader, "priority")?
            .map(|value| {
                value
                    .parse::<u16>()
                    .map_err(|_| provider_error("Metalink mirror priority is invalid"))
            })
            .transpose()?
            .unwrap_or(u16::MAX);
        Ok(Self {
            uri: String::new(),
            priority,
            location: attribute(start, reader, "location")?,
        })
    }

    fn finish(self) -> Result<MetalinkMirror, CatalogError> {
        let uri = self.uri.trim().to_string();
        let parsed = Url::parse(&uri)
            .map_err(|_| provider_error("Metalink mirror is not an absolute URI"))?;
        if parsed.scheme() != "https" {
            return Err(provider_error("Metalink mirrors must use HTTPS"));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(provider_error(
                "Metalink mirrors cannot contain embedded credentials",
            ));
        }
        if parsed.fragment().is_some() {
            return Err(provider_error("Metalink mirrors cannot contain fragments"));
        }
        Ok(MetalinkMirror {
            uri: parsed.to_string(),
            priority: self.priority,
            location: self.location,
        })
    }
}

fn apply_text(
    path: &[String],
    text: &str,
    file: Option<&mut FileBuilder>,
    mirror: Option<&mut MirrorBuilder>,
    collect_sha256: bool,
) -> Result<(), CatalogError> {
    let current = path.last().map(String::as_str);
    let parent = path.get(path.len().saturating_sub(2)).map(String::as_str);
    if current == Some("url") && parent == Some("file") {
        let mirror = mirror.ok_or_else(|| provider_error("Metalink URL text without start"))?;
        append_field(&mut mirror.uri, text, "url")?;
    } else if current == Some("size") && parent == Some("file") {
        let file = file.ok_or_else(|| provider_error("Metalink size is outside a file"))?;
        let size = file
            .size
            .as_mut()
            .ok_or_else(|| provider_error("Metalink size text without start"))?;
        append_field(size, text, "size")?;
    } else if current == Some("hash") && parent == Some("file") && collect_sha256 {
        let file = file.ok_or_else(|| provider_error("Metalink hash is outside a file"))?;
        let digest = file
            .sha256
            .as_mut()
            .ok_or_else(|| provider_error("Metalink SHA-256 text without start"))?;
        append_field(digest, text, "sha-256")?;
    }
    Ok(())
}

fn apply_general_reference(
    path: &[String],
    reference: &BytesRef<'_>,
    file: Option<&mut FileBuilder>,
    mirror: Option<&mut MirrorBuilder>,
    collect_sha256: bool,
) -> Result<(), CatalogError> {
    let name = reference
        .decode()
        .map_err(|error| CatalogError::Provider(error.to_string()))?;
    if let Some(replacement) = resolve_xml_entity(name.as_ref()) {
        return apply_text(path, replacement, file, mirror, collect_sha256);
    }
    let character = reference
        .resolve_char_ref()
        .map_err(|error| CatalogError::Provider(error.to_string()))?
        .ok_or_else(|| provider_error("Metalink custom general entities are forbidden"))?;
    if !is_legal_xml_10_character(character) {
        return Err(provider_error(
            "Metalink character reference is not legal in XML 1.0",
        ));
    }
    let mut encoded = [0_u8; 4];
    apply_text(
        path,
        character.encode_utf8(&mut encoded),
        file,
        mirror,
        collect_sha256,
    )
}

fn validate_xml_10_text(value: &str) -> Result<(), CatalogError> {
    if value.chars().all(is_legal_xml_10_character) {
        return Ok(());
    }
    Err(provider_error(
        "Metalink text contains a character that is not legal in XML 1.0",
    ))
}

fn is_legal_xml_10_character(character: char) -> bool {
    matches!(character, '\u{9}' | '\u{A}' | '\u{D}')
        || ('\u{20}'..='\u{D7FF}').contains(&character)
        || ('\u{E000}'..='\u{FFFD}').contains(&character)
        || ('\u{10000}'..='\u{10FFFF}').contains(&character)
}

fn parse_file_name(start: &BytesStart<'_>, reader: &Reader<&[u8]>) -> Result<String, CatalogError> {
    attribute(start, reader, "name")?.ok_or_else(|| provider_error("Metalink file has no name"))
}

fn attribute(
    start: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
    requested: &str,
) -> Result<Option<String>, CatalogError> {
    let mut found = None;
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|error| CatalogError::Provider(error.to_string()))?;
        if local_name(attribute.key.as_ref())? == requested {
            if found.is_some() {
                return Err(provider_error("Metalink attribute is duplicated"));
            }
            found = Some(bounded_attribute(
                attribute
                    .decoded_and_normalized_value_with(
                        XmlVersion::Implicit1_0,
                        reader.decoder(),
                        1,
                        resolve_xml_entity,
                    )
                    .map_err(|error| CatalogError::Provider(error.to_string()))?
                    .into_owned(),
                requested,
            )?);
        }
    }
    Ok(found)
}

fn validate_file_name(value: &str) -> Result<(), CatalogError> {
    let stem = value.split('.').next().unwrap_or(value);
    let windows_reserved = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if value.is_empty()
        || value.len() > 255
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.ends_with([' ', '.'])
        || value
            .chars()
            .any(|character| character.is_control() || r#"<>:"|?*"#.contains(character))
        || windows_reserved
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Err(provider_error("Metalink file name is not portable"));
    }
    Ok(())
}

fn bounded_attribute(value: String, field: &str) -> Result<String, CatalogError> {
    validate_xml_10_text(&value)?;
    if value.len() > MAX_ATTRIBUTE_BYTES {
        return Err(provider_error(&format!(
            "Metalink attribute {field:?} exceeds {MAX_ATTRIBUTE_BYTES} bytes"
        )));
    }
    Ok(value)
}

fn validate_sha256(value: &str) -> Result<String, CatalogError> {
    let value = value.trim();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(provider_error("Metalink SHA-256 hash is invalid"));
    }
    Ok(value.to_ascii_lowercase())
}

fn append_field(target: &mut String, text: &str, field: &str) -> Result<(), CatalogError> {
    let new_len = target
        .len()
        .checked_add(text.len())
        .ok_or(CatalogError::IntegerOverflow)?;
    if new_len > MAX_FIELD_BYTES {
        return Err(provider_error(&format!(
            "Metalink field {field:?} exceeds {MAX_FIELD_BYTES} bytes"
        )));
    }
    target.push_str(text);
    Ok(())
}

fn local_name(qualified: &[u8]) -> Result<String, CatalogError> {
    let local = qualified
        .rsplit(|byte| *byte == b':')
        .next()
        .unwrap_or(qualified);
    std::str::from_utf8(local)
        .map(ToString::to_string)
        .map_err(|_| provider_error("Metalink element name is not UTF-8"))
}

fn provider_error(message: &str) -> CatalogError {
    CatalogError::Provider(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIWIX_METALINK: &[u8] = include_bytes!("../tests/fixtures/kiwix-zim.meta4");

    #[test]
    fn parses_exact_payload_and_orders_https_mirrors() -> Result<(), CatalogError> {
        let parsed = parse_metalink4(KIWIX_METALINK)?;
        assert_eq!(parsed.file_name, "wikipedia_bg_all_maxi_2026-07.zim");
        assert_eq!(parsed.expected_bytes, 5_121_103_348);
        assert_eq!(
            parsed.sha256,
            "e033d74dbadca167a75c0c9415e283d40e28e1cc698221c32334a67206de9f9d"
        );
        assert_eq!(parsed.mirrors.len(), 2);
        assert_eq!(parsed.mirrors[0].priority, 1);
        let artifact = parsed.to_artifact_descriptor(
            ArtifactId::parse("zim")?,
            ArtifactRole::Primary,
            "application/x-zim",
        )?;
        artifact.validate()?;
        Ok(())
    }

    #[test]
    fn rejects_traversal_names_missing_hashes_and_insecure_mirrors() {
        let traversal = valid_document().replace("payload.zim", "../payload.zim");
        assert!(parse_metalink4(traversal.as_bytes()).is_err());

        let missing_hash = valid_document().replace(
            "<hash type=\"sha-256\">aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</hash>",
            "",
        );
        assert!(parse_metalink4(missing_hash.as_bytes()).is_err());

        let insecure = valid_document().replace("https://", "http://");
        assert!(parse_metalink4(insecure.as_bytes()).is_err());
    }

    #[test]
    fn rejects_non_portable_file_names() {
        for file_name in [
            "CON.zim",
            "payload.zim.",
            "payload.zim ",
            "payload:alternate.zim",
            "payload?.zim",
            "payload\u{7f}.zim",
        ] {
            let document = valid_document().replace("payload.zim", file_name);
            assert!(
                parse_metalink4(document.as_bytes()).is_err(),
                "accepted non-portable file name {file_name:?}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_file_elements() {
        let duplicate = valid_document().replace(
            "</metalink>",
            "<file name=\"other.zim\"><size>1</size><hash type=\"sha-256\">bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb</hash><url>https://example.invalid/other.zim</url></file></metalink>",
        );
        assert!(parse_metalink4(duplicate.as_bytes()).is_err());

        let duplicate_size =
            valid_document().replace("<size>7</size>", "<size>7</size><size>7</size>");
        assert!(parse_metalink4(duplicate_size.as_bytes()).is_err());
    }

    #[test]
    fn enforces_explicit_input_limit_before_xml_parsing() {
        let document = valid_document();
        assert!(matches!(
            parse_metalink4_with_limit(document.as_bytes(), document.len() - 1),
            Err(CatalogError::InputTooLarge { .. })
        ));
    }

    #[test]
    fn resolves_only_safe_xml_references() -> Result<(), CatalogError> {
        let escaped = valid_document()
            .replace("payload.zim\"", "payload&#x2E;zim\"")
            .replace(
                "https://example.invalid/payload.zim",
                "https://example.invalid/payload.zim?a=1&amp;b=2",
            );
        let parsed = parse_metalink4(escaped.as_bytes())?;
        assert_eq!(parsed.file_name, "payload.zim");
        assert_eq!(
            parsed.mirrors[0].uri,
            "https://example.invalid/payload.zim?a=1&b=2"
        );

        let unknown = valid_document().replace("payload.zim</url>", "&custom;</url>");
        assert!(parse_metalink4(unknown.as_bytes()).is_err());
        let illegal = valid_document().replace("payload.zim</url>", "payload.zim&#1;</url>");
        assert!(parse_metalink4(illegal.as_bytes()).is_err());
        let doctype = valid_document().replacen(
            "<metalink>",
            "<!DOCTYPE metalink [<!ENTITY custom 'x'>]><metalink>",
            1,
        );
        assert!(parse_metalink4(doctype.as_bytes()).is_err());
        Ok(())
    }

    fn valid_document() -> String {
        "<metalink><file name=\"payload.zim\"><size>7</size><hash type=\"sha-256\">aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</hash><url priority=\"1\">https://example.invalid/payload.zim</url></file></metalink>".to_string()
    }
}
