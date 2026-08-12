mod support;

use jsonschema::{Retrieve, Uri, Validator};
use platform_vertical_fixtures_v0::{
    ObservationEnvelopeV0, VerticalFixtureLockV0, VerticalFixtureManifestV0, VerticalIdV0,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;

const MANIFEST_SCHEMA_ID: &str =
    "https://schemas.delysis.dev/w1/v0/vertical-fixture-manifest.schema.json";
const OBSERVATION_SCHEMA_ID: &str =
    "https://schemas.delysis.dev/w1/v0/vertical-observation.schema.json";
const LOCK_SCHEMA_ID: &str = "https://schemas.delysis.dev/w1/v0/vertical-lock.schema.json";
const EVIDENCE_SCHEMA_ID: &str = "https://schemas.delysis.dev/w1/v0/evidence.schema.json";

#[derive(Clone)]
struct InMemoryRetriever {
    resources: Arc<BTreeMap<String, Value>>,
}

impl Retrieve for InMemoryRetriever {
    fn retrieve(&self, uri: &Uri<String>) -> Result<Value, Box<dyn Error + Send + Sync>> {
        self.resources
            .get(uri.as_str())
            .cloned()
            .ok_or_else(|| format!("schema retrieval is not permitted for {uri}").into())
    }
}

fn schemas() -> BTreeMap<&'static str, Value> {
    [
        (
            MANIFEST_SCHEMA_ID,
            include_str!("../../../schemas/v0/vertical-fixture-manifest.schema.json"),
        ),
        (
            OBSERVATION_SCHEMA_ID,
            include_str!("../../../schemas/v0/vertical-observation.schema.json"),
        ),
        (
            LOCK_SCHEMA_ID,
            include_str!("../../../schemas/v0/vertical-lock.schema.json"),
        ),
        (
            EVIDENCE_SCHEMA_ID,
            include_str!("../../../schemas/v0/evidence.schema.json"),
        ),
    ]
    .into_iter()
    .map(|(id, source)| {
        (
            id,
            serde_json::from_str(source)
                .unwrap_or_else(|error| panic!("{id} must be JSON: {error}")),
        )
    })
    .collect()
}

fn validator(schema_id: &str) -> Validator {
    let schemas = schemas();
    let schema = schemas
        .get(schema_id)
        .unwrap_or_else(|| panic!("schema {schema_id} must exist"));
    let resources = schemas
        .iter()
        .map(|(id, schema)| ((*id).to_owned(), schema.clone()))
        .collect();
    jsonschema::options()
        .with_retriever(InMemoryRetriever {
            resources: Arc::new(resources),
        })
        .build(schema)
        .unwrap_or_else(|error| panic!("schema {schema_id} must compile offline: {error}"))
}

#[test]
fn schemas_compile_offline_and_accept_typed_examples() {
    let (manifest, _) = support::manifest_and_projection(VerticalIdV0::MomChatCancelRetry);
    let observation = support::observation(VerticalIdV0::MomChatCancelRetry);
    let (lock, _) = support::complete_lock();

    for (schema_id, value) in [
        (
            MANIFEST_SCHEMA_ID,
            serde_json::to_value(manifest).expect("manifest JSON"),
        ),
        (
            OBSERVATION_SCHEMA_ID,
            serde_json::to_value(observation).expect("observation JSON"),
        ),
        (
            LOCK_SCHEMA_ID,
            serde_json::to_value(lock).expect("lock JSON"),
        ),
    ] {
        assert!(
            validator(schema_id).is_valid(&value),
            "typed example must conform to {schema_id}"
        );
    }
}

#[test]
fn schemas_and_serde_reject_authority_inflation() {
    let (manifest, _) = support::manifest_and_projection(VerticalIdV0::MomAttachment);
    let mut manifest_json = serde_json::to_value(manifest).expect("manifest JSON");
    manifest_json["cases"][0]["replay"][0]["schedule"] = serde_json::json!("weekly");
    assert!(!validator(MANIFEST_SCHEMA_ID).is_valid(&manifest_json));
    assert!(serde_json::from_value::<VerticalFixtureManifestV0>(manifest_json).is_err());

    let observation = support::observation(VerticalIdV0::CurrentExactGemma);
    let mut observation_json = serde_json::to_value(observation).expect("observation JSON");
    observation_json["authority"] = serde_json::json!(true);
    assert!(!validator(OBSERVATION_SCHEMA_ID).is_valid(&observation_json));
    assert!(serde_json::from_value::<ObservationEnvelopeV0>(observation_json).is_err());

    let (lock, _) = support::complete_lock();
    let mut lock_json = serde_json::to_value(lock).expect("lock JSON");
    lock_json["accepted"] = serde_json::json!(true);
    assert!(!validator(LOCK_SCHEMA_ID).is_valid(&lock_json));
    assert!(serde_json::from_value::<VerticalFixtureLockV0>(lock_json).is_err());
}

#[test]
fn manifest_schema_rejects_wrong_section_16_class() {
    let (manifest, _) = support::manifest_and_projection(VerticalIdV0::AppleInstalledVoice);
    let mut json = serde_json::to_value(manifest).expect("manifest JSON");
    json["class"] = serde_json::json!("model_free");
    assert!(!validator(MANIFEST_SCHEMA_ID).is_valid(&json));
}

#[test]
fn schemas_require_real_prerequisites_state_identity_and_fail_closed_facts() {
    for vertical_id in [
        VerticalIdV0::CurrentExactQwen,
        VerticalIdV0::CurrentExactGemma,
        VerticalIdV0::CurrentParakeetModelAudio,
        VerticalIdV0::AppleInstalledVoice,
    ] {
        let (manifest, _) = support::manifest_and_projection(vertical_id);
        let mut json = serde_json::to_value(manifest).expect("real manifest JSON");
        json["cases"][0]["prerequisites"] = serde_json::json!([]);
        assert!(
            !validator(MANIFEST_SCHEMA_ID).is_valid(&json),
            "{vertical_id:?} schema must require exact prerequisites"
        );
    }

    let (state, _) = support::manifest_and_projection(VerticalIdV0::FteLegacyDatabase);
    let mut state_json = serde_json::to_value(state).expect("state manifest JSON");
    state_json["cases"][0]["state_identities"] = serde_json::json!([]);
    assert!(!validator(MANIFEST_SCHEMA_ID).is_valid(&state_json));

    let observation = support::observation(VerticalIdV0::SpeechPeerCancellation);
    let mut observation_json = serde_json::to_value(observation).expect("observation JSON");
    observation_json["projection"]["fail_closed_facts"] = serde_json::json!([]);
    assert!(!validator(OBSERVATION_SCHEMA_ID).is_valid(&observation_json));
}

#[test]
fn observation_schema_requires_prerequisite_correlation_and_state_schema_fields() {
    let observation = support::observation(VerticalIdV0::MomChatCancelRetry);
    let json = serde_json::to_value(observation).expect("observation JSON");

    for pointer in [
        "/observed_prerequisites",
        "/projection/ordered_events/0/correlation_id",
        "/projection/lifecycle/0/correlation_id",
        "/projection/durable_state/0/schema_id",
    ] {
        let mut changed = json.clone();
        let (parent, field) = pointer.rsplit_once('/').expect("object field pointer");
        changed
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .expect("pointer parent must be an object")
            .remove(field);
        assert!(
            !validator(OBSERVATION_SCHEMA_ID).is_valid(&changed),
            "schema must require {pointer}"
        );
    }
}
