use jsonschema::{Retrieve, Uri, Validator};
use platform_contracts_v0::TerminalV0;
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;

const SCHEMAS: [(&str, &str); 6] = [
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

const FIXTURES: [(&str, &str, &str); 12] = [
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
    let mut durability_without_file_sync = fixture("publication-durability-unknown.json");
    durability_without_file_sync["receipt"]["file_synced"] = Value::Bool(false);
    assert_rejected(
        &publication,
        &durability_without_file_sync,
        "visible publication without file synchronization",
    );
    let mut durability_with_wrong_error = fixture("publication-durability-unknown.json");
    durability_with_wrong_error["error"]["class"] = Value::String("storage".to_owned());
    assert_rejected(
        &publication,
        &durability_with_wrong_error,
        "durability-unknown outcome with non-publication error",
    );

    let shutdown = schemas.validator("shutdown.schema.json");
    let mut closed_with_active_work = fixture("shutdown-success.json");
    closed_with_active_work["active_operations"] = Value::from(1);
    assert_rejected(
        &shutdown,
        &closed_with_active_work,
        "closed supervisor with active work",
    );
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
    let terminal: TerminalV0 =
        serde_json::from_value(mismatched).expect("mismatched identity remains well-typed JSON");
    assert!(
        terminal.validate().is_err(),
        "Rust semantic validation must enforce cross-field identity equality"
    );
}
