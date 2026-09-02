#![forbid(unsafe_code)]

//! Bounded OpenZIM parsing and source-neutral managed-document production.
//!
//! This crate opens one caller-authorized local archive read-only. It has no
//! network, process, memory-map, FFI, renderer, or model authority. Archive
//! HTML is reduced to inert UTF-8 text before it crosses the public boundary.

mod reader;
mod text;

use chrono::{DateTime, Utc};
use information_native_types::{
    ArtifactId, ContractError, ManagedDocumentsV1, ManagedMaterializationId, ReleaseId,
    RepresentationId, ResourceId, RightsStatement, UsePolicy, default_managed_document_rights,
    default_managed_document_use_policy,
};
use serde::Serialize;
use std::path::PathBuf;
use thiserror::Error;

pub use reader::produce_managed_documents;

pub const ZIM_PRODUCER_VERSION: &str = "information.openzim.inert_text.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZimTextKind {
    Html,
    PlainText,
}

/// Explicit allocation and work ceilings for one archive conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZimLimits {
    pub max_archive_bytes: u64,
    pub max_entries: u32,
    pub max_clusters: u32,
    pub max_mime_list_bytes: usize,
    pub max_dirent_bytes: usize,
    pub max_total_dirent_bytes: usize,
    pub max_path_bytes: usize,
    pub max_title_bytes: usize,
    pub max_compressed_cluster_bytes: usize,
    pub max_decoded_cluster_bytes: usize,
    pub max_cluster_blob_count: u32,
    pub max_zstd_window_bytes: u64,
    pub max_blob_bytes: usize,
    pub max_total_selected_blob_bytes: usize,
    pub max_article_text_bytes: usize,
    pub max_candidate_entries: usize,
    pub max_documents: usize,
    pub max_total_text_bytes: usize,
    pub max_decoded_clusters: usize,
    pub max_total_decoded_cluster_bytes: usize,
}

impl Default for ZimLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 64 * 1024 * 1024 * 1024,
            max_entries: 1_000_000,
            max_clusters: 1_000_000,
            max_mime_list_bytes: 64 * 1024,
            max_dirent_bytes: 64 * 1024,
            max_total_dirent_bytes: 256 * 1024 * 1024,
            max_path_bytes: 8_000,
            max_title_bytes: 8 * 1024,
            max_compressed_cluster_bytes: 64 * 1024 * 1024,
            max_decoded_cluster_bytes: 128 * 1024 * 1024,
            max_cluster_blob_count: 1_000_000,
            max_zstd_window_bytes: 64 * 1024 * 1024,
            max_blob_bytes: 4 * 1024 * 1024,
            max_total_selected_blob_bytes: 256 * 1024 * 1024,
            max_article_text_bytes: information_native_types::MAX_MANAGED_SEGMENT_BYTES,
            max_candidate_entries: 40_000,
            max_documents: information_native_types::MAX_MANAGED_DOCUMENTS_PER_MATERIALIZATION,
            max_total_text_bytes: 256 * 1024 * 1024,
            max_decoded_clusters: 4_096,
            max_total_decoded_cluster_bytes: 512 * 1024 * 1024,
        }
    }
}

impl ZimLimits {
    fn validate(self) -> Result<(), ZimError> {
        let max_managed_text = usize::try_from(information_native_types::MAX_MANAGED_TEXT_BYTES)
            .map_err(|_| ZimError::InvalidLimits)?;
        if self.max_archive_bytes < 96
            || self.max_entries == 0
            || self.max_clusters == 0
            || self.max_mime_list_bytes < 2
            || self.max_dirent_bytes < 18
            || self.max_total_dirent_bytes == 0
            || self.max_path_bytes == 0
            || self.max_title_bytes == 0
            || self.max_compressed_cluster_bytes == 0
            || self.max_decoded_cluster_bytes < 8
            || self.max_cluster_blob_count == 0
            || self.max_zstd_window_bytes == 0
            || self.max_blob_bytes == 0
            || self.max_total_selected_blob_bytes == 0
            || self.max_article_text_bytes == 0
            || self.max_candidate_entries == 0
            || self.max_documents == 0
            || self.max_total_text_bytes == 0
            || self.max_decoded_clusters == 0
            || self.max_total_decoded_cluster_bytes == 0
        {
            return Err(ZimError::InvalidLimits);
        }
        if self.max_article_text_bytes > information_native_types::MAX_MANAGED_SEGMENT_BYTES
            || self.max_documents
                > information_native_types::MAX_MANAGED_DOCUMENTS_PER_MATERIALIZATION
            || self.max_candidate_entries < self.max_documents
            || self.max_path_bytes > 8_000
            || self.max_title_bytes > 8_192
            || self.max_cluster_blob_count > self.max_entries
            || self.max_total_text_bytes > max_managed_text
        {
            return Err(ZimError::InvalidLimits);
        }
        Ok(())
    }
}

/// Native-only conversion request. The path is never serializable or exposed
/// to renderer/tool contracts; callers must already possess local-file
/// authority and an acquisition-bound expected size and SHA-256.
#[derive(Debug, Clone)]
pub struct ZimMaterializationRequest {
    pub archive_path: PathBuf,
    pub expected_archive_bytes: u64,
    pub expected_archive_sha256: String,
    pub source_artifact_id: ArtifactId,
    pub source_uri: String,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub materialization_id: ManagedMaterializationId,
    pub publisher: String,
    pub created_at: DateTime<Utc>,
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
    pub limits: ZimLimits,
}

impl ZimMaterializationRequest {
    /// Apply the locked promotion defaults: private visibility, local search
    /// allowed, model use unknown/fail-closed, export and redistribution
    /// forbidden.
    #[allow(clippy::too_many_arguments)]
    pub fn private(
        archive_path: PathBuf,
        expected_archive_bytes: u64,
        expected_archive_sha256: String,
        source_artifact_id: ArtifactId,
        source_uri: String,
        resource_id: ResourceId,
        release_id: ReleaseId,
        representation_id: RepresentationId,
        materialization_id: ManagedMaterializationId,
        publisher: String,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            archive_path,
            expected_archive_bytes,
            expected_archive_sha256,
            source_artifact_id,
            source_uri,
            resource_id,
            release_id,
            representation_id,
            materialization_id,
            publisher,
            created_at,
            rights: default_managed_document_rights(),
            use_policy: default_managed_document_use_policy(),
            limits: ZimLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ZimArchiveIdentity {
    pub bytes: u64,
    pub sha256: String,
    pub uuid_hex: String,
    pub major_version: u16,
    pub minor_version: u16,
    pub entry_count: u32,
    pub cluster_count: u32,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ZimOmissions {
    pub redirects: u64,
    pub linktargets_or_deleted: u64,
    pub non_article_namespaces: u64,
    pub non_text_mime_types: u64,
    pub invalid_or_empty_text: u64,
    pub document_limit: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ZimProductionReport {
    pub archive: ZimArchiveIdentity,
    /// Directory identity records whose bounded fixed fields, namespace/path
    /// order, MIME index, and redirect/cluster index were validated. Blob
    /// ranges are validated only for clusters actually decoded below.
    pub validated_directory_entry_count: u32,
    pub materialized_document_count: u64,
    pub materialized_text_bytes: u64,
    pub decoded_cluster_count: u64,
    pub codecs_used: Vec<String>,
    pub truncated_article_count: u64,
    pub omissions: ZimOmissions,
}

#[derive(Debug, Clone)]
pub struct ProducedManagedDocuments {
    pub documents: ManagedDocumentsV1,
    pub report: ZimProductionReport,
}

#[derive(Debug, Error)]
pub enum ZimError {
    #[error("invalid OpenZIM limits")]
    InvalidLimits,
    #[error("invalid OpenZIM materialization request: {0}")]
    InvalidRequest(&'static str),
    #[error("archive path is a symbolic link")]
    SymlinkArchive,
    #[error("split OpenZIM archives are not supported")]
    SplitArchiveUnsupported,
    #[error("archive is not a regular file")]
    NotARegularFile,
    #[error("archive size {actual} does not match expected {expected}")]
    ArchiveSizeMismatch { expected: u64, actual: u64 },
    #[error("archive exceeds the configured byte limit")]
    ArchiveLimitExceeded,
    #[error("invalid expected SHA-256 digest")]
    InvalidExpectedSha256,
    #[error("archive SHA-256 does not match the acquisition identity")]
    ArchiveHashMismatch,
    #[error("archive changed while it was being parsed")]
    ArchiveChanged,
    #[error("truncated OpenZIM structure: {0}")]
    Truncated(&'static str),
    #[error("invalid OpenZIM magic")]
    InvalidMagic,
    #[error("unsupported OpenZIM major version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid OpenZIM header: {0}")]
    InvalidHeader(&'static str),
    #[error("OpenZIM configured count exceeds a parsing limit: {0}")]
    CountLimit(&'static str),
    #[error("OpenZIM offset arithmetic overflow")]
    IntegerOverflow,
    #[error("OpenZIM range is outside the archive: {0}")]
    OutOfBounds(&'static str),
    #[error("OpenZIM structural regions overlap")]
    OverlappingRegions,
    #[error("invalid OpenZIM MIME list")]
    InvalidMimeList,
    #[error("invalid OpenZIM directory entry {entry_index}: {reason}")]
    InvalidDirectoryEntry {
        entry_index: u32,
        reason: &'static str,
    },
    #[error("unsupported OpenZIM cluster compression code {0}")]
    UnsupportedCompression(u8),
    #[error("invalid OpenZIM cluster {cluster_index}: {reason}")]
    InvalidCluster {
        cluster_index: u32,
        reason: &'static str,
    },
    #[error("OpenZIM cluster exceeds a configured byte limit")]
    ClusterLimitExceeded,
    #[error("OpenZIM article exceeds a configured byte limit")]
    ArticleLimitExceeded,
    #[error("OpenZIM article is not UTF-8")]
    InvalidArticleUtf8,
    #[error("malformed archive HTML")]
    MalformedHtml,
    #[error("archive contains no materializable inert text articles")]
    NoMaterializableArticles,
    #[error("I/O failed while {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("Zstandard decoding failed: {0}")]
    Zstd(String),
    #[error(transparent)]
    Contract(#[from] ContractError),
}

impl ZimError {
    fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io { operation, source }
    }
}

#[cfg(test)]
mod tests;
