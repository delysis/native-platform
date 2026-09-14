use attachment_native_host::{AttachmentHost, AttachmentHostConfig, ProvidedAttachment};
use attachment_native_types::{ArtifactId, ArtifactPayload, AttachmentBundle, AttachmentReceipt};
use chrono::{DateTime, Utc};
use flate2::Compression;
use flate2::write::GzEncoder;
use information_native_attachment_bridge::{
    AttachmentTextMaterializationRequest, ConfirmedRights, materialize_attachment_text,
};
use information_native_store::ManagedStore;
use information_native_types::{
    MANAGED_DOCUMENTS_SEARCH_SCHEMA, ManagedDocumentsSearchRequest, RedistributionPolicy,
    RightsStatement,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::tempdir;

const SOURCE_BYTES: &[u8] =
    b"Bridge provenance survives source deletion.\nExact canonical text remains searchable.\n";

#[derive(Debug, PartialEq, Eq)]
struct SourceIdentity {
    bytes: Vec<u8>,
    byte_len: u64,
    sha256: String,
}

fn source_identity(path: &Path) -> Result<SourceIdentity, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    Ok(SourceIdentity {
        byte_len: fs::metadata(path)?.len(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
    })
}

fn sha256_json<T: serde::Serialize>(value: &T) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn nested_archive_bytes() -> Result<Vec<u8>, Box<dyn Error>> {
    let mut inner = GzEncoder::new(Vec::new(), Compression::default());
    inner.write_all(SOURCE_BYTES)?;
    let inner = inner.finish()?;
    let mut outer = GzEncoder::new(Vec::new(), Compression::default());
    outer.write_all(&inner)?;
    Ok(outer.finish()?)
}

fn canonical_attachment(
    source_bytes: &[u8],
) -> Result<(AttachmentBundle, AttachmentReceipt, ArtifactId), Box<dyn Error>> {
    let host = AttachmentHost::new(AttachmentHostConfig::default())?;
    let canonicalized = host.inspect_and_canonicalize(ProvidedAttachment::from_bytes(
        "bridge-source.txt.gz.gz",
        Some("application/gzip".to_string()),
        source_bytes.to_vec(),
    ))?;
    let artifact_id = canonicalized
        .bundle
        .artifacts
        .iter()
        .find(|artifact| matches!(artifact.payload, ArtifactPayload::Text { .. }))
        .ok_or("fixture produced no canonical text artifact")?
        .id
        .clone();
    Ok((canonicalized.bundle, canonicalized.receipt, artifact_id))
}

fn confirmed_rights() -> Result<ConfirmedRights, Box<dyn Error>> {
    let confirmed_at = "2026-08-24T12:00:00Z".parse::<DateTime<Utc>>()?;
    Ok(ConfirmedRights::private(
        confirmed_at,
        vec![RightsStatement {
            scope: "caller-confirmed local private use".to_string(),
            expression: "The caller confirmed authority to create a private local search copy."
                .to_string(),
            license_url: None,
            license_text_sha256: None,
            attribution: None,
            obligations: Vec::new(),
            redistribution: RedistributionPolicy::PrivateUseOnly,
        }],
    )?)
}

fn exact_request(
    bundle: &AttachmentBundle,
    receipt: &AttachmentReceipt,
    artifact_id: ArtifactId,
) -> Result<AttachmentTextMaterializationRequest, Box<dyn Error>> {
    Ok(AttachmentTextMaterializationRequest::exact(
        bundle,
        receipt,
        artifact_id,
        "Exact Attachment Bridge Fixture".to_string(),
        confirmed_rights()?,
    )?)
}

fn assert_no_publish(store: &ManagedStore) -> Result<(), Box<dyn Error>> {
    for directory in ["active", "staging"] {
        let path = store.root().join("managed-documents-v1").join(directory);
        if path.exists() {
            assert_eq!(fs::read_dir(path)?.count(), 0);
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn exact_materialization_is_idempotent_source_preserving_and_reopenable()
-> Result<(), Box<dyn Error>> {
    let temporary = tempdir()?;
    let source_path = temporary.path().join("caller-owned-source.txt.gz.gz");
    let source_bytes = nested_archive_bytes()?;
    fs::write(&source_path, &source_bytes)?;
    let source_before = source_identity(&source_path)?;
    let store_root = temporary.path().join("information-managed");
    let store = ManagedStore::open(&store_root)?;
    let (bundle, receipt, artifact_id) = canonical_attachment(&source_bytes)?;
    let graph_before = serde_json::to_vec(&bundle.graph)?;
    let artifacts_before = serde_json::to_vec(&bundle.artifacts)?;
    let blobs_before = bundle.blobs.clone();
    let request = exact_request(&bundle, &receipt, artifact_id)?;

    assert_eq!(source_before.sha256, request.root_sha256);
    assert_eq!(request.graph_locator.edge_indexes.len(), 2);
    assert_ne!(
        request.graph_locator.source_object_sha256,
        request.graph_locator.root_sha256
    );
    let first = materialize_attachment_text(&store, &bundle, &receipt, &request)?;
    let retry = materialize_attachment_text(&store, &bundle, &receipt, &request)?;
    assert_eq!(retry, first, "an exact retry must reuse the active release");
    assert_eq!(source_identity(&source_path)?, source_before);
    assert_eq!(serde_json::to_vec(&bundle.graph)?, graph_before);
    assert_eq!(serde_json::to_vec(&bundle.artifacts)?, artifacts_before);
    assert_eq!(bundle.blobs, blobs_before);

    let search_request = ManagedDocumentsSearchRequest {
        schema: MANAGED_DOCUMENTS_SEARCH_SCHEMA.to_string(),
        materialization_id: first.managed.materialization_id.clone(),
        content_sha256: first.managed.content_sha256.clone(),
        query: "Bridge provenance source deletion".to_string(),
        max_hits: 10,
        max_snippet_chars: 512,
    };
    let before_deletion = store.search_managed_documents(&search_request)?;
    assert_eq!(before_deletion.hits.len(), 1);
    let hit = &before_deletion.hits[0];
    assert_eq!(hit.title, request.title);
    assert_eq!(hit.source_artifacts.len(), 1);
    assert_eq!(hit.source_artifacts[0].sha256, request.root_sha256);
    assert_eq!(
        hit.source_artifacts[0].source_uri,
        format!("urn:sha256:{}", request.root_sha256)
    );
    assert_eq!(hit.lineage.len(), 1);
    assert_eq!(hit.lineage[0].source_record_id, request.artifact_id.0);
    assert_eq!(
        hit.lineage[0].source_record_sha256,
        request.canonical_text_sha256
    );
    assert_eq!(hit.document_locator, hit.segment_locator);
    assert_eq!(
        hit.provenance.metadata["attachment_graph_sha256"],
        request.graph_sha256
    );
    assert_eq!(
        hit.provenance.metadata["attachment_canonical_text_sha256"],
        format!("{:x}", Sha256::digest(request.canonical_text.as_bytes()))
    );
    assert_eq!(
        hit.provenance.metadata["confirmed_rights_sha256"],
        request.rights.rights_sha256
    );

    drop(store);
    drop(bundle);
    drop(receipt);
    fs::remove_file(&source_path)?;
    assert!(!source_path.exists());

    let reopened = ManagedStore::open(&store_root)?;
    let after_deletion = reopened.search_managed_documents(&search_request)?;
    assert_eq!(after_deletion, before_deletion);
    Ok(())
}

#[cfg(unix)]
#[test]
fn every_bound_identity_and_rights_fact_fails_before_publication_when_wrong()
-> Result<(), Box<dyn Error>> {
    let temporary = tempdir()?;
    let store = ManagedStore::open(temporary.path().join("managed"))?;
    let source_bytes = nested_archive_bytes()?;
    let (bundle, receipt, artifact_id) = canonical_attachment(&source_bytes)?;
    let exact = exact_request(&bundle, &receipt, artifact_id)?;

    let mut cases = Vec::new();

    let mut wrong = exact.clone();
    wrong.root_sha256 = "0".repeat(64);
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.graph_sha256 = "1".repeat(64);
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.graph_locator.edge_indexes.push(0);
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.artifact_id = ArtifactId("missing-artifact".to_string());
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.processor.name.push_str("-wrong");
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.processor.policy_fingerprint = "sha256:wrong-policy".to_string();
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.canonical_text.push_str("tampered");
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.canonical_text_bytes += 1;
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.canonical_text_sha256 = "2".repeat(64);
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.rights.confirmed = false;
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.rights.rights_sha256 = "3".repeat(64);
    cases.push(wrong);

    let mut wrong = exact.clone();
    wrong.rights.statements[0].redistribution = RedistributionPolicy::Forbidden;
    wrong.rights.rights_sha256 = sha256_json(&wrong.rights.statements)?;
    cases.push(wrong);

    for request in cases {
        assert!(
            materialize_attachment_text(&store, &bundle, &receipt, &request).is_err(),
            "wrong bridge identity must fail closed"
        );
        assert_no_publish(&store)?;
    }
    Ok(())
}

#[test]
fn serialized_request_is_an_exact_path_free_contract() -> Result<(), Box<dyn Error>> {
    let source_bytes = nested_archive_bytes()?;
    let (bundle, receipt, artifact_id) = canonical_attachment(&source_bytes)?;
    let request = exact_request(&bundle, &receipt, artifact_id)?;
    let encoded = serde_json::to_value(request)?;
    let keys = encoded
        .as_object()
        .ok_or("request must serialize as an object")?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        BTreeSet::from([
            "artifact_id",
            "canonical_text",
            "canonical_text_bytes",
            "canonical_text_sha256",
            "graph_locator",
            "graph_sha256",
            "processor",
            "rights",
            "root_sha256",
            "schema",
            "title",
        ])
    );
    let serialized = serde_json::to_string(&encoded)?;
    for forbidden in ["source_path", "archive_path", "managed_path", "source_url"] {
        assert!(!serialized.contains(forbidden));
    }
    Ok(())
}
