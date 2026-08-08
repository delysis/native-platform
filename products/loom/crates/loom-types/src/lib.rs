#![forbid(unsafe_code)]

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

mod generation;

pub use generation::*;

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new())
            }

            pub const fn from_ulid(value: Ulid) -> Self {
                Self(value)
            }

            pub const fn as_ulid(self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ulid::from_str(value).map(Self)
            }
        }
    };
}

ulid_id!(ArtifactId);
ulid_id!(BranchId);
ulid_id!(CandidateId);
ulid_id!(CommandId);
ulid_id!(DocumentId);
ulid_id!(GenerationEventId);
ulid_id!(GenerationRunId);
ulid_id!(OperationId);
ulid_id!(ProjectId);
ulid_id!(RevisionId);
ulid_id!(SelectionId);

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlobId([u8; 32]);

impl BlobId {
    pub const BYTE_LEN: usize = 32;

    pub fn digest(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub const fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("BlobId")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl FromStr for BlobId {
    type Err = HashIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(value).map_err(HashIdParseError::Hex)?;
        let bytes: [u8; Self::BYTE_LEN] = bytes
            .try_into()
            .map_err(|bytes: Vec<u8>| HashIdParseError::Length(bytes.len()))?;
        Ok(Self(bytes))
    }
}

impl Serialize for BlobId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for BlobId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModelEnvironmentId([u8; 32]);

impl ModelEnvironmentId {
    pub fn digest(canonical_environment: &[u8]) -> Self {
        Self(Sha256::digest(canonical_environment).into())
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for ModelEnvironmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ModelEnvironmentId")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl fmt::Display for ModelEnvironmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl FromStr for ModelEnvironmentId {
    type Err = HashIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let blob = BlobId::from_str(value)?;
        Ok(Self(*blob.as_bytes()))
    }
}

impl Serialize for ModelEnvironmentId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for ModelEnvironmentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let blob = BlobId::deserialize(deserializer)?;
        Ok(Self(*blob.as_bytes()))
    }
}

#[derive(Debug, Error)]
pub enum HashIdParseError {
    #[error("invalid hexadecimal hash: {0}")]
    Hex(#[source] hex::FromHexError),
    #[error("hash has {0} bytes; expected 32")]
    Length(usize),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    AuthorshipAttestation,
    AuthorityPolicy,
    ContextRecipe,
    DocumentRevision,
    Evaluation,
    GeneratedSpan,
    GenerationRun,
    HumanContribution,
    ModelEnvironment,
    PromptRecipe,
    ReplayWitness,
    SearchRun,
    SelectionEvent,
    SourceExcerpt,
    TextBlob,
    TokenTrace,
}

impl ArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorshipAttestation => "authorship_attestation",
            Self::AuthorityPolicy => "authority_policy",
            Self::ContextRecipe => "context_recipe",
            Self::DocumentRevision => "document_revision",
            Self::Evaluation => "evaluation",
            Self::GeneratedSpan => "generated_span",
            Self::GenerationRun => "generation_run",
            Self::HumanContribution => "human_contribution",
            Self::ModelEnvironment => "model_environment",
            Self::PromptRecipe => "prompt_recipe",
            Self::ReplayWitness => "replay_witness",
            Self::SearchRun => "search_run",
            Self::SelectionEvent => "selection_event",
            Self::SourceExcerpt => "source_excerpt",
            Self::TextBlob => "text_blob",
            Self::TokenTrace => "token_trace",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Compose,
    Evaluate,
    Export,
    Generate,
    HumanEdit,
    Import,
    Merge,
    Retrieve,
    Select,
    Split,
}

impl OperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compose => "compose",
            Self::Evaluate => "evaluate",
            Self::Export => "export",
            Self::Generate => "generate",
            Self::HumanEdit => "human_edit",
            Self::Import => "import",
            Self::Merge => "merge",
            Self::Retrieve => "retrieve",
            Self::Select => "select",
            Self::Split => "split",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Hybrid,
    Prose,
    Verse,
}

impl DocumentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Prose => "prose",
            Self::Verse => "verse",
        }
    }

    pub const fn default_extension(self) -> &'static str {
        match self {
            Self::Hybrid | Self::Prose => "md",
            Self::Verse => "txt",
        }
    }
}

impl FromStr for DocumentKind {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "hybrid" => Ok(Self::Hybrid),
            "prose" => Ok(Self::Prose),
            "verse" => Ok(Self::Verse),
            _ => Err(ParseEnumError::new("document kind", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionKind {
    Generated,
    Human,
    Mixed,
    Source,
}

impl ContributionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Human => "human",
            Self::Mixed => "mixed",
            Self::Source => "source",
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("invalid {kind} `{value}`")]
pub struct ParseEnumError {
    kind: &'static str,
    value: String,
}

impl ParseEnumError {
    pub fn new(kind: &'static str, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub const fn new(start: u64, end: u64) -> Option<Self> {
        if start <= end {
            Some(Self { start, end })
        } else {
            None
        }
    }

    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub blob_id: BlobId,
    pub kind: ArtifactKind,
    pub media_type: String,
    pub created_at_ms: i64,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Operation {
    pub id: OperationId,
    pub kind: OperationKind,
    pub ordered_inputs: Vec<ArtifactId>,
    pub ordered_outputs: Vec<ArtifactId>,
    pub created_at_ms: i64,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionSegment {
    pub artifact_id: ArtifactId,
    pub byte_range: ByteRange,
    pub contribution: ContributionKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentRevision {
    pub id: RevisionId,
    pub document_id: DocumentId,
    pub parent_revision_id: Option<RevisionId>,
    pub segments: Vec<RevisionSegment>,
    pub created_at_ms: i64,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    CancelGeneration,
    Checkpoint,
    CloseProject,
    CreateDocument,
    Export,
    Import,
    InitProject,
    KeepAlternative,
    OpenProject,
    PromoteCandidate,
    ReconcileExternal,
    Recover,
    Weave,
}

impl CommandKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CancelGeneration => "cancel_generation",
            Self::Checkpoint => "checkpoint",
            Self::CloseProject => "close_project",
            Self::CreateDocument => "create_document",
            Self::Export => "export",
            Self::Import => "import",
            Self::InitProject => "init_project",
            Self::KeepAlternative => "keep_alternative",
            Self::OpenProject => "open_project",
            Self::PromoteCandidate => "promote_candidate",
            Self::ReconcileExternal => "reconcile_external",
            Self::Recover => "recover",
            Self::Weave => "weave",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandReceipt {
    pub command_id: CommandId,
    pub command: CommandKind,
    pub project_id: ProjectId,
    pub project_schema_version: u32,
    pub source_revision_id: Option<RevisionId>,
    pub resulting_artifact_ids: Vec<ArtifactId>,
    pub resulting_operation_ids: Vec<OperationId>,
    pub resulting_revision_ids: Vec<RevisionId>,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectManifest {
    pub format: String,
    pub schema_version: u32,
    pub project_id: ProjectId,
    pub name: String,
    pub created_at_ms: i64,
}

pub fn now_unix_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_id_json_is_readable_and_round_trips() {
        let id = BlobId::digest(b"loom");
        let json = serde_json::to_string(&id).expect("serialize hash");
        assert_eq!(json.len(), 66);
        assert_eq!(
            serde_json::from_str::<BlobId>(&json).expect("parse hash"),
            id
        );
    }

    #[test]
    fn occurrence_ids_are_distinct() {
        assert_ne!(ArtifactId::new(), ArtifactId::new());
    }

    #[test]
    fn byte_ranges_reject_reversal() {
        assert!(ByteRange::new(9, 2).is_none());
        assert_eq!(ByteRange::new(2, 9).expect("valid range").len(), 7);
    }
}
