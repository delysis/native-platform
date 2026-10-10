//! One compiled aggregate over the complete admitted library, independent of FTS.
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Clone, Copy, Debug)]
pub struct DocumentCountRequest {
    pub purpose: RetrievalPurpose,
    pub timeout_ms: u64,
}

/// Counts document records, including records without indexed passages. This is
/// metadata, not a content excerpt or a claim that every document was prepared.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentCount {
    pub value: u64,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub source_fingerprint: String,
    #[serde(deserialize_with = "deserialize_source_identity")]
    pub source_identity: SourceIdentity,
    pub provenance: Provenance,
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
}

fn deserialize_source_identity<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<SourceIdentity, D::Error> {
    // SourceIdentity has a u128 timestamp. Serde's untagged-enum buffer cannot
    // decode u128 directly; the JSON value bridge preserves the integer bytes.
    serde_json::from_value(JsonValue::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

impl AlexandriaBackend {
    pub fn count_documents(
        &self,
        request: DocumentCountRequest,
    ) -> Result<DocumentCount, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        if request.timeout_ms == 0 || request.timeout_ms > 30_000 {
            return Err(input_error(
                "invalid_count_budget",
                "Count requires a 1–30000 ms deadline.",
            ));
        }
        self.with_connection(request.timeout_ms, |connection, fingerprint| {
            let value: i64 = connection
                .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
                .map_err(map_sqlite_error)?;
            let value = u64::try_from(value).map_err(|_| {
                integrity_error(
                    "invalid_document_count",
                    "SQLite returned a negative document count.",
                )
            })?;
            let source_uri = Url::from_file_path(&self.path)
                .map_err(|()| {
                    input_error("invalid_count_source", "Library path is not a file URI.")
                })?
                .to_string();
            // Live access may intentionally rebind between operations; describe
            // the file in THIS checked transaction, not the initial admission.
            let mut source_identity = FileIdentity::observe(&self.path)?.source_identity();
            source_identity.sha256 = self
                .immutable_source_sha256
                .as_ref()
                .map(|hash| format!("sha256:{hash}"));
            Ok(DocumentCount {
                value,
                resource_id: self.descriptor.resource_id.clone(),
                release_id: self.descriptor.release_id.clone(),
                representation_id: self.descriptor.representation_id.clone(),
                source_fingerprint: fingerprint.into(),
                source_identity,
                provenance: Provenance {
                    publisher: self.publisher.clone(),
                    source_uri,
                    upstream_record_id: None,
                    source_inputs: vec![format!("sqlite-source-{fingerprint}")],
                    transformation: Some("SELECT COUNT(*) FROM documents".into()),
                    metadata: BTreeMap::from([
                        ("profile".into(), json!(PROFILE_NAME)),
                        ("operation".into(), json!("count")),
                        ("unit".into(), json!("document_records")),
                        ("scope".into(), json!("entire_library")),
                        (
                            "source_fingerprint_kind".into(),
                            json!(source_fingerprint_kind(fingerprint)),
                        ),
                    ]),
                },
                rights: self.rights.clone(),
                use_policy: self.use_policy,
            })
        })
    }
}
