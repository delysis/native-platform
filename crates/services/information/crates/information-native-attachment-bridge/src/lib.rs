#![forbid(unsafe_code)]

//! Exact, path-free promotion of one canonical Attachment text artifact into
//! Information's immutable managed-document store.
//!
//! Attachment remains authoritative for hostile source bytes, graph identity,
//! canonicalization, and processor policy. Information becomes authoritative
//! only for the derived managed release after atomic activation. This bridge
//! owns neither source paths nor product consent UI.

use attachment_native_types::{
    ArtifactId as AttachmentArtifactId, ArtifactPayload, AttachmentBundle, AttachmentGraph,
    AttachmentReceipt, CanonicalArtifact, ContractError as AttachmentContractError, EdgeOutcome,
    ObjectId, ProcessorProvenance,
};
use chrono::{DateTime, Utc};
use information_native_store::{ManagedStore, StoreError};
use information_native_types::{
    ArtifactId as InformationArtifactId, ContractError as InformationContractError,
    EvidenceLocator, MANAGED_DOCUMENTS_SCHEMA, ManagedDocument, ManagedDocumentId,
    ManagedDocumentLineage, ManagedDocumentVisibility, ManagedDocumentsReceipt, ManagedDocumentsV1,
    ManagedMaterializationId, ManagedSegmentId, ManagedSourceArtifact, ManagedTextSegment,
    Provenance, RedistributionPolicy, ReleaseId, RepresentationId, ResourceId, RightsStatement,
    UsePermission, UsePolicy, managed_text_sha256,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;

pub const ATTACHMENT_TEXT_MATERIALIZATION_SCHEMA: &str =
    "information_native.attachment_text_materialization.v1";
pub const ATTACHMENT_TEXT_MATERIALIZATION_RECEIPT_SCHEMA: &str =
    "information_native.attachment_text_materialization_receipt.v1";
pub const ATTACHMENT_TEXT_TRANSFORMATION: &str = "attachment-information.canonical-text.v1";
pub const MAX_ATTACHMENT_TEXT_BYTES: usize = 1024 * 1024;
const MAX_TITLE_BYTES: usize = 8_192;

/// One ordered, derivation-only path from the Attachment graph root to the
/// object that owns the selected canonical artifact. Edge indexes refer to the
/// exact serialized graph vector; reordering the graph invalidates the locator
/// and its graph hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttachmentGraphLocator {
    pub root_sha256: String,
    pub source_object_sha256: String,
    pub edge_indexes: Vec<u32>,
}

impl AttachmentGraphLocator {
    fn derive(graph: &AttachmentGraph, source: &ObjectId) -> Result<Self, BridgeError> {
        graph.validate()?;
        if !graph.objects.iter().any(|object| &object.id == source) {
            return Err(BridgeError::IdentityMismatch(IdentityField::GraphLocator));
        }
        if source == &graph.root {
            return Ok(Self {
                root_sha256: graph.root.0.clone(),
                source_object_sha256: source.0.clone(),
                edge_indexes: Vec::new(),
            });
        }

        let mut visited = BTreeSet::from([graph.root.clone()]);
        let mut queue = VecDeque::from([(graph.root.clone(), Vec::<u32>::new())]);
        while let Some((parent, path)) = queue.pop_front() {
            for (index, edge) in graph.edges.iter().enumerate() {
                if edge.parent != parent || edge.outcome != EdgeOutcome::Derived {
                    continue;
                }
                let Some(child) = &edge.child else {
                    continue;
                };
                let edge_index = u32::try_from(index).map_err(|_| BridgeError::IntegerOverflow)?;
                let mut child_path = path.clone();
                child_path.push(edge_index);
                if child == source {
                    return Ok(Self {
                        root_sha256: graph.root.0.clone(),
                        source_object_sha256: source.0.clone(),
                        edge_indexes: child_path,
                    });
                }
                if visited.insert(child.clone()) {
                    queue.push_back((child.clone(), child_path));
                }
            }
        }
        Err(BridgeError::IdentityMismatch(IdentityField::GraphLocator))
    }

    fn validate_against(
        &self,
        graph: &AttachmentGraph,
        source: &ObjectId,
    ) -> Result<(), BridgeError> {
        if self.root_sha256 != graph.root.0 || self.source_object_sha256 != source.0 {
            return Err(BridgeError::IdentityMismatch(IdentityField::GraphLocator));
        }
        if source == &graph.root {
            return if self.edge_indexes.is_empty() {
                Ok(())
            } else {
                Err(BridgeError::IdentityMismatch(IdentityField::GraphLocator))
            };
        }
        if self.edge_indexes.is_empty()
            || self.edge_indexes.len() > usize::from(graph.limits.max_depth)
        {
            return Err(BridgeError::IdentityMismatch(IdentityField::GraphLocator));
        }

        let mut current = &graph.root;
        for (depth, index) in self.edge_indexes.iter().copied().enumerate() {
            let index = usize::try_from(index).map_err(|_| BridgeError::IntegerOverflow)?;
            let edge = graph
                .edges
                .get(index)
                .ok_or(BridgeError::IdentityMismatch(IdentityField::GraphLocator))?;
            let expected_depth =
                u16::try_from(depth + 1).map_err(|_| BridgeError::IntegerOverflow)?;
            if edge.parent != *current
                || edge.depth != expected_depth
                || edge.outcome != EdgeOutcome::Derived
            {
                return Err(BridgeError::IdentityMismatch(IdentityField::GraphLocator));
            }
            current = edge
                .child
                .as_ref()
                .ok_or(BridgeError::IdentityMismatch(IdentityField::GraphLocator))?;
        }
        if current != source {
            return Err(BridgeError::IdentityMismatch(IdentityField::GraphLocator));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttachmentProcessorBinding {
    pub name: String,
    pub version: String,
    pub policy_fingerprint: String,
}

impl From<&ProcessorProvenance> for AttachmentProcessorBinding {
    fn from(value: &ProcessorProvenance) -> Self {
        Self {
            name: value.name.clone(),
            version: value.version.clone(),
            policy_fingerprint: value.policy_fingerprint.clone(),
        }
    }
}

/// Rights are caller-confirmed facts, not defaults inferred by either service.
/// The digest binds the exact ordered statements presented to the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedRights {
    pub confirmed: bool,
    pub confirmed_at: DateTime<Utc>,
    pub statements: Vec<RightsStatement>,
    pub rights_sha256: String,
}

impl ConfirmedRights {
    pub fn private(
        confirmed_at: DateTime<Utc>,
        statements: Vec<RightsStatement>,
    ) -> Result<Self, BridgeError> {
        let rights_sha256 = rights_sha256(&statements)?;
        Ok(Self {
            confirmed: true,
            confirmed_at,
            statements,
            rights_sha256,
        })
    }

    fn private_use_policy(&self) -> Result<UsePolicy, BridgeError> {
        if !self.confirmed {
            return Err(BridgeError::RightsNotConfirmed);
        }
        if self.statements.is_empty() || self.statements.len() > 32 {
            return Err(BridgeError::RightsDenied);
        }
        for statement in &self.statements {
            statement.validate()?;
            if matches!(
                statement.redistribution,
                RedistributionPolicy::Unknown | RedistributionPolicy::Forbidden
            ) {
                return Err(BridgeError::RightsDenied);
            }
        }
        if !canonical_sha256(&self.rights_sha256)
            || self.rights_sha256 != rights_sha256(&self.statements)?
        {
            return Err(BridgeError::IdentityMismatch(IdentityField::Rights));
        }
        let attribution_required = self.statements.iter().any(|statement| {
            statement.redistribution == RedistributionPolicy::AllowedWithObligations
                || statement
                    .attribution
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                || statement
                    .license_url
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        });
        let policy = UsePolicy {
            local_search: UsePermission::Allowed,
            model_context: UsePermission::Unknown,
            excerpt_export: UsePermission::Forbidden,
            redistribution: UsePermission::Forbidden,
            attribution_required,
        };
        policy.validate_with_rights(&self.statements)?;
        Ok(policy)
    }
}

/// Exact caller snapshot. `canonical_text` is the already-canonical inert text
/// returned by Attachment, never raw source bytes or renderer markup.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttachmentTextMaterializationRequest {
    pub schema: String,
    pub root_sha256: String,
    pub graph_sha256: String,
    pub graph_locator: AttachmentGraphLocator,
    pub artifact_id: AttachmentArtifactId,
    pub processor: AttachmentProcessorBinding,
    pub canonical_text: String,
    pub canonical_text_bytes: u64,
    pub canonical_text_sha256: String,
    pub title: String,
    pub rights: ConfirmedRights,
}

impl AttachmentTextMaterializationRequest {
    pub fn exact(
        bundle: &AttachmentBundle,
        receipt: &AttachmentReceipt,
        artifact_id: AttachmentArtifactId,
        title: String,
        rights: ConfirmedRights,
    ) -> Result<Self, BridgeError> {
        bundle.validate()?;
        receipt.validate_against(bundle, None)?;
        validate_title(&title)?;
        rights.private_use_policy()?;
        let artifact = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == artifact_id)
            .ok_or(BridgeError::IdentityMismatch(IdentityField::ArtifactId))?;
        let ArtifactPayload::Text { text, .. } = &artifact.payload else {
            return Err(BridgeError::UnsupportedArtifact);
        };
        Ok(Self {
            schema: ATTACHMENT_TEXT_MATERIALIZATION_SCHEMA.to_string(),
            root_sha256: bundle.graph.root.0.clone(),
            graph_sha256: attachment_graph_sha256(&bundle.graph)?,
            graph_locator: AttachmentGraphLocator::derive(&bundle.graph, &artifact.source)?,
            artifact_id,
            processor: AttachmentProcessorBinding::from(&artifact.processor),
            canonical_text: text.clone(),
            canonical_text_bytes: u64::try_from(text.len())
                .map_err(|_| BridgeError::IntegerOverflow)?,
            canonical_text_sha256: canonical_text_sha256(text),
            title,
            rights,
        })
    }
}

/// Source identity retained in the bridge receipt and reproduced in every
/// managed search hit's provenance metadata and lineage locator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttachmentTextBinding {
    pub root_sha256: String,
    pub graph_sha256: String,
    pub graph_locator: AttachmentGraphLocator,
    pub artifact_id: AttachmentArtifactId,
    pub processor: AttachmentProcessorBinding,
    pub canonical_text_bytes: u64,
    pub canonical_text_sha256: String,
    pub title: String,
    pub rights_sha256: String,
    pub rights_confirmed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MaterializedAttachmentText {
    pub schema: String,
    pub binding_sha256: String,
    pub binding: AttachmentTextBinding,
    pub managed: ManagedDocumentsReceipt,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityField {
    RootSha256,
    GraphSha256,
    GraphLocator,
    ArtifactId,
    Processor,
    PolicyFingerprint,
    CanonicalText,
    CanonicalTextBytes,
    CanonicalTextSha256,
    Rights,
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("Attachment contract rejected the source: {0}")]
    Attachment(#[from] AttachmentContractError),
    #[error("Information contract rejected the managed release: {0}")]
    Information(#[from] InformationContractError),
    #[error("Information store rejected the managed release: {0}")]
    Store(#[from] StoreError),
    #[error("attachment materialization identity mismatch: {0:?}")]
    IdentityMismatch(IdentityField),
    #[error("the selected Attachment artifact is not canonical text")]
    UnsupportedArtifact,
    #[error("attachment text materialization rights were not explicitly confirmed")]
    RightsNotConfirmed,
    #[error("attachment text materialization rights do not permit a private local representation")]
    RightsDenied,
    #[error("invalid attachment text materialization request: {0}")]
    InvalidRequest(&'static str),
    #[error("attachment text materialization accounting overflowed")]
    IntegerOverflow,
    #[error("attachment text materialization could not be encoded deterministically: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Validate every Attachment and consent fact before asking Information to
/// create any staging directory. Exact retries return the original immutable
/// receipt; conflicting content under the same binding cannot be activated.
pub fn materialize_attachment_text(
    store: &ManagedStore,
    bundle: &AttachmentBundle,
    receipt: &AttachmentReceipt,
    request: &AttachmentTextMaterializationRequest,
) -> Result<MaterializedAttachmentText, BridgeError> {
    let prepared = prepare_materialization(bundle, receipt, request)?;
    let managed = store.materialize_documents(&prepared.documents)?;
    Ok(MaterializedAttachmentText {
        schema: ATTACHMENT_TEXT_MATERIALIZATION_RECEIPT_SCHEMA.to_string(),
        binding_sha256: prepared.binding_sha256,
        binding: prepared.binding,
        managed,
    })
}

fn canonical_text_sha256(text: &str) -> String {
    sha256(text.as_bytes())
}

fn attachment_graph_sha256(graph: &AttachmentGraph) -> Result<String, BridgeError> {
    graph.validate()?;
    Ok(sha256(&serde_json::to_vec(graph)?))
}

fn rights_sha256(statements: &[RightsStatement]) -> Result<String, BridgeError> {
    Ok(sha256(&serde_json::to_vec(statements)?))
}

struct PreparedMaterialization {
    binding_sha256: String,
    binding: AttachmentTextBinding,
    documents: ManagedDocumentsV1,
}

fn prepare_materialization(
    bundle: &AttachmentBundle,
    receipt: &AttachmentReceipt,
    request: &AttachmentTextMaterializationRequest,
) -> Result<PreparedMaterialization, BridgeError> {
    if request.schema != ATTACHMENT_TEXT_MATERIALIZATION_SCHEMA {
        return Err(BridgeError::InvalidRequest("schema is not supported"));
    }
    bundle.validate()?;
    receipt.validate_against(bundle, None)?;
    if !canonical_sha256(&request.root_sha256)
        || request.root_sha256 != bundle.graph.root.0
        || request.root_sha256 != receipt.root_sha256
    {
        return Err(BridgeError::IdentityMismatch(IdentityField::RootSha256));
    }
    let observed_graph_sha256 = attachment_graph_sha256(&bundle.graph)?;
    if !canonical_sha256(&request.graph_sha256) || request.graph_sha256 != observed_graph_sha256 {
        return Err(BridgeError::IdentityMismatch(IdentityField::GraphSha256));
    }
    let artifact = bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == request.artifact_id)
        .ok_or(BridgeError::IdentityMismatch(IdentityField::ArtifactId))?;
    request
        .graph_locator
        .validate_against(&bundle.graph, &artifact.source)?;
    validate_processor(receipt, artifact, &request.processor)?;
    let ArtifactPayload::Text { text, .. } = &artifact.payload else {
        return Err(BridgeError::UnsupportedArtifact);
    };
    validate_text(request, text)?;
    validate_title(&request.title)?;
    let use_policy = request.rights.private_use_policy()?;

    let binding = AttachmentTextBinding {
        root_sha256: request.root_sha256.clone(),
        graph_sha256: request.graph_sha256.clone(),
        graph_locator: request.graph_locator.clone(),
        artifact_id: request.artifact_id.clone(),
        processor: request.processor.clone(),
        canonical_text_bytes: request.canonical_text_bytes,
        canonical_text_sha256: request.canonical_text_sha256.clone(),
        title: request.title.clone(),
        rights_sha256: request.rights.rights_sha256.clone(),
        rights_confirmed_at: request.rights.confirmed_at,
    };
    let binding_sha256 = sha256(&serde_json::to_vec(&binding)?);
    let documents = build_documents(bundle, request, &binding, &binding_sha256, use_policy)?;
    Ok(PreparedMaterialization {
        binding_sha256,
        binding,
        documents,
    })
}

fn validate_processor(
    receipt: &AttachmentReceipt,
    artifact: &CanonicalArtifact,
    claimed: &AttachmentProcessorBinding,
) -> Result<(), BridgeError> {
    if claimed.name != artifact.processor.name || claimed.version != artifact.processor.version {
        return Err(BridgeError::IdentityMismatch(IdentityField::Processor));
    }
    if claimed.policy_fingerprint != artifact.processor.policy_fingerprint
        || claimed.policy_fingerprint != receipt.policy_fingerprint
    {
        return Err(BridgeError::IdentityMismatch(
            IdentityField::PolicyFingerprint,
        ));
    }
    if receipt.processor_versions.get(&claimed.name) != Some(&claimed.version) {
        return Err(BridgeError::IdentityMismatch(IdentityField::Processor));
    }
    Ok(())
}

fn validate_text(
    request: &AttachmentTextMaterializationRequest,
    artifact_text: &str,
) -> Result<(), BridgeError> {
    if request.canonical_text.is_empty() || request.canonical_text.len() > MAX_ATTACHMENT_TEXT_BYTES
    {
        return Err(BridgeError::InvalidRequest(
            "canonical text is empty or exceeds the one MiB bridge limit",
        ));
    }
    if request.canonical_text != artifact_text {
        return Err(BridgeError::IdentityMismatch(IdentityField::CanonicalText));
    }
    let observed_bytes =
        u64::try_from(artifact_text.len()).map_err(|_| BridgeError::IntegerOverflow)?;
    if request.canonical_text_bytes != observed_bytes {
        return Err(BridgeError::IdentityMismatch(
            IdentityField::CanonicalTextBytes,
        ));
    }
    if !canonical_sha256(&request.canonical_text_sha256)
        || request.canonical_text_sha256 != canonical_text_sha256(artifact_text)
    {
        return Err(BridgeError::IdentityMismatch(
            IdentityField::CanonicalTextSha256,
        ));
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<(), BridgeError> {
    if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(BridgeError::InvalidRequest(
            "title is empty or exceeds 8192 bytes",
        ));
    }
    Ok(())
}

fn build_documents(
    bundle: &AttachmentBundle,
    request: &AttachmentTextMaterializationRequest,
    binding: &AttachmentTextBinding,
    binding_sha256: &str,
    use_policy: UsePolicy,
) -> Result<ManagedDocumentsV1, BridgeError> {
    let root = bundle
        .graph
        .objects
        .iter()
        .find(|object| object.id == bundle.graph.root)
        .ok_or(BridgeError::IdentityMismatch(IdentityField::RootSha256))?;
    let source_uri = format!("urn:sha256:{}", binding.root_sha256);
    let source_artifact_id =
        InformationArtifactId::parse(format!("attachment-root:{}", binding.root_sha256))?;
    let locator = attachment_locator(binding);
    let transformation = format!(
        "{} via {}@{} under {}",
        ATTACHMENT_TEXT_TRANSFORMATION,
        binding.processor.name,
        binding.processor.version,
        binding.processor.policy_fingerprint
    );
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "attachment_root_sha256".to_string(),
        json!(binding.root_sha256),
    );
    metadata.insert(
        "attachment_graph_sha256".to_string(),
        json!(binding.graph_sha256),
    );
    metadata.insert(
        "attachment_graph_edge_indexes".to_string(),
        json!(binding.graph_locator.edge_indexes),
    );
    metadata.insert(
        "attachment_source_object_sha256".to_string(),
        json!(binding.graph_locator.source_object_sha256),
    );
    metadata.insert(
        "attachment_artifact_id".to_string(),
        json!(binding.artifact_id.0),
    );
    metadata.insert(
        "attachment_processor".to_string(),
        json!({
            "name": binding.processor.name,
            "version": binding.processor.version,
            "policy_fingerprint": binding.processor.policy_fingerprint,
        }),
    );
    metadata.insert(
        "attachment_canonical_text_bytes".to_string(),
        json!(binding.canonical_text_bytes),
    );
    metadata.insert(
        "attachment_canonical_text_sha256".to_string(),
        json!(binding.canonical_text_sha256),
    );
    metadata.insert(
        "confirmed_rights_sha256".to_string(),
        json!(binding.rights_sha256),
    );
    metadata.insert("binding_sha256".to_string(), json!(binding_sha256));

    let segment = ManagedTextSegment {
        segment_id: ManagedSegmentId::parse(format!("attachment-text:{binding_sha256}"))?,
        ordinal: 0,
        text: request.canonical_text.clone(),
        text_sha256: managed_text_sha256(&request.canonical_text),
        locator: locator.clone(),
    };
    let document = ManagedDocument {
        document_id: ManagedDocumentId::parse(format!(
            "attachment-artifact:{}",
            binding.artifact_id.0
        ))?,
        title: binding.title.clone(),
        creator: None,
        source_uri: Some(source_uri.clone()),
        locator: locator.clone(),
        immutable: true,
        visibility: ManagedDocumentVisibility::Private,
        lineage: vec![ManagedDocumentLineage {
            source_artifact_id: source_artifact_id.clone(),
            source_record_id: binding.artifact_id.0.clone(),
            source_record_sha256: binding.canonical_text_sha256.clone(),
            source_locator: locator,
            transformation,
        }],
        rights: request.rights.statements.clone(),
        use_policy,
        segments: vec![segment],
    };
    let mut documents = ManagedDocumentsV1 {
        schema: MANAGED_DOCUMENTS_SCHEMA.to_string(),
        materialization_id: ManagedMaterializationId::parse(format!(
            "attachment-text:{binding_sha256}"
        ))?,
        resource_id: ResourceId::parse(format!("attachment-root:{}", binding.root_sha256))?,
        release_id: ReleaseId::parse(format!("attachment-release:{binding_sha256}"))?,
        representation_id: RepresentationId::parse(format!("attachment-text:{binding_sha256}"))?,
        created_at: request.rights.confirmed_at,
        provenance: Provenance {
            publisher: "attachment-native-kit".to_string(),
            source_uri: source_uri.clone(),
            upstream_record_id: Some(binding.artifact_id.0.clone()),
            source_inputs: vec![
                format!("root-sha256:{}", binding.root_sha256),
                format!("graph-sha256:{}", binding.graph_sha256),
                format!("artifact-id:{}", binding.artifact_id.0),
                format!("canonical-text-sha256:{}", binding.canonical_text_sha256),
                format!("rights-sha256:{}", binding.rights_sha256),
            ],
            transformation: Some(ATTACHMENT_TEXT_TRANSFORMATION.to_string()),
            metadata,
        },
        source_artifacts: vec![ManagedSourceArtifact {
            artifact_id: source_artifact_id,
            source_uri,
            bytes: root.byte_len,
            sha256: binding.root_sha256.clone(),
            immutable: true,
        }],
        documents: vec![document],
        content_sha256: "0".repeat(64),
    };
    documents.refresh_content_sha256()?;
    documents.validate()?;
    Ok(documents)
}

fn attachment_locator(binding: &AttachmentTextBinding) -> EvidenceLocator {
    let edges = binding
        .graph_locator
        .edge_indexes
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    EvidenceLocator::Record {
        collection: Some("attachment_native.graph.v1".to_string()),
        key: format!(
            "root={};graph={};edges={edges};source={};artifact={}",
            binding.root_sha256,
            binding.graph_sha256,
            binding.graph_locator.source_object_sha256,
            binding.artifact_id.0,
        ),
    }
}

fn canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
