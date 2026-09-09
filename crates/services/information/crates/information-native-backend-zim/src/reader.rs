use crate::text::extract_inert_text;
use crate::{
    ProducedManagedDocuments, ZIM_PRODUCER_VERSION, ZimArchiveIdentity, ZimError, ZimLimits,
    ZimMaterializationRequest, ZimOmissions, ZimProductionReport, ZimTextKind,
};
use information_native_types::{
    EvidenceLocator, MANAGED_DOCUMENTS_SCHEMA, ManagedDocument, ManagedDocumentId,
    ManagedDocumentLineage, ManagedDocumentVisibility, ManagedDocumentsV1, ManagedSegmentId,
    ManagedSourceArtifact, ManagedTextSegment, Provenance,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::ops::Range;

const ZIM_MAGIC: u32 = 0x044d_495a;
const HEADER_BYTES: usize = 80;
const HEADER_BYTES_U64: u64 = 80;
const CHECKSUM_BYTES: u64 = 16;
const REDIRECT_MIME: u16 = 0xffff;
const LINKTARGET_MIME: u16 = 0xfffe;
const DELETED_MIME: u16 = 0xfffd;

pub fn produce_managed_documents(
    request: &ZimMaterializationRequest,
) -> Result<ProducedManagedDocuments, ZimError> {
    request.limits.validate()?;
    validate_request(request)?;
    let mut archive = Archive::open(request)?;
    let parsed = archive.parse(request.limits)?;
    let mut omissions = ZimOmissions::default();
    let candidates = collect_candidates(&parsed, request.limits, &mut omissions)?;
    let selected =
        archive.read_selected_blobs(&parsed, &candidates, request.limits, &mut omissions)?;
    let (documents, materialized_text_bytes, truncated_article_count) = build_documents(
        request,
        &parsed,
        &candidates,
        &selected.blobs,
        &mut omissions,
    )?;
    if documents.is_empty() {
        return Err(ZimError::NoMaterializableArticles);
    }

    archive.verify_unchanged()?;
    let archive_identity = ZimArchiveIdentity {
        bytes: archive.len,
        sha256: archive.sha256.clone(),
        uuid_hex: parsed.header.uuid_hex.clone(),
        major_version: parsed.header.major_version,
        minor_version: parsed.header.minor_version,
        entry_count: parsed.header.entry_count,
        cluster_count: parsed.header.cluster_count,
    };
    let mut metadata = BTreeMap::new();
    metadata.insert("archive_bytes".to_string(), json!(archive_identity.bytes));
    metadata.insert("archive_sha256".to_string(), json!(archive_identity.sha256));
    metadata.insert("archive_uuid".to_string(), json!(archive_identity.uuid_hex));
    metadata.insert(
        "openzim_version".to_string(),
        json!({
            "major": archive_identity.major_version,
            "minor": archive_identity.minor_version,
        }),
    );

    let mut materialization = ManagedDocumentsV1 {
        schema: MANAGED_DOCUMENTS_SCHEMA.to_string(),
        materialization_id: request.materialization_id.clone(),
        resource_id: request.resource_id.clone(),
        release_id: request.release_id.clone(),
        representation_id: request.representation_id.clone(),
        created_at: request.created_at,
        provenance: Provenance {
            publisher: request.publisher.clone(),
            source_uri: request.source_uri.clone(),
            upstream_record_id: Some(parsed.header.uuid_hex.clone()),
            source_inputs: vec![format!("sha256:{}", archive.sha256)],
            transformation: Some(ZIM_PRODUCER_VERSION.to_string()),
            metadata,
        },
        source_artifacts: vec![ManagedSourceArtifact {
            artifact_id: request.source_artifact_id.clone(),
            source_uri: request.source_uri.clone(),
            bytes: archive.len,
            sha256: archive.sha256.clone(),
            immutable: true,
        }],
        documents,
        content_sha256: "0".repeat(64),
    };
    materialization.refresh_content_sha256()?;
    materialization.validate()?;

    let materialized_document_count =
        u64::try_from(materialization.documents.len()).map_err(|_| ZimError::IntegerOverflow)?;
    Ok(ProducedManagedDocuments {
        documents: materialization,
        report: ZimProductionReport {
            archive: archive_identity,
            validated_directory_entry_count: parsed.header.entry_count,
            materialized_document_count,
            materialized_text_bytes,
            decoded_cluster_count: selected.decoded_cluster_count,
            codecs_used: selected.codecs_used,
            truncated_article_count,
            omissions,
        },
    })
}

fn validate_request(request: &ZimMaterializationRequest) -> Result<(), ZimError> {
    if request.expected_archive_bytes == 0 {
        return Err(ZimError::InvalidRequest("expected archive size is zero"));
    }
    if request.publisher.trim().is_empty() || request.publisher.len() > 8_192 {
        return Err(ZimError::InvalidRequest("publisher is empty or too long"));
    }
    if request.source_uri.len() > 16 * 1024 {
        return Err(ZimError::InvalidRequest("source URI is too long"));
    }
    if request.rights.is_empty() || request.rights.len() > 32 {
        return Err(ZimError::InvalidRequest(
            "rights must contain between 1 and 32 statements",
        ));
    }
    request.use_policy.validate_with_rights(&request.rights)?;
    Ok(())
}

struct Archive {
    file: File,
    len: u64,
    sha256: String,
}

impl Archive {
    fn open(request: &ZimMaterializationRequest) -> Result<Self, ZimError> {
        if is_split_archive_name(&request.archive_path) {
            return Err(ZimError::SplitArchiveUnsupported);
        }
        let path_metadata = fs::symlink_metadata(&request.archive_path)
            .map_err(|source| ZimError::io("reading archive metadata", source))?;
        if path_metadata.file_type().is_symlink() {
            return Err(ZimError::SymlinkArchive);
        }
        let mut file = File::open(&request.archive_path)
            .map_err(|source| ZimError::io("opening archive read-only", source))?;
        let metadata = file
            .metadata()
            .map_err(|source| ZimError::io("reading opened archive metadata", source))?;
        if !metadata.is_file() {
            return Err(ZimError::NotARegularFile);
        }
        let len = metadata.len();
        if len > request.limits.max_archive_bytes {
            return Err(ZimError::ArchiveLimitExceeded);
        }
        if len != request.expected_archive_bytes {
            return Err(ZimError::ArchiveSizeMismatch {
                expected: request.expected_archive_bytes,
                actual: len,
            });
        }
        let expected_sha256 = normalize_sha256(&request.expected_archive_sha256)?;
        let sha256 = hash_file(&mut file)?;
        if sha256 != expected_sha256 {
            return Err(ZimError::ArchiveHashMismatch);
        }
        Ok(Self { file, len, sha256 })
    }

    fn parse(&mut self, limits: ZimLimits) -> Result<ParsedArchive, ZimError> {
        if self.len < HEADER_BYTES_U64 {
            return Err(ZimError::Truncated("header"));
        }
        let header_bytes = read_range(&mut self.file, 0, HEADER_BYTES, "header")?;
        let header = Header::parse(&header_bytes, self.len, limits)?;
        let url_table = table_range(
            header.url_pointer_position,
            header.entry_count,
            8,
            self.len,
            "URL pointer table",
        )?;
        let cluster_table = table_range(
            header.cluster_pointer_position,
            header.cluster_count,
            8,
            self.len,
            "cluster pointer table",
        )?;
        let title_table = if header.title_index_position == u64::MAX {
            None
        } else {
            Some(table_range(
                header.title_index_position,
                header.entry_count,
                4,
                self.len,
                "title index table",
            )?)
        };
        let checksum_range = header.checksum_position..self.len;
        let mut fixed_regions = Vec::with_capacity(5);
        fixed_regions.push(0..HEADER_BYTES_U64);
        fixed_regions.push(url_table.clone());
        fixed_regions.push(cluster_table.clone());
        if let Some(title_table) = &title_table {
            fixed_regions.push(title_table.clone());
        }
        fixed_regions.push(checksum_range.clone());
        validate_disjoint_regions(&fixed_regions)?;

        let entry_offsets = read_u64_table(
            &mut self.file,
            &url_table,
            header.entry_count,
            "URL pointer table",
        )?;
        let cluster_offsets = read_u64_table(
            &mut self.file,
            &cluster_table,
            header.cluster_count,
            "cluster pointer table",
        )?;
        validate_strict_offsets(&entry_offsets, "directory entry offsets")?;
        validate_strict_offsets(&cluster_offsets, "cluster offsets")?;
        validate_content_anchors(
            &entry_offsets,
            &cluster_offsets,
            &fixed_regions,
            header.checksum_position,
        )?;

        let mime_upper = entry_offsets
            .first()
            .into_iter()
            .chain(cluster_offsets.first())
            .chain(fixed_regions.iter().map(|range| &range.start))
            .copied()
            .filter(|offset| *offset > header.mime_list_position)
            .min()
            .ok_or(ZimError::InvalidMimeList)?;
        let (mime_types, mime_range) = read_mime_list(
            &mut self.file,
            header.mime_list_position,
            mime_upper,
            limits.max_mime_list_bytes,
        )?;
        fixed_regions.push(mime_range);
        validate_disjoint_regions(&fixed_regions)?;
        validate_content_anchors(
            &entry_offsets,
            &cluster_offsets,
            &fixed_regions,
            header.checksum_position,
        )?;

        let anchors = build_anchors(
            &entry_offsets,
            &cluster_offsets,
            &fixed_regions,
            header.checksum_position,
        );
        let mut entries: Vec<DirectoryEntry> = Vec::with_capacity(entry_offsets.len());
        let mut total_dirent_bytes = 0_usize;
        for (index, offset) in entry_offsets.iter().copied().enumerate() {
            let entry_index = u32::try_from(index).map_err(|_| ZimError::IntegerOverflow)?;
            let upper = next_anchor(&anchors, offset)?;
            let (entry, encoded_bytes) = read_directory_entry(
                &mut self.file,
                offset,
                upper,
                entry_index,
                &mime_types,
                header.entry_count,
                header.cluster_count,
                limits,
            )?;
            total_dirent_bytes = total_dirent_bytes
                .checked_add(encoded_bytes)
                .ok_or(ZimError::IntegerOverflow)?;
            if total_dirent_bytes > limits.max_total_dirent_bytes {
                return Err(ZimError::CountLimit("directory entry bytes"));
            }
            if let Some(previous) = entries.last() {
                let previous_key = (previous.namespace, previous.path.as_str());
                let current_key = (entry.namespace, entry.path.as_str());
                if previous_key >= current_key {
                    return Err(ZimError::InvalidDirectoryEntry {
                        entry_index,
                        reason: "URL pointer table is not in strict namespace/path order",
                    });
                }
            }
            entries.push(entry);
        }

        if let (Some(title_range), Some(_)) = (title_table.as_ref(), entries.first()) {
            let title_indices = read_u32_table(
                &mut self.file,
                title_range,
                header.entry_count,
                "title index table",
            )?;
            validate_title_index(&title_indices, &entries)?;
        }

        Ok(ParsedArchive {
            header,
            mime_types,
            entries,
            cluster_offsets,
            anchors,
        })
    }

    fn read_selected_blobs(
        &mut self,
        parsed: &ParsedArchive,
        candidates: &[Candidate],
        limits: ZimLimits,
        omissions: &mut ZimOmissions,
    ) -> Result<SelectedBlobs, ZimError> {
        let mut by_cluster: BTreeMap<u32, Vec<&Candidate>> = BTreeMap::new();
        for candidate in candidates {
            by_cluster
                .entry(candidate.cluster_index)
                .or_default()
                .push(candidate);
        }
        let mut selected = HashMap::new();
        let mut total_decoded = 0_usize;
        let mut total_blob_bytes = 0_usize;
        let mut decoded_cluster_count = 0_u64;
        let mut codecs = BTreeSet::new();
        let mut decode_budget_exhausted = false;

        for (cluster_index, cluster_candidates) in by_cluster {
            if decode_budget_exhausted {
                let omitted = u64::try_from(cluster_candidates.len())
                    .map_err(|_| ZimError::IntegerOverflow)?;
                omissions.document_limit = omissions.document_limit.saturating_add(omitted);
                continue;
            }
            let offset_index =
                usize::try_from(cluster_index).map_err(|_| ZimError::IntegerOverflow)?;
            let offset =
                *parsed
                    .cluster_offsets
                    .get(offset_index)
                    .ok_or(ZimError::InvalidCluster {
                        cluster_index,
                        reason: "cluster index is outside the pointer table",
                    })?;
            let upper = next_anchor(&parsed.anchors, offset)?;
            let cluster = read_cluster(&mut self.file, offset, upper, cluster_index, limits)?;
            decoded_cluster_count = decoded_cluster_count.saturating_add(1);
            codecs.insert(cluster.codec.to_string());
            let Some(next_total_decoded) = total_decoded.checked_add(cluster.decoded.len()) else {
                return Err(ZimError::IntegerOverflow);
            };
            if next_total_decoded > limits.max_total_decoded_cluster_bytes {
                decode_budget_exhausted = true;
                let omitted = u64::try_from(cluster_candidates.len())
                    .map_err(|_| ZimError::IntegerOverflow)?;
                omissions.document_limit = omissions.document_limit.saturating_add(omitted);
                continue;
            }
            total_decoded = next_total_decoded;

            for candidate in cluster_candidates {
                let blob = match cluster.blob(
                    candidate.blob_index,
                    cluster_index,
                    limits.max_blob_bytes,
                ) {
                    Ok(blob) => blob,
                    Err(ZimError::ArticleLimitExceeded) => {
                        omissions.invalid_or_empty_text =
                            omissions.invalid_or_empty_text.saturating_add(1);
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let Some(next_total_blob_bytes) = total_blob_bytes.checked_add(blob.len()) else {
                    return Err(ZimError::IntegerOverflow);
                };
                if next_total_blob_bytes > limits.max_total_selected_blob_bytes {
                    omissions.document_limit = omissions.document_limit.saturating_add(1);
                    continue;
                }
                total_blob_bytes = next_total_blob_bytes;
                selected.insert(candidate.entry_index, blob.to_vec());
            }
        }

        Ok(SelectedBlobs {
            blobs: selected,
            decoded_cluster_count,
            codecs_used: codecs.into_iter().collect(),
        })
    }

    fn verify_unchanged(&mut self) -> Result<(), ZimError> {
        let metadata = self
            .file
            .metadata()
            .map_err(|source| ZimError::io("rechecking archive metadata", source))?;
        if metadata.len() != self.len || hash_file(&mut self.file)? != self.sha256 {
            return Err(ZimError::ArchiveChanged);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Header {
    major_version: u16,
    minor_version: u16,
    uuid_hex: String,
    entry_count: u32,
    cluster_count: u32,
    url_pointer_position: u64,
    title_index_position: u64,
    cluster_pointer_position: u64,
    mime_list_position: u64,
    checksum_position: u64,
}

impl Header {
    fn parse(bytes: &[u8], archive_len: u64, limits: ZimLimits) -> Result<Self, ZimError> {
        if read_u32(bytes, 0)? != ZIM_MAGIC {
            return Err(ZimError::InvalidMagic);
        }
        let major_version = read_u16(bytes, 4)?;
        if major_version != 6 {
            return Err(ZimError::UnsupportedVersion(major_version));
        }
        let minor_version = read_u16(bytes, 6)?;
        let uuid = bytes.get(8..24).ok_or(ZimError::Truncated("UUID"))?;
        let entry_count = read_u32(bytes, 24)?;
        let cluster_count = read_u32(bytes, 28)?;
        let url_pointer_position = read_u64(bytes, 32)?;
        let title_index_position = read_u64(bytes, 40)?;
        let cluster_pointer_position = read_u64(bytes, 48)?;
        let mime_list_position = read_u64(bytes, 56)?;
        let main_page = read_u32(bytes, 64)?;
        let layout_page = read_u32(bytes, 68)?;
        let checksum_position = read_u64(bytes, 72)?;

        if entry_count == 0 || cluster_count == 0 || cluster_count > entry_count {
            return Err(ZimError::InvalidHeader("invalid entry/cluster counts"));
        }
        if entry_count > limits.max_entries {
            return Err(ZimError::CountLimit("entries"));
        }
        if cluster_count > limits.max_clusters {
            return Err(ZimError::CountLimit("clusters"));
        }
        if mime_list_position != HEADER_BYTES_U64 {
            return Err(ZimError::InvalidHeader(
                "version 6 MIME list must begin at byte 80",
            ));
        }
        if url_pointer_position < mime_list_position
            || cluster_pointer_position < mime_list_position
            || (title_index_position != u64::MAX && title_index_position < mime_list_position)
        {
            return Err(ZimError::InvalidHeader(
                "pointer tables precede the MIME list",
            ));
        }
        if checksum_position < mime_list_position
            || checksum_position
                .checked_add(CHECKSUM_BYTES)
                .ok_or(ZimError::IntegerOverflow)?
                != archive_len
        {
            return Err(ZimError::InvalidHeader(
                "checksum must be the final 16 archive bytes",
            ));
        }
        for page in [main_page, layout_page] {
            if page != u32::MAX && page >= entry_count {
                return Err(ZimError::InvalidHeader("page index is out of bounds"));
            }
        }
        Ok(Self {
            major_version,
            minor_version,
            uuid_hex: hex::encode(uuid),
            entry_count,
            cluster_count,
            url_pointer_position,
            title_index_position,
            cluster_pointer_position,
            mime_list_position,
            checksum_position,
        })
    }
}

struct ParsedArchive {
    header: Header,
    mime_types: Vec<String>,
    entries: Vec<DirectoryEntry>,
    cluster_offsets: Vec<u64>,
    anchors: Vec<u64>,
}

#[derive(Debug)]
struct DirectoryEntry {
    namespace: u8,
    path: String,
    title: String,
    kind: DirectoryEntryKind,
}

#[derive(Debug)]
enum DirectoryEntryKind {
    Item {
        mime_index: u16,
        cluster_index: u32,
        blob_index: u32,
    },
    Redirect,
    LinktargetOrDeleted,
}

#[derive(Debug)]
struct Candidate {
    entry_index: u32,
    cluster_index: u32,
    blob_index: u32,
    text_kind: ZimTextKind,
}

struct SelectedBlobs {
    blobs: HashMap<u32, Vec<u8>>,
    decoded_cluster_count: u64,
    codecs_used: Vec<String>,
}

fn collect_candidates(
    parsed: &ParsedArchive,
    limits: ZimLimits,
    omissions: &mut ZimOmissions,
) -> Result<Vec<Candidate>, ZimError> {
    let mut candidates = Vec::new();
    let mut clusters = BTreeSet::new();
    for (entry_index, entry) in parsed.entries.iter().enumerate() {
        let entry_index = u32::try_from(entry_index).map_err(|_| ZimError::IntegerOverflow)?;
        let (mime_index, cluster_index, blob_index) = match entry.kind {
            DirectoryEntryKind::Redirect => {
                omissions.redirects = omissions.redirects.saturating_add(1);
                continue;
            }
            DirectoryEntryKind::LinktargetOrDeleted => {
                omissions.linktargets_or_deleted =
                    omissions.linktargets_or_deleted.saturating_add(1);
                continue;
            }
            DirectoryEntryKind::Item {
                mime_index,
                cluster_index,
                blob_index,
            } => (mime_index, cluster_index, blob_index),
        };
        if !matches!(entry.namespace, b'A' | b'C') {
            omissions.non_article_namespaces = omissions.non_article_namespaces.saturating_add(1);
            continue;
        }
        let mime = parsed.mime_types.get(usize::from(mime_index)).ok_or(
            ZimError::InvalidDirectoryEntry {
                entry_index,
                reason: "MIME index is outside the MIME list",
            },
        )?;
        let Some(text_kind) = classify_text_mime(mime) else {
            omissions.non_text_mime_types = omissions.non_text_mime_types.saturating_add(1);
            continue;
        };
        if candidates.len() >= limits.max_candidate_entries {
            omissions.document_limit = omissions.document_limit.saturating_add(1);
            continue;
        }
        if !clusters.contains(&cluster_index) && clusters.len() >= limits.max_decoded_clusters {
            omissions.document_limit = omissions.document_limit.saturating_add(1);
            continue;
        }
        clusters.insert(cluster_index);
        candidates.push(Candidate {
            entry_index,
            cluster_index,
            blob_index,
            text_kind,
        });
    }
    Ok(candidates)
}

fn build_documents(
    request: &ZimMaterializationRequest,
    parsed: &ParsedArchive,
    candidates: &[Candidate],
    selected_blobs: &HashMap<u32, Vec<u8>>,
    omissions: &mut ZimOmissions,
) -> Result<(Vec<ManagedDocument>, u64, u64), ZimError> {
    let mut documents = Vec::new();
    let mut total_text_bytes = 0_usize;
    let mut truncated_article_count = 0_u64;

    for candidate in candidates {
        let Some(blob) = selected_blobs.get(&candidate.entry_index) else {
            continue;
        };
        if documents.len() >= request.limits.max_documents
            || total_text_bytes >= request.limits.max_total_text_bytes
        {
            omissions.document_limit = omissions.document_limit.saturating_add(1);
            continue;
        }
        let remaining = request
            .limits
            .max_total_text_bytes
            .checked_sub(total_text_bytes)
            .ok_or(ZimError::IntegerOverflow)?;
        let output_limit = request.limits.max_article_text_bytes.min(remaining);
        let extracted = match extract_inert_text(blob, candidate.text_kind, output_limit) {
            Ok(extracted) => extracted,
            Err(ZimError::InvalidArticleUtf8 | ZimError::MalformedHtml) => {
                omissions.invalid_or_empty_text = omissions.invalid_or_empty_text.saturating_add(1);
                continue;
            }
            Err(error) => return Err(error),
        };
        if extracted.text.trim().is_empty() {
            omissions.invalid_or_empty_text = omissions.invalid_or_empty_text.saturating_add(1);
            continue;
        }
        if extracted.truncated {
            truncated_article_count = truncated_article_count.saturating_add(1);
        }
        total_text_bytes = total_text_bytes
            .checked_add(extracted.text.len())
            .ok_or(ZimError::IntegerOverflow)?;

        let entry = parsed
            .entries
            .get(usize::try_from(candidate.entry_index).map_err(|_| ZimError::IntegerOverflow)?)
            .ok_or(ZimError::InvalidDirectoryEntry {
                entry_index: candidate.entry_index,
                reason: "entry index is outside the parsed directory",
            })?;
        let internal_path = format!("{}/{}", char::from(entry.namespace), entry.path);
        let locator = EvidenceLocator::ZimArticle {
            archive_uuid: Some(parsed.header.uuid_hex.clone()),
            internal_path: internal_path.clone(),
            entry_index: Some(candidate.entry_index),
        };
        let raw_sha256 = format!("{:x}", Sha256::digest(blob));
        let identity_digest = format!(
            "{:x}",
            Sha256::digest(format!("{}\0{}", candidate.entry_index, internal_path).as_bytes())
        );
        let short_digest = identity_digest.get(..24).ok_or(ZimError::IntegerOverflow)?;
        let document_id =
            ManagedDocumentId::parse(format!("zim-{}-{short_digest}", candidate.entry_index))?;
        let segment_id =
            ManagedSegmentId::parse(format!("zim-{}-{short_digest}-0", candidate.entry_index))?;
        let title_source = if entry.title.trim().is_empty() {
            entry.path.as_str()
        } else {
            entry.title.as_str()
        };
        let title = truncate_utf8(title_source, 8_192).to_string();
        let mut segment = ManagedTextSegment {
            segment_id,
            ordinal: 0,
            text: extracted.text,
            text_sha256: String::new(),
            locator: locator.clone(),
        };
        segment.refresh_text_sha256();
        documents.push(ManagedDocument {
            document_id,
            title,
            creator: None,
            source_uri: Some(request.source_uri.clone()),
            locator: locator.clone(),
            immutable: true,
            visibility: ManagedDocumentVisibility::Private,
            lineage: vec![ManagedDocumentLineage {
                source_artifact_id: request.source_artifact_id.clone(),
                source_record_id: format!("{internal_path}#{}", candidate.entry_index),
                source_record_sha256: raw_sha256,
                source_locator: locator,
                transformation: format!(
                    "{ZIM_PRODUCER_VERSION}; mime={}",
                    match candidate.text_kind {
                        ZimTextKind::Html => "html",
                        ZimTextKind::PlainText => "plain_text",
                    }
                ),
            }],
            rights: request.rights.clone(),
            use_policy: request.use_policy,
            segments: vec![segment],
        });
    }
    let total_text_bytes =
        u64::try_from(total_text_bytes).map_err(|_| ZimError::IntegerOverflow)?;
    Ok((documents, total_text_bytes, truncated_article_count))
}

struct DecodedCluster {
    codec: &'static str,
    decoded: Vec<u8>,
    offsets: Vec<usize>,
}

impl DecodedCluster {
    fn blob(
        &self,
        blob_index: u32,
        cluster_index: u32,
        max_blob_bytes: usize,
    ) -> Result<&[u8], ZimError> {
        let index = usize::try_from(blob_index).map_err(|_| ZimError::IntegerOverflow)?;
        let end_index = index.checked_add(1).ok_or(ZimError::IntegerOverflow)?;
        let start = *self.offsets.get(index).ok_or(ZimError::InvalidCluster {
            cluster_index,
            reason: "blob index is outside the cluster offset table",
        })?;
        let end = *self
            .offsets
            .get(end_index)
            .ok_or(ZimError::InvalidCluster {
                cluster_index,
                reason: "blob index is outside the cluster offset table",
            })?;
        let blob = self
            .decoded
            .get(start..end)
            .ok_or(ZimError::InvalidCluster {
                cluster_index,
                reason: "blob range is outside decoded cluster bytes",
            })?;
        if blob.len() > max_blob_bytes {
            return Err(ZimError::ArticleLimitExceeded);
        }
        Ok(blob)
    }
}

fn read_cluster(
    file: &mut File,
    offset: u64,
    upper: u64,
    cluster_index: u32,
    limits: ZimLimits,
) -> Result<DecodedCluster, ZimError> {
    let span = upper.checked_sub(offset).ok_or(ZimError::IntegerOverflow)?;
    if span < 2 {
        return Err(ZimError::InvalidCluster {
            cluster_index,
            reason: "cluster is truncated",
        });
    }
    let payload_len_u64 = span.checked_sub(1).ok_or(ZimError::IntegerOverflow)?;
    let payload_len =
        usize::try_from(payload_len_u64).map_err(|_| ZimError::ClusterLimitExceeded)?;
    if payload_len > limits.max_compressed_cluster_bytes {
        return Err(ZimError::ClusterLimitExceeded);
    }
    let info = read_range(file, offset, 1, "cluster info")?[0];
    if info & 0xe0 != 0 {
        return Err(ZimError::InvalidCluster {
            cluster_index,
            reason: "reserved cluster-info bits are set",
        });
    }
    let extended_offsets = info & 0x10 != 0;
    let compression = info & 0x0f;
    let payload_offset = offset.checked_add(1).ok_or(ZimError::IntegerOverflow)?;
    let payload = read_range(file, payload_offset, payload_len, "cluster payload")?;
    let (codec, decoded) = match compression {
        0 | 1 => {
            if payload.len() > limits.max_decoded_cluster_bytes {
                return Err(ZimError::ClusterLimitExceeded);
            }
            ("none", payload)
        }
        5 => (
            "zstd",
            decode_zstd(
                &payload,
                limits.max_zstd_window_bytes,
                limits.max_decoded_cluster_bytes,
            )?,
        ),
        unsupported => return Err(ZimError::UnsupportedCompression(unsupported)),
    };
    let offsets = parse_blob_offsets(
        &decoded,
        extended_offsets,
        cluster_index,
        limits.max_cluster_blob_count,
    )?;
    Ok(DecodedCluster {
        codec,
        decoded,
        offsets,
    })
}

fn decode_zstd(
    bytes: &[u8],
    max_window_bytes: u64,
    max_output_bytes: usize,
) -> Result<Vec<u8>, ZimError> {
    let mut decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
        Cursor::new(bytes),
        max_window_bytes,
    )
    .map_err(|error| ZimError::Zstd(error.to_string()))?;
    let mut output = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = decoder
            .read(&mut chunk)
            .map_err(|error| ZimError::Zstd(error.to_string()))?;
        if read == 0 {
            break;
        }
        let next_len = output
            .len()
            .checked_add(read)
            .ok_or(ZimError::IntegerOverflow)?;
        if next_len > max_output_bytes {
            return Err(ZimError::ClusterLimitExceeded);
        }
        output.extend_from_slice(&chunk[..read]);
    }
    let consumed = decoder.decoder.bytes_read_from_source();
    let expected = u64::try_from(bytes.len()).map_err(|_| ZimError::IntegerOverflow)?;
    if consumed != expected {
        return Err(ZimError::Zstd(
            "cluster must contain exactly one frame with no trailing bytes".to_string(),
        ));
    }
    Ok(output)
}

fn parse_blob_offsets(
    decoded: &[u8],
    extended: bool,
    cluster_index: u32,
    max_blob_count: u32,
) -> Result<Vec<usize>, ZimError> {
    let width = if extended { 8_usize } else { 4_usize };
    if decoded.len() < width {
        return Err(ZimError::InvalidCluster {
            cluster_index,
            reason: "cluster blob offset table is truncated",
        });
    }
    let first = if extended {
        usize::try_from(read_u64(decoded, 0)?).map_err(|_| ZimError::IntegerOverflow)?
    } else {
        usize::try_from(read_u32(decoded, 0)?).map_err(|_| ZimError::IntegerOverflow)?
    };
    if first < width || first % width != 0 || first > decoded.len() {
        return Err(ZimError::InvalidCluster {
            cluster_index,
            reason: "invalid first blob offset",
        });
    }
    let count = first / width;
    let max_offsets = usize::try_from(max_blob_count)
        .map_err(|_| ZimError::IntegerOverflow)?
        .checked_add(1)
        .ok_or(ZimError::IntegerOverflow)?;
    if count > max_offsets {
        return Err(ZimError::InvalidCluster {
            cluster_index,
            reason: "cluster blob count exceeds its configured limit",
        });
    }
    let mut offsets = Vec::with_capacity(count);
    for index in 0..count {
        let position = index.checked_mul(width).ok_or(ZimError::IntegerOverflow)?;
        let value = if extended {
            usize::try_from(read_u64(decoded, position)?).map_err(|_| ZimError::IntegerOverflow)?
        } else {
            usize::try_from(read_u32(decoded, position)?).map_err(|_| ZimError::IntegerOverflow)?
        };
        if value < first
            || value > decoded.len()
            || offsets.last().is_some_and(|last| *last > value)
        {
            return Err(ZimError::InvalidCluster {
                cluster_index,
                reason: "cluster blob offsets are unordered or out of bounds",
            });
        }
        offsets.push(value);
    }
    if offsets.last().copied() != Some(decoded.len()) {
        return Err(ZimError::InvalidCluster {
            cluster_index,
            reason: "cluster blob table does not cover the decoded payload exactly",
        });
    }
    Ok(offsets)
}

#[allow(clippy::too_many_arguments)]
fn read_directory_entry(
    file: &mut File,
    offset: u64,
    upper: u64,
    entry_index: u32,
    mime_types: &[String],
    entry_count: u32,
    cluster_count: u32,
    limits: ZimLimits,
) -> Result<(DirectoryEntry, usize), ZimError> {
    let available = upper.checked_sub(offset).ok_or(ZimError::IntegerOverflow)?;
    let max_dirent_bytes =
        u64::try_from(limits.max_dirent_bytes).map_err(|_| ZimError::IntegerOverflow)?;
    let read_len =
        usize::try_from(available.min(max_dirent_bytes)).map_err(|_| ZimError::IntegerOverflow)?;
    let bytes = read_range(file, offset, read_len, "directory entry")?;
    let mut cursor = ByteCursor::new(&bytes);
    let mime_index = cursor
        .u16()
        .map_err(|_| invalid_entry(entry_index, "fixed header is truncated"))?;
    let extra_len = usize::from(
        cursor
            .u8()
            .map_err(|_| invalid_entry(entry_index, "fixed header is truncated"))?,
    );
    let namespace = cursor
        .u8()
        .map_err(|_| invalid_entry(entry_index, "fixed header is truncated"))?;
    let _version = cursor
        .u32()
        .map_err(|_| invalid_entry(entry_index, "fixed header is truncated"))?;
    if !namespace.is_ascii_graphic() || namespace == b'/' {
        return Err(invalid_entry(
            entry_index,
            "namespace is not a safe ASCII byte",
        ));
    }
    let kind = match mime_index {
        REDIRECT_MIME => {
            let redirect = cursor
                .u32()
                .map_err(|_| invalid_entry(entry_index, "redirect target is truncated"))?;
            if redirect >= entry_count {
                return Err(invalid_entry(
                    entry_index,
                    "redirect target is out of bounds",
                ));
            }
            DirectoryEntryKind::Redirect
        }
        LINKTARGET_MIME | DELETED_MIME => DirectoryEntryKind::LinktargetOrDeleted,
        mime_index => {
            if usize::from(mime_index) >= mime_types.len() {
                return Err(invalid_entry(entry_index, "MIME index is out of bounds"));
            }
            let cluster_index = cursor
                .u32()
                .map_err(|_| invalid_entry(entry_index, "cluster index is truncated"))?;
            let blob_index = cursor
                .u32()
                .map_err(|_| invalid_entry(entry_index, "blob index is truncated"))?;
            if cluster_index >= cluster_count {
                return Err(invalid_entry(entry_index, "cluster index is out of bounds"));
            }
            DirectoryEntryKind::Item {
                mime_index,
                cluster_index,
                blob_index,
            }
        }
    };
    let path = cursor
        .nul_string(limits.max_path_bytes)
        .map_err(|reason| invalid_entry(entry_index, reason))?;
    let title = cursor
        .nul_string(limits.max_title_bytes)
        .map_err(|reason| invalid_entry(entry_index, reason))?;
    cursor
        .skip(extra_len)
        .map_err(|_| invalid_entry(entry_index, "parameter bytes are truncated"))?;
    if path.is_empty() {
        return Err(invalid_entry(entry_index, "path is empty"));
    }
    if path.chars().any(char::is_control) || title.chars().any(char::is_control) {
        return Err(invalid_entry(
            entry_index,
            "path or title contains control characters",
        ));
    }
    Ok((
        DirectoryEntry {
            namespace,
            path,
            title,
            kind,
        },
        cursor.position,
    ))
}

fn validate_title_index(indices: &[u32], entries: &[DirectoryEntry]) -> Result<(), ZimError> {
    let mut seen = BTreeSet::new();
    let mut previous: Option<(u8, &str, &str)> = None;
    for index in indices {
        let index_usize = usize::try_from(*index).map_err(|_| ZimError::IntegerOverflow)?;
        let entry = entries.get(index_usize).ok_or(ZimError::InvalidHeader(
            "title index contains an out-of-bounds entry",
        ))?;
        if !seen.insert(*index) {
            return Err(ZimError::InvalidHeader(
                "title index is not a permutation of entries",
            ));
        }
        let title = if entry.title.is_empty() {
            entry.path.as_str()
        } else {
            entry.title.as_str()
        };
        let current = (entry.namespace, title, entry.path.as_str());
        if previous.is_some_and(|previous| previous > current) {
            return Err(ZimError::InvalidHeader("title index is not ordered"));
        }
        previous = Some(current);
    }
    Ok(())
}

fn read_mime_list(
    file: &mut File,
    start: u64,
    upper: u64,
    max_bytes: usize,
) -> Result<(Vec<String>, Range<u64>), ZimError> {
    let available = upper.checked_sub(start).ok_or(ZimError::IntegerOverflow)?;
    let max_bytes = u64::try_from(max_bytes).map_err(|_| ZimError::IntegerOverflow)?;
    let read_len =
        usize::try_from(available.min(max_bytes)).map_err(|_| ZimError::IntegerOverflow)?;
    let bytes = read_range(file, start, read_len, "MIME list")?;
    let terminator = bytes
        .windows(2)
        .position(|window| window == [0, 0])
        .ok_or(ZimError::InvalidMimeList)?;
    if terminator == 0 {
        return Err(ZimError::InvalidMimeList);
    }
    let mut mime_types = Vec::new();
    for raw in bytes[..terminator].split(|byte| *byte == 0) {
        if raw.is_empty() || raw.len() > 255 {
            return Err(ZimError::InvalidMimeList);
        }
        let value = std::str::from_utf8(raw).map_err(|_| ZimError::InvalidMimeList)?;
        if !value.is_ascii() || value.chars().any(char::is_whitespace) {
            return Err(ZimError::InvalidMimeList);
        }
        mime_types.push(value.to_ascii_lowercase());
    }
    if mime_types.len() > usize::from(u16::MAX) - 2 {
        return Err(ZimError::InvalidMimeList);
    }
    let consumed = terminator.checked_add(2).ok_or(ZimError::IntegerOverflow)?;
    let end = start
        .checked_add(u64::try_from(consumed).map_err(|_| ZimError::IntegerOverflow)?)
        .ok_or(ZimError::IntegerOverflow)?;
    Ok((mime_types, start..end))
}

fn classify_text_mime(value: &str) -> Option<ZimTextKind> {
    match value {
        "text/html" | "application/xhtml+xml" => Some(ZimTextKind::Html),
        "text/plain" => Some(ZimTextKind::PlainText),
        _ => None,
    }
}

fn table_range(
    start: u64,
    count: u32,
    width: u64,
    archive_len: u64,
    label: &'static str,
) -> Result<Range<u64>, ZimError> {
    let bytes = u64::from(count)
        .checked_mul(width)
        .ok_or(ZimError::IntegerOverflow)?;
    let end = start.checked_add(bytes).ok_or(ZimError::IntegerOverflow)?;
    if start < HEADER_BYTES_U64 || end > archive_len || start >= end {
        return Err(ZimError::OutOfBounds(label));
    }
    Ok(start..end)
}

fn validate_disjoint_regions(regions: &[Range<u64>]) -> Result<(), ZimError> {
    let mut sorted = regions.to_vec();
    sorted.sort_by_key(|range| range.start);
    for pair in sorted.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(ZimError::OverlappingRegions);
        }
    }
    Ok(())
}

fn validate_content_anchors(
    entry_offsets: &[u64],
    cluster_offsets: &[u64],
    fixed_regions: &[Range<u64>],
    checksum_position: u64,
) -> Result<(), ZimError> {
    let entry_set: BTreeSet<_> = entry_offsets.iter().copied().collect();
    if cluster_offsets
        .iter()
        .any(|offset| entry_set.contains(offset))
    {
        return Err(ZimError::OverlappingRegions);
    }
    for offset in entry_offsets.iter().chain(cluster_offsets) {
        if *offset < HEADER_BYTES_U64
            || *offset >= checksum_position
            || fixed_regions
                .iter()
                .any(|range| *offset >= range.start && *offset < range.end)
        {
            return Err(ZimError::OutOfBounds("content pointer"));
        }
    }
    Ok(())
}

fn build_anchors(
    entry_offsets: &[u64],
    cluster_offsets: &[u64],
    fixed_regions: &[Range<u64>],
    checksum_position: u64,
) -> Vec<u64> {
    let mut anchors = BTreeSet::new();
    anchors.extend(entry_offsets.iter().copied());
    anchors.extend(cluster_offsets.iter().copied());
    anchors.extend(fixed_regions.iter().map(|range| range.start));
    anchors.insert(checksum_position);
    anchors.into_iter().collect()
}

fn next_anchor(anchors: &[u64], offset: u64) -> Result<u64, ZimError> {
    let index = anchors.partition_point(|anchor| *anchor <= offset);
    anchors
        .get(index)
        .copied()
        .ok_or(ZimError::OutOfBounds("content extent"))
}

fn validate_strict_offsets(offsets: &[u64], label: &'static str) -> Result<(), ZimError> {
    if offsets.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(ZimError::InvalidHeader(label));
    }
    Ok(())
}

fn read_u64_table(
    file: &mut File,
    range: &Range<u64>,
    count: u32,
    operation: &'static str,
) -> Result<Vec<u64>, ZimError> {
    let len = usize::try_from(
        range
            .end
            .checked_sub(range.start)
            .ok_or(ZimError::IntegerOverflow)?,
    )
    .map_err(|_| ZimError::IntegerOverflow)?;
    let bytes = read_range(file, range.start, len, operation)?;
    let mut values =
        Vec::with_capacity(usize::try_from(count).map_err(|_| ZimError::IntegerOverflow)?);
    for index in 0..usize::try_from(count).map_err(|_| ZimError::IntegerOverflow)? {
        values.push(read_u64(
            &bytes,
            index.checked_mul(8).ok_or(ZimError::IntegerOverflow)?,
        )?);
    }
    Ok(values)
}

fn read_u32_table(
    file: &mut File,
    range: &Range<u64>,
    count: u32,
    operation: &'static str,
) -> Result<Vec<u32>, ZimError> {
    let len = usize::try_from(
        range
            .end
            .checked_sub(range.start)
            .ok_or(ZimError::IntegerOverflow)?,
    )
    .map_err(|_| ZimError::IntegerOverflow)?;
    let bytes = read_range(file, range.start, len, operation)?;
    let mut values =
        Vec::with_capacity(usize::try_from(count).map_err(|_| ZimError::IntegerOverflow)?);
    for index in 0..usize::try_from(count).map_err(|_| ZimError::IntegerOverflow)? {
        values.push(read_u32(
            &bytes,
            index.checked_mul(4).ok_or(ZimError::IntegerOverflow)?,
        )?);
    }
    Ok(values)
}

fn read_range(
    file: &mut File,
    offset: u64,
    len: usize,
    operation: &'static str,
) -> Result<Vec<u8>, ZimError> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| ZimError::io(operation, source))?;
    let mut bytes = vec![0_u8; len];
    file.read_exact(&mut bytes).map_err(|source| {
        if source.kind() == std::io::ErrorKind::UnexpectedEof {
            ZimError::Truncated(operation)
        } else {
            ZimError::io(operation, source)
        }
    })?;
    Ok(bytes)
}

fn hash_file(file: &mut File) -> Result<String, ZimError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| ZimError::io("seeking archive for hashing", source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| ZimError::io("hashing archive", source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn normalize_sha256(value: &str) -> Result<String, ZimError> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ZimError::InvalidExpectedSha256);
    }
    Ok(digest.to_ascii_lowercase())
}

fn is_split_archive_name(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    bytes.len() >= 6
        && bytes[bytes.len() - 6..bytes.len() - 2] == *b".zim"
        && bytes[bytes.len() - 2..].iter().all(u8::is_ascii_lowercase)
}

fn invalid_entry(entry_index: u32, reason: &'static str) -> ZimError {
    ZimError::InvalidDirectoryEntry {
        entry_index,
        reason,
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    &value[..boundary]
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ZimError> {
    let raw: [u8; 2] = bytes
        .get(offset..offset.checked_add(2).ok_or(ZimError::IntegerOverflow)?)
        .ok_or(ZimError::Truncated("u16"))?
        .try_into()
        .map_err(|_| ZimError::Truncated("u16"))?;
    Ok(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ZimError> {
    let raw: [u8; 4] = bytes
        .get(offset..offset.checked_add(4).ok_or(ZimError::IntegerOverflow)?)
        .ok_or(ZimError::Truncated("u32"))?
        .try_into()
        .map_err(|_| ZimError::Truncated("u32"))?;
    Ok(u32::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ZimError> {
    let raw: [u8; 8] = bytes
        .get(offset..offset.checked_add(8).ok_or(ZimError::IntegerOverflow)?)
        .ok_or(ZimError::Truncated("u64"))?
        .try_into()
        .map_err(|_| ZimError::Truncated("u64"))?;
    Ok(u64::from_le_bytes(raw))
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn u8(&mut self) -> Result<u8, ()> {
        let value = self.bytes.get(self.position).copied().ok_or(())?;
        self.position = self.position.checked_add(1).ok_or(())?;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, ()> {
        let end = self.position.checked_add(2).ok_or(())?;
        let raw: [u8; 2] = self
            .bytes
            .get(self.position..end)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?;
        self.position = end;
        Ok(u16::from_le_bytes(raw))
    }

    fn u32(&mut self) -> Result<u32, ()> {
        let end = self.position.checked_add(4).ok_or(())?;
        let raw: [u8; 4] = self
            .bytes
            .get(self.position..end)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?;
        self.position = end;
        Ok(u32::from_le_bytes(raw))
    }

    fn nul_string(&mut self, max_bytes: usize) -> Result<String, &'static str> {
        let remaining = self
            .bytes
            .get(self.position..)
            .ok_or("string is truncated")?;
        let terminator = remaining
            .iter()
            .take(max_bytes.saturating_add(1))
            .position(|byte| *byte == 0)
            .ok_or("string is unterminated or exceeds its byte limit")?;
        if terminator > max_bytes {
            return Err("string exceeds its byte limit");
        }
        let raw = &remaining[..terminator];
        let value = std::str::from_utf8(raw).map_err(|_| "string is not UTF-8")?;
        self.position = self
            .position
            .checked_add(terminator)
            .and_then(|position| position.checked_add(1))
            .ok_or("string offset overflows")?;
        Ok(value.to_string())
    }

    fn skip(&mut self, bytes: usize) -> Result<(), ()> {
        let end = self.position.checked_add(bytes).ok_or(())?;
        if end > self.bytes.len() {
            return Err(());
        }
        self.position = end;
        Ok(())
    }
}
