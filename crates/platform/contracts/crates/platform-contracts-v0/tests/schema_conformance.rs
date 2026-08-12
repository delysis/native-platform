use jsonschema::{Retrieve, Uri, Validator};
use platform_contracts_v0::{ClosedSummaryV0, TerminalV0};
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;

const SCHEMAS: [(&str, &str); 7] = [
    (
        "capability.schema.json",
        include_str!("../../../schemas/v0/capability.schema.json"),
    ),
    (
        "evidence.schema.json",
        include_str!("../../../schemas/v0/evidence.schema.json"),
    ),
    (
        "publication.schema.json",
        include_str!("../../../schemas/v0/publication.schema.json"),
    ),
    (
        "privacy-policy.schema.json",
        include_str!("../../../schemas/v0/privacy-policy.schema.json"),
    ),
    (
        "service-error.schema.json",
        include_str!("../../../schemas/v0/service-error.schema.json"),
    ),
    (
        "shutdown.schema.json",
        include_str!("../../../schemas/v0/shutdown.schema.json"),
    ),
    (
        "terminal.schema.json",
        include_str!("../../../schemas/v0/terminal.schema.json"),
    ),
];

const FIXTURES: [(&str, &str, &str); 15] = [
    (
        "capability-known.json",
        "capability.schema.json",
        include_str!("../../../fixtures/v0/capability-known.json"),
    ),
    (
        "capability-unknown.json",
        "capability.schema.json",
        include_str!("../../../fixtures/v0/capability-unknown.json"),
    ),
    (
        "evidence-operational.json",
        "evidence.schema.json",
        include_str!("../../../fixtures/v0/evidence-operational.json"),
    ),
    (
        "evidence-reproducible.json",
        "evidence.schema.json",
        include_str!("../../../fixtures/v0/evidence-reproducible.json"),
    ),
    (
        "publication-durability-unknown.json",
        "publication.schema.json",
        include_str!("../../../fixtures/v0/publication-durability-unknown.json"),
    ),
    (
        "publication-visible-file-sync-unknown.json",
        "publication.schema.json",
        include_str!("../../../fixtures/v0/publication-visible-file-sync-unknown.json"),
    ),
    (
        "publication-not-published.json",
        "publication.schema.json",
        include_str!("../../../fixtures/v0/publication-not-published.json"),
    ),
    (
        "publication-published.json",
        "publication.schema.json",
        include_str!("../../../fixtures/v0/publication-published.json"),
    ),
    (
        "privacy-policy-hosted-explicit.json",
        "privacy-policy.schema.json",
        include_str!("../../../fixtures/v0/privacy-policy-hosted-explicit.json"),
    ),
    (
        "privacy-policy-local-only.json",
        "privacy-policy.schema.json",
        include_str!("../../../fixtures/v0/privacy-policy-local-only.json"),
    ),
    (
        "service-error.json",
        "service-error.schema.json",
        include_str!("../../../fixtures/v0/service-error.json"),
    ),
    (
        "shutdown-multiple-failures.json",
        "shutdown.schema.json",
        include_str!("../../../fixtures/v0/shutdown-multiple-failures.json"),
    ),
    (
        "shutdown-success.json",
        "shutdown.schema.json",
        include_str!("../../../fixtures/v0/shutdown-success.json"),
    ),
    (
        "terminal-cancelled.json",
        "terminal.schema.json",
        include_str!("../../../fixtures/v0/terminal-cancelled.json"),
    ),
    (
        "terminal-completed.json",
        "terminal.schema.json",
        include_str!("../../../fixtures/v0/terminal-completed.json"),
    ),
];

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

struct SchemaSet {
    documents: BTreeMap<&'static str, Value>,
    retriever: InMemoryRetriever,
}

impl SchemaSet {
    fn load() -> Self {
        let documents = SCHEMAS
            .into_iter()
            .map(|(name, source)| {
                let document: Value = serde_json::from_str(source)
                    .unwrap_or_else(|error| panic!("{name} must be valid JSON: {error}"));
                (name, document)
            })
            .collect::<BTreeMap<_, _>>();

        let service_error = documents
            .get("service-error.schema.json")
            .expect("service-error schema must be loaded")
            .clone();
        let service_error_id = service_error
            .get("$id")
            .and_then(Value::as_str)
            .expect("service-error schema must declare a string $id")
            .to_owned();
        let resources = BTreeMap::from([(service_error_id, service_error)]);

        Self {
            documents,
            retriever: InMemoryRetriever {
                resources: Arc::new(resources),
            },
        }
    }

    fn validator(&self, name: &str) -> Validator {
        let schema = self
            .documents
            .get(name)
            .unwrap_or_else(|| panic!("schema {name} must be loaded"));
        jsonschema::options()
            .with_retriever(self.retriever.clone())
            .build(schema)
            .unwrap_or_else(|error| panic!("schema {name} must compile offline: {error}"))
    }
}

fn fixture(name: &str) -> Value {
    let source = FIXTURES
        .iter()
        .find_map(|(fixture_name, _, source)| (*fixture_name == name).then_some(*source))
        .unwrap_or_else(|| panic!("fixture {name} must be declared"));
    serde_json::from_str(source)
        .unwrap_or_else(|error| panic!("fixture {name} must be valid JSON: {error}"))
}

fn assert_rejected(validator: &Validator, instance: &Value, case: &str) {
    assert!(
        !validator.is_valid(instance),
        "schema must reject negative case: {case}"
    );
}

#[test]
fn all_schemas_compile_with_only_the_registered_relative_reference() {
    let schemas = SchemaSet::load();
    for (name, _) in SCHEMAS {
        let _validator = schemas.validator(name);
    }
}

#[test]
fn every_golden_fixture_conforms_to_its_schema() {
    let schemas = SchemaSet::load();
    for (name, schema_name, source) in FIXTURES {
        let instance = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("fixture {name} must be valid JSON: {error}"));
        let validator = schemas.validator(schema_name);
        assert!(
            validator.is_valid(&instance),
            "fixture {name} must conform to {schema_name}"
        );
    }
}

#[test]
fn schemas_reject_critical_semantic_violations() {
    let schemas = SchemaSet::load();

    let service_error = schemas.validator("service-error.schema.json");
    let mut invalid_error = fixture("service-error.json");
    invalid_error["code"] = Value::String("publication..sync".to_owned());
    assert_rejected(&service_error, &invalid_error, "empty error-code segment");

    let capability = schemas.validator("capability.schema.json");
    let mut unknown_without_remediation = fixture("capability-unknown.json");
    unknown_without_remediation["services"]["speech"][0]["remediation"] = Value::Null;
    assert_rejected(
        &capability,
        &unknown_without_remediation,
        "unknown capability without remediation",
    );

    let evidence = schemas.validator("evidence.schema.json");
    let mut inflated_evidence = fixture("evidence-reproducible.json");
    inflated_evidence["tier"] = Value::String("research_eligible".to_owned());
    assert_rejected(
        &evidence,
        &inflated_evidence,
        "unsupported research-eligible tier",
    );

    let terminal = schemas.validator("terminal.schema.json");
    let mut cancelled_without_operation = fixture("terminal-cancelled.json");
    cancelled_without_operation["error"]["operation_id"] = Value::Null;
    assert_rejected(
        &terminal,
        &cancelled_without_operation,
        "terminal error without operation identity",
    );
    let mut completed_with_error = fixture("terminal-completed.json");
    completed_with_error["error"] = fixture("service-error.json");
    assert_rejected(
        &terminal,
        &completed_with_error,
        "completed terminal carrying an error",
    );

    let publication = schemas.validator("publication.schema.json");
    let durability_without_file_sync = fixture("publication-visible-file-sync-unknown.json");
    assert!(
        publication.is_valid(&durability_without_file_sync),
        "visible exact bytes with failed file sync are a valid durability-unknown outcome"
    );
    let mut durability_with_directory_sync = durability_without_file_sync;
    durability_with_directory_sync["receipt"]["directory_synced"] = Value::Bool(true);
    assert_rejected(
        &publication,
        &durability_with_directory_sync,
        "directory sync cannot be true when file sync is unknown",
    );
    let mut durability_with_wrong_error = fixture("publication-durability-unknown.json");
    durability_with_wrong_error["error"]["class"] = Value::String("storage".to_owned());
    assert_rejected(
        &publication,
        &durability_with_wrong_error,
        "durability-unknown outcome with non-publication error",
    );

    let privacy = schemas.validator("privacy-policy.schema.json");
    let mut unknown_tier = fixture("privacy-policy-hosted-explicit.json");
    unknown_tier["allowed_hosted_data_tiers"] = serde_json::json!(["unknown"]);
    assert_rejected(&privacy, &unknown_tier, "unknown hosted data tier");
    let mut missing_provider = fixture("privacy-policy-hosted-explicit.json");
    missing_provider["allowed_provider_ids"] = serde_json::json!([]);
    assert_rejected(
        &privacy,
        &missing_provider,
        "allow-listed network without a provider",
    );
    let mut local_with_provider = fixture("privacy-policy-local-only.json");
    local_with_provider["allowed_provider_ids"] = serde_json::json!(["provider.cerebras"]);
    assert_rejected(
        &privacy,
        &local_with_provider,
        "local-only policy with a hosted provider",
    );
    let mut credential = fixture("privacy-policy-hosted-explicit.json");
    credential["api_key"] = Value::String("secret".to_owned());
    assert_rejected(
        &privacy,
        &credential,
        "privacy policy carrying credential authority",
    );

    let shutdown = schemas.validator("shutdown.schema.json");
    let mut closed_with_active_work = fixture("shutdown-success.json");
    closed_with_active_work["active_operations"] = Value::from(1);
    assert_rejected(
        &shutdown,
        &closed_with_active_work,
        "closed supervisor with active work",
    );
    let mut empty_resources = fixture("shutdown-success.json");
    empty_resources["resources"] = serde_json::json!([]);
    assert_rejected(
        &shutdown,
        &empty_resources,
        "closed supervisor without resource accounting",
    );
    let mut path_as_resource_id = fixture("shutdown-success.json");
    path_as_resource_id["resources"][0]["resource_id"] =
        Value::String("/private/tmp/worker".to_owned());
    assert_rejected(
        &shutdown,
        &path_as_resource_id,
        "resource identity carrying a filesystem path",
    );
}

#[test]
fn shutdown_cross_record_accounting_remains_rust_semantic_validation() {
    let schemas = SchemaSet::load();
    let validator = schemas.validator("shutdown.schema.json");

    let mut duplicate_failure = fixture("shutdown-multiple-failures.json");
    duplicate_failure["failures"][1]["failure_id"] =
        duplicate_failure["failures"][0]["failure_id"].clone();
    assert!(
        validator.is_valid(&duplicate_failure),
        "JSON Schema cannot express unique object fields across array items"
    );
    assert!(serde_json::from_value::<ClosedSummaryV0>(duplicate_failure).is_err());

    let mut mismatched_aggregate = fixture("shutdown-success.json");
    mismatched_aggregate["joined_workers"] = Value::from(2);
    assert!(
        validator.is_valid(&mismatched_aggregate),
        "JSON Schema cannot sum resource worker counts"
    );
    assert!(serde_json::from_value::<ClosedSummaryV0>(mismatched_aggregate).is_err());

    let mut unknown_resource = fixture("shutdown-multiple-failures.json");
    unknown_resource["failures"][0]["resource_id"] = Value::String("speech.unknown".to_owned());
    assert!(validator.is_valid(&unknown_resource));
    assert!(serde_json::from_value::<ClosedSummaryV0>(unknown_resource).is_err());
}

#[test]
fn terminal_identity_equality_remains_rust_semantic_validation() {
    let schemas = SchemaSet::load();
    let validator = schemas.validator("terminal.schema.json");
    let mut mismatched = fixture("terminal-cancelled.json");
    mismatched["error"]["operation_id"] = Value::String("op-mismatched".to_owned());

    assert!(
        validator.is_valid(&mismatched),
        "JSON Schema cannot compare sibling field values"
    );
    assert!(
        serde_json::from_value::<TerminalV0>(mismatched).is_err(),
        "validated deserialization must enforce cross-field identity equality"
    );
}
