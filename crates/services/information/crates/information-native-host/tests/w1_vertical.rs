#![cfg(feature = "unstable-w1-vertical-tests")]

use chrono::{DateTime, Utc};
use information_native_acquire::{
    AcquireClient, AcquisitionPolicy, ProgressControl, TransferPhase, TransferProgress,
};
use information_native_catalog::{CatalogIndex, PlanRequest};
use information_native_host::{InformationHost, MountInstallationOptions};
use information_native_retrieval::RetrievalRouter;
use information_native_store::{
    ExternalRegistrationRequest, IdentityStrength, ManagedStore, PartialInstallState,
    RegisteredInstallation, RemovalKind, StoreError, StoreSnapshot, TransferSummary,
    capture_source_identity, check_source_identity, reject_nonempty_sqlite_sidecars,
};
use information_native_types::*;
use platform_contracts_fixture_v0::ArtifactIdentityV0;
use platform_vertical_fixtures_v0::{
    ArtifactAvailabilityV0, DurableStateFactV0, EquivalenceProjectionV0, EventFactV0, FactValueV0,
    LifecycleFactV0, ObservationEnvelopeV0, OwnershipFactsV0, StateDispositionV0,
    VERTICAL_OBSERVATION_SCHEMA_V0, VerticalFixtureManifestV0, VerticalIdV0, W1_CONTRACT_REVISION,
    sha256_identity, validate_baseline,
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;
type InstalledDatabase = (InformationHost, InstallPlan, InstallReceipt, Vec<u8>);

const BASELINE: &str = "750e27e5ad27b6040e7ab7b66f7a2acb910b613a";
const SOURCE_DESCRIPTOR: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-production-tree-750e27e.json"
));
const DATABASE_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-alexandria.sql"
));
const INSTALL_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-install-query.manifest.json"
));
const INSTALL_PROJECTION: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-install-query.projection.json"
));
const STORE_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-resource-store.manifest.json"
));
const STORE_PROJECTION: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-resource-store.projection.json"
));
const PARTIAL_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-partial-publication.manifest.json"
));
const PARTIAL_PROJECTION: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-partial-publication.projection.json"
));
const CORRUPT_CACHE_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-corrupted-disposable-cache.manifest.json"
));
const CORRUPT_CACHE_PROJECTION: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/w1/v0/information-corrupted-disposable-cache.projection.json"
));

#[derive(Deserialize)]
struct SourceDescriptor {
    schema: String,
    repository_id: String,
    commit: String,
    git_trees: BTreeMap<String, String>,
}

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
}

fn git_output(args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository_root())
        .output()
        .expect("execute production source identity check");
    assert!(
        output.status.success(),
        "git source identity check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn verify_production_source() {
    let descriptor: SourceDescriptor =
        serde_json::from_slice(SOURCE_DESCRIPTOR).expect("production source descriptor");
    assert_eq!(descriptor.schema, "delysis.production_source_roots.v0");
    assert_eq!(descriptor.repository_id, "delysis/information-native-kit");
    assert_eq!(descriptor.commit, BASELINE);
    let ancestry = Command::new("git")
        .args(["merge-base", "--is-ancestor", BASELINE, "HEAD"])
        .current_dir(repository_root())
        .status()
        .expect("execute production source ancestry check");
    assert!(
        ancestry.success(),
        "fixture revision must descend from baseline"
    );
    for (path, expected) in descriptor.git_trees {
        for revision in [BASELINE, "HEAD"] {
            let spec = format!("{revision}:{path}");
            let actual = String::from_utf8(git_output(&["rev-parse", &spec]))
                .expect("git object identity is UTF-8");
            assert_eq!(actual.trim(), expected, "production source drift: {path}");
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-12T12:00:00Z")
        .expect("fixed fixture time")
        .with_timezone(&Utc)
}

fn create_database(path: &Path) -> TestResult {
    let connection = Connection::open(path)?;
    connection.execute_batch(DATABASE_SQL)?;
    connection.close().map_err(|(_, error)| error)?;
    Ok(())
}

fn rights() -> Vec<RightsStatement> {
    vec![RightsStatement {
        scope: "complete fixture resource".to_owned(),
        expression: "CC0-1.0".to_owned(),
        license_url: Some("https://creativecommons.org/publicdomain/zero/1.0/".to_owned()),
        license_text_sha256: None,
        attribution: None,
        obligations: Vec::new(),
        redistribution: RedistributionPolicy::Allowed,
    }]
}

fn use_policy() -> UsePolicy {
    UsePolicy {
        local_search: UsePermission::Allowed,
        model_context: UsePermission::Allowed,
        excerpt_export: UsePermission::Allowed,
        redistribution: UsePermission::Allowed,
        attribution_required: false,
    }
}

fn fixture_catalog(
    source_uri: String,
    bytes: &[u8],
    format: RepresentationFormat,
    representation_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let expected_bytes = u64::try_from(bytes.len())?;
    let resource_id = ResourceId::parse("org.delysis.w1.fixture")?;
    let release_id = ReleaseId::parse("2026-08-12")?;
    let representation_id = RepresentationId::parse(representation_id)?;
    let is_database = format.kind == FormatKind::AlexandriaSqlite;
    let capabilities = if is_database {
        BTreeSet::from([
            InformationCapability::LexicalSearch,
            InformationCapability::RecordLookup,
        ])
    } else {
        BTreeSet::from([InformationCapability::RecordLookup])
    };
    let catalog = InformationCatalog {
        schema: CATALOG_SCHEMA.to_owned(),
        catalogue_id: "information-w1-fixture".to_owned(),
        generated_at: fixed_time(),
        publisher: Publisher {
            name: "Delysis deterministic fixture".to_owned(),
            homepage: None,
        },
        declared_trust: CatalogTrust::BuiltIn,
        resources: vec![ResourceRecord {
            resource: ResourceDescriptor {
                id: resource_id.clone(),
                kind: ResourceKind::TextCorpus,
                title: "Information Wave 1 fixture".to_owned(),
                summary: "One public-domain record for deterministic offline replay".to_owned(),
                languages: vec!["en".to_owned()],
                subjects: vec!["contemplation".to_owned()],
                homepage: None,
                extensions: BTreeMap::new(),
            },
            releases: vec![ResourceRelease {
                id: release_id,
                published_at: Some(fixed_time()),
                upstream_id: Some("information-w1-v0".to_owned()),
                immutable: true,
                provenance: Provenance {
                    publisher: "Delysis deterministic fixture".to_owned(),
                    source_uri: "fixture://information/w1".to_owned(),
                    upstream_record_id: Some("W1:DOC:0001".to_owned()),
                    source_inputs: Vec::new(),
                    transformation: Some("deterministic SQLite fixture".to_owned()),
                    metadata: BTreeMap::new(),
                },
                rights: rights(),
                default_use_policy: use_policy(),
                representations: vec![RepresentationDescriptor {
                    id: representation_id,
                    format,
                    capabilities,
                    coverage: CoverageDescriptor {
                        languages: vec!["en".to_owned()],
                        subjects: vec!["contemplation".to_owned()],
                        records: Some(1),
                        ..CoverageDescriptor::default()
                    },
                    subset_support: SubsetSupport::default(),
                    expected_installed_bytes: expected_bytes,
                    artifacts: vec![ArtifactDescriptor {
                        id: ArtifactId::parse("primary")?,
                        role: ArtifactRole::Primary,
                        file_name: if is_database {
                            "library.sqlite".to_owned()
                        } else {
                            "payload.jsonl".to_owned()
                        },
                        media_type: if is_database {
                            "application/vnd.sqlite3".to_owned()
                        } else {
                            "application/x-ndjson".to_owned()
                        },
                        expected_bytes,
                        sha256: sha256(bytes),
                        mirrors: vec![ArtifactMirror {
                            uri: source_uri,
                            priority: 1,
                        }],
                    }],
                    runtime: RuntimeRequirement::None,
                }],
            }],
        }],
    };
    Ok(serde_json::to_value(catalog)?)
}

fn catalog_from_value(value: Value) -> TestResult {
    let catalog: InformationCatalog = serde_json::from_value(value)?;
    catalog.validate()?;
    Ok(())
}

fn build_catalog(
    source_uri: String,
    bytes: &[u8],
    format: RepresentationFormat,
    representation_id: &str,
) -> Result<InformationCatalog, Box<dyn Error>> {
    let value = fixture_catalog(source_uri, bytes, format, representation_id)?;
    catalog_from_value(value.clone())?;
    Ok(serde_json::from_value(value)?)
}

fn database_format() -> RepresentationFormat {
    RepresentationFormat {
        kind: FormatKind::AlexandriaSqlite,
        profile: Some("alexandria.blocks.v1".to_owned()),
        media_type: Some("application/vnd.sqlite3".to_owned()),
    }
}

fn jsonl_format() -> RepresentationFormat {
    RepresentationFormat {
        kind: FormatKind::JsonLines,
        profile: Some("information.w1.partial.v0".to_owned()),
        media_type: Some("application/x-ndjson".to_owned()),
    }
}

fn resolve_plan(
    catalog: &InformationCatalog,
    installation_id: &str,
) -> Result<InstallPlan, Box<dyn Error>> {
    Ok(
        CatalogIndex::new(catalog.clone())?.resolve_install_plan(PlanRequest {
            installation_id: InstallationId::parse(installation_id)?,
            resource_id: ResourceId::parse("org.delysis.w1.fixture")?,
            release_id: ReleaseId::parse("2026-08-12")?,
            representation_id: catalog.resources[0].releases[0].representations[0]
                .id
                .clone(),
            selection: InstallSelection::default(),
            mirror_choices: BTreeMap::new(),
            available_bytes_observed: Some(1024 * 1024),
            created_at: fixed_time(),
        })?,
    )
}

fn normalized_plan_bytes(plan: &InstallPlan) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(serde_json::to_vec(&json!({
        "schema": "information.w1.normalized_install_plan.v0",
        "installation_id": plan.installation_id,
        "resource_id": plan.resource_id,
        "release_id": plan.release_id,
        "representation_id": plan.representation_id,
        "selection": plan.selection,
        "expected_installed_bytes": plan.expected_installed_bytes,
        "artifacts": plan.artifacts.iter().map(|artifact| json!({
            "artifact_id": artifact.artifact_id,
            "file_name": artifact.file_name,
            "expected_bytes": artifact.expected_bytes,
            "sha256": artifact.sha256,
        })).collect::<Vec<_>>(),
        "rights": plan.resolved.rights,
        "use_policy": plan.resolved.use_policy,
    }))?)
}

fn stable_receipt(receipt: &InstallReceipt, normalized_plan: &[u8]) -> Value {
    json!({
        "schema": "information.w1.receipt.summary.v0",
        "installation_id": receipt.installation_id,
        "resource_id": receipt.resource_id,
        "release_id": receipt.release_id,
        "representation_id": receipt.representation_id,
        "state": receipt.state,
        "network_attempted": receipt.network_attempted,
        "network_used": receipt.network_used,
        "downloaded_bytes": receipt.downloaded_bytes,
        "unverified_staged_bytes": receipt.unverified_staged_bytes,
        "installed_relative_path": receipt.installed_relative_path,
        "artifacts": receipt.artifacts,
        "normalized_plan_sha256": sha256(normalized_plan),
        "rights": receipt.resolved.rights,
        "use_policy": receipt.resolved.use_policy,
    })
}

fn query_for(
    installation: &RegisteredInstallation,
    query_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let (resource_id, release_id, representation_id) = match installation {
        RegisteredInstallation::Managed(receipt) => (
            receipt.resource_id.clone(),
            receipt.release_id.clone(),
            receipt.representation_id.clone(),
        ),
        RegisteredInstallation::External(registration) => (
            registration.resource_id.clone(),
            registration.release_id.clone(),
            registration.representation_id.clone(),
        ),
    };
    let query = InformationQuery {
        schema: QUERY_SCHEMA.to_owned(),
        query_id: QueryId::parse(query_id)?,
        text: "prayer quiet".to_owned(),
        syntax: QuerySyntax::NaturalTerms,
        purpose: RetrievalPurpose::LocalUi,
        targets: vec![RetrievalTarget {
            resource_id,
            release_id,
            representation_id,
        }],
        resources: Vec::new(),
        representations: Vec::new(),
        filters: QueryFilters::default(),
        budget: QueryBudget {
            max_hits: 1,
            max_hits_per_backend: 1,
            max_backends: 1,
            max_context_chars: 512,
            timeout_ms: 5_000,
        },
    };
    Ok(serde_json::to_value(query)?)
}

fn stable_evidence(evidence: &EvidenceSet) -> Value {
    let hit = evidence.hits.first().expect("one evidence hit");
    json!({
        "schema": evidence.schema,
        "query_id": evidence.query_id,
        "complete": evidence.complete,
        "warnings": evidence.warnings,
        "hit_count": evidence.hits.len(),
        "hit": {
            "resource_id": hit.resource_id,
            "release_id": hit.release_id,
            "representation_id": hit.representation_id,
            "rank": hit.rank,
            "title": hit.title,
            "creator": hit.creator,
            "snippet": hit.snippet,
            "context": hit.context,
            "excerpt_sha256": hit.excerpt_sha256,
            "source_fingerprint": hit.source_fingerprint,
            "document_id": hit.document_id,
            "passage_id": hit.passage_id,
            "locator": hit.locator,
            "source_uri": hit.source_uri,
            "rights": hit.rights,
            "use_policy": hit.use_policy,
        }
    })
}

fn identity(id: &str, value: &Value) -> ArtifactIdentityV0 {
    let mut bytes = serde_json::to_vec_pretty(value).expect("canonical fixture JSON");
    bytes.push(b'\n');
    sha256_identity(id.to_owned(), &bytes)
}

fn checked_in_identity(id: &str, relative_path: &str) -> ArtifactIdentityV0 {
    let bytes = fs::read(repository_root().join(relative_path)).expect("checked-in fixture bytes");
    sha256_identity(id.to_owned(), &bytes)
}

fn assert_checked_in_value(id: &str, relative_path: &str, actual: &Value) {
    assert_eq!(checked_in_identity(id, relative_path), identity(id, actual));
}

fn assert_manifest_input(manifest_bytes: &[u8], id: &str, bytes: &[u8]) -> TestResult {
    let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(manifest_bytes)?;
    let input = manifest.cases[0]
        .inputs
        .iter()
        .find(|artifact| artifact.identity.id == id)
        .ok_or_else(|| std::io::Error::other(format!("generated input {id} is missing")))?;
    assert_eq!(input.identity, sha256_identity(id, bytes));
    Ok(())
}

fn resource_store_state(snapshot: &StoreSnapshot) -> Value {
    json!({
        "schema": "information.w1.resource_store.before.v0",
        "managed": snapshot.managed,
        "external": snapshot.external,
    })
}

fn directory_state(schema: &str, directory: &Path) -> Result<Value, Box<dyn Error>> {
    let mut entries = Vec::new();
    if directory.exists() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let bytes = fs::read(entry.path())?;
                entries.push(json!({
                    "name": entry.file_name().to_string_lossy(),
                    "bytes": bytes.len(),
                    "sha256": sha256(&bytes),
                }));
            }
        }
    }
    entries.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({"schema": schema, "entries": entries}))
}

fn projection(
    events: Vec<EventFactV0>,
    durable_state: Vec<DurableStateFactV0>,
    lifecycle: Vec<LifecycleFactV0>,
    output_facts: BTreeMap<String, FactValueV0>,
    fail_closed_facts: Vec<String>,
) -> EquivalenceProjectionV0 {
    EquivalenceProjectionV0 {
        ordered_events: events,
        durable_state,
        lifecycle,
        ownership: OwnershipFactsV0 {
            active_operations: 0,
            retained_tasks: 0,
            expected_workers: 0,
            joined_workers: 0,
        },
        output_facts,
        fail_closed_facts,
    }
}

fn event(sequence: u64, operation_id: &str, kind: &str) -> EventFactV0 {
    EventFactV0 {
        sequence: sequence - 1,
        operation_id: operation_id.to_owned(),
        attempt_id: None,
        correlation_id: None,
        kind: kind.to_owned(),
        payload: None,
    }
}

fn completed(operation_id: &str) -> LifecycleFactV0 {
    serde_json::from_value(json!({
        "operation_id": operation_id,
        "attempt_id": null,
        "correlation_id": null,
        "terminal": "completed",
        "released": true
    }))
    .expect("valid completed lifecycle fact")
}

fn cancelled(operation_id: &str) -> LifecycleFactV0 {
    serde_json::from_value(json!({
        "operation_id": operation_id,
        "attempt_id": null,
        "correlation_id": null,
        "terminal": "cancelled",
        "released": true
    }))
    .expect("valid cancelled lifecycle fact")
}

fn validate_fixture(
    manifest_bytes: &[u8],
    expected_bytes: &[u8],
    vertical_id: VerticalIdV0,
    actual: EquivalenceProjectionV0,
) -> TestResult {
    verify_production_source();
    let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(manifest_bytes)?;
    assert_eq!(manifest.vertical_id, vertical_id);
    assert_eq!(manifest.class, vertical_id.class());
    assert_eq!(manifest.contract_revision, W1_CONTRACT_REVISION);
    let case = manifest
        .cases
        .first()
        .ok_or_else(|| std::io::Error::other("fixture manifest has no case"))?;
    assert_eq!(
        case.source.production_tree,
        sha256_identity(
            "information.production.source_roots.750e27e",
            SOURCE_DESCRIPTOR
        )
    );
    for artifact in case
        .inputs
        .iter()
        .chain(case.state_identities.iter().map(|state| &state.baseline))
    {
        if artifact.availability == ArtifactAvailabilityV0::CheckedIn {
            let path = artifact
                .relative_path
                .as_deref()
                .ok_or_else(|| std::io::Error::other("checked-in fixture has no path"))?;
            assert_eq!(
                artifact.identity,
                checked_in_identity(&artifact.identity.id, path)
            );
        }
    }
    let expected_identity = sha256_identity(case.expected_projection.id.clone(), expected_bytes);
    assert_eq!(case.expected_projection, expected_identity);
    let expected: EquivalenceProjectionV0 = serde_json::from_slice(expected_bytes)?;
    assert_eq!(actual, expected);
    let runtime_bytes = serde_json::to_vec(&actual)?;
    let observation: ObservationEnvelopeV0 = serde_json::from_value(json!({
        "schema": VERTICAL_OBSERVATION_SCHEMA_V0,
        "vertical_id": vertical_id,
        "case_id": case.case_id,
        "implementation_revision": BASELINE,
        "observed_prerequisites": [],
        "evidence": {
            "schema": "delysis.evidence_claim.v0",
            "tier": "reproducible",
            "threat_model": "deterministic local fixture with no network or credentials",
            "exact_source": case.source.production_tree.digest,
            "exact_runtime_or_artifact": sha256_identity("information.runtime.projection", &runtime_bytes).digest,
            "execution_kind": "fixture",
            "omitted_claims": manifest.omitted_claims,
            "negative_evidence": []
        },
        "projection": actual
    }))?;
    validate_baseline(&manifest, &case.case_id, expected_bytes, &[], &observation)?;
    Ok(())
}

fn maybe_dump(label: &str, projection: &EquivalenceProjectionV0, extra: &Value) -> bool {
    if std::env::var_os("INFORMATION_W1_DUMP").is_none() {
        return false;
    }
    eprintln!(
        "W1_DUMP {label} EXTRA {}\nW1_DUMP {label} PROJECTION {}",
        serde_json::to_string_pretty(extra).expect("dump extra"),
        serde_json::to_string_pretty(projection).expect("dump projection")
    );
    true
}

fn make_host(
    root: &Path,
    source_root: &Path,
    catalog: InformationCatalog,
) -> Result<InformationHost, Box<dyn Error>> {
    let mut policy = AcquisitionPolicy::restricted();
    policy.grant_file_root(source_root)?;
    Ok(InformationHost::with_components_and_policy(
        CatalogIndex::new(catalog)?,
        ManagedStore::open(root)?,
        AcquireClient::with_defaults()?,
        policy,
        RetrievalRouter::new(),
    )?)
}

fn install_database(
    temporary: &TempDir,
    installation_id: &str,
) -> Result<InstalledDatabase, Box<dyn Error>> {
    let source = temporary.path().join(format!("{installation_id}.sqlite"));
    create_database(&source)?;
    let bytes = fs::read(&source)?;
    let source_uri = url::Url::from_file_path(&source)
        .map_err(|_| std::io::Error::other("fixture file URI"))?
        .to_string();
    let catalog = build_catalog(source_uri, &bytes, database_format(), "alexandria")?;
    let host = make_host(&temporary.path().join("managed"), temporary.path(), catalog)?;
    let plan = resolve_plan(host.catalog(), installation_id)?;
    let receipt = host.install(&plan)?;
    Ok((host, plan, receipt, bytes))
}

#[test]
fn information_install_query_replays_through_production_paths() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let (host, plan, receipt, database_bytes) =
        install_database(&temporary, "information-w1-install")?;
    assert_manifest_input(INSTALL_MANIFEST, "information.database", &database_bytes)?;
    let normalized_plan = normalized_plan_bytes(&plan)?;
    assert_eq!(receipt.state, InstallationState::Ready);
    assert!(!receipt.network_used);
    assert_eq!(receipt.artifacts[0].sha256, sha256(&database_bytes));
    host.mount_installation(&plan.installation_id, MountInstallationOptions::default())?;
    let installed = host
        .store()
        .get(&plan.installation_id)?
        .ok_or_else(|| std::io::Error::other("installed fixture missing"))?;
    let query: InformationQuery = serde_json::from_value(query_for(&installed, "w1-query")?)?;
    let evidence = host.search(&query)?;
    assert_eq!(evidence.hits.len(), 1);
    assert_eq!(
        evidence.hits[0].passage_id.as_deref(),
        Some("W1:DOC:0001:B000001")
    );
    let removal = host.plan_removal(&plan.installation_id)?;
    assert_eq!(removal.kind, RemovalKind::ManagedPackage);
    assert!(removal.requires_explicit_confirmation);

    let state = stable_receipt(&receipt, &normalized_plan);
    let evidence = stable_evidence(&evidence);
    let after = identity("information.install_query.ready", &state);
    let actual = projection(
        vec![event(
            1,
            "information.install-query",
            "installed_and_queried",
        )],
        vec![DurableStateFactV0 {
            state_id: "information.installation".to_owned(),
            schema_id: "information.w1.receipt.summary.v0".to_owned(),
            before: None,
            after: Some(after),
            disposition: StateDispositionV0::Created,
        }],
        vec![completed("information.install-query")],
        BTreeMap::from([
            (
                "database_bytes".to_owned(),
                FactValueV0::Integer(i64::try_from(database_bytes.len())?),
            ),
            (
                "database_sha256".to_owned(),
                FactValueV0::Text(sha256(&database_bytes)),
            ),
            (
                "normalized_plan".to_owned(),
                FactValueV0::Digest(
                    sha256_identity("information.install.plan", &normalized_plan).digest,
                ),
            ),
            (
                "query_evidence".to_owned(),
                FactValueV0::Digest(identity("information.query.evidence", &evidence).digest),
            ),
            ("bounded_hit_count".to_owned(), FactValueV0::Integer(1)),
            (
                "stable_locator".to_owned(),
                FactValueV0::Text("record:blocks:W1:DOC:0001:B000001".to_owned()),
            ),
            (
                "removal_requires_confirmation".to_owned(),
                FactValueV0::Boolean(true),
            ),
            ("network_used".to_owned(), FactValueV0::Boolean(false)),
        ]),
        vec![
            "ungranted network sources cannot participate in this replay".to_owned(),
            "removal planning does not delete product-owned state".to_owned(),
        ],
    );
    if maybe_dump(
        "install",
        &actual,
        &json!({"database": sha256_identity("information.database", &database_bytes), "state": state, "evidence": evidence}),
    ) {
        return Ok(());
    }
    validate_fixture(
        INSTALL_MANIFEST,
        INSTALL_PROJECTION,
        VerticalIdV0::InformationInstallQuery,
        actual,
    )
}

#[test]
fn information_resource_store_reopens_managed_and_external_state() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let empty_store = ManagedStore::open(temporary.path().join("managed"))?;
    let before_value = resource_store_state(&empty_store.list()?);
    assert_checked_in_value(
        "information.resource_store.before",
        "tests/fixtures/w1/v0/information-resource-store-before.json",
        &before_value,
    );
    let (host, plan, receipt, database_bytes) =
        install_database(&temporary, "information-w1-managed")?;
    let source = temporary.path().join("information-w1-managed.sqlite");
    let before_identity = capture_source_identity(&source, IdentityStrength::Sha256)?;
    let external_request = ExternalRegistrationRequest {
        installation_id: InstallationId::parse("information-w1-external")?,
        resource_id: ResourceId::parse("org.delysis.w1.fixture.external")?,
        release_id: ReleaseId::parse("2026-08-12")?,
        representation_id: RepresentationId::parse("alexandria")?,
        format: database_format(),
        absolute_path: source.clone(),
        access_mode: ExternalAccessMode::ImmutableReadOnly,
        provenance: Provenance {
            publisher: "Delysis deterministic fixture".to_owned(),
            source_uri: "fixture://information/w1/external".to_owned(),
            upstream_record_id: Some("W1:DOC:0001".to_owned()),
            source_inputs: Vec::new(),
            transformation: None,
            metadata: BTreeMap::new(),
        },
        rights: rights(),
        use_policy: use_policy(),
    };
    let wal = source.with_file_name("information-w1-managed.sqlite-wal");
    fs::write(&wal, b"pending transaction")?;
    assert!(matches!(
        reject_nonempty_sqlite_sidecars(&source),
        Err(StoreError::NonEmptySqliteWal(path)) if path == wal
    ));
    assert_eq!(fs::read(&source)?, database_bytes);
    fs::remove_file(&wal)?;
    let external = host.register_external(&external_request)?;
    let database_sha256 = sha256(&database_bytes);
    assert_eq!(
        external.identity.sha256.as_deref(),
        Some(database_sha256.as_str())
    );
    drop(host);

    let catalog_source = url::Url::from_file_path(&source)
        .map_err(|_| std::io::Error::other("fixture file URI"))?
        .to_string();
    let catalog = build_catalog(
        catalog_source,
        &database_bytes,
        database_format(),
        "alexandria",
    )?;
    let reopened = make_host(&temporary.path().join("managed"), temporary.path(), catalog)?;
    let mounted = reopened.mount_supported_installations(MountInstallationOptions::default())?;
    assert_eq!(mounted.len(), 2);
    let snapshot = reopened.installed()?;
    assert_eq!(snapshot.managed.len(), 1);
    assert_eq!(snapshot.external.len(), 1);
    let managed = reopened
        .store()
        .get(&plan.installation_id)?
        .ok_or_else(|| std::io::Error::other("managed fixture missing after reopen"))?;
    let managed_query: InformationQuery =
        serde_json::from_value(query_for(&managed, "w1-managed-query")?)?;
    assert_eq!(reopened.search(&managed_query)?.hits.len(), 1);
    let external_installation = reopened
        .store()
        .get(&external.installation_id)?
        .ok_or_else(|| std::io::Error::other("external fixture missing after reopen"))?;
    let external_query: InformationQuery =
        serde_json::from_value(query_for(&external_installation, "w1-external-query")?)?;
    assert_eq!(reopened.search(&external_query)?.hits.len(), 1);
    let identity_check = check_source_identity(&source, &before_identity)?;
    assert!(identity_check.unchanged);

    let normalized_plan = normalized_plan_bytes(&plan)?;
    let after_value = json!({
        "schema": "information.w1.resource_store.after.v0",
        "managed": [stable_receipt(&receipt, &normalized_plan)],
        "external": [{
            "installation_id": external.installation_id,
            "resource_id": external.resource_id,
            "release_id": external.release_id,
            "representation_id": external.representation_id,
            "access_mode": external.access_mode,
            "bytes": external.identity.bytes,
            "sha256": external.identity.sha256,
            "rights": external.rights,
            "use_policy": external.use_policy
        }],
        "partial": [],
        "reopened": true
    });
    let before = checked_in_identity(
        "information.resource_store.before",
        "tests/fixtures/w1/v0/information-resource-store-before.json",
    );
    let after = identity("information.resource_store.after", &after_value);
    let actual = projection(
        vec![event(
            1,
            "information.resource-store",
            "reopened_and_queried",
        )],
        vec![DurableStateFactV0 {
            state_id: "information.resource_store".to_owned(),
            schema_id: "information.w1.resource_store.v0".to_owned(),
            before: Some(before),
            after: Some(after),
            disposition: StateDispositionV0::Updated,
        }],
        vec![completed("information.resource-store")],
        BTreeMap::from([
            ("managed_ready".to_owned(), FactValueV0::Boolean(true)),
            ("external_immutable".to_owned(), FactValueV0::Boolean(true)),
            (
                "external_identity_stable".to_owned(),
                FactValueV0::Boolean(true),
            ),
            ("mounted_after_reopen".to_owned(), FactValueV0::Integer(2)),
            (
                "queryable_after_reopen".to_owned(),
                FactValueV0::Boolean(true),
            ),
            ("network_used".to_owned(), FactValueV0::Boolean(false)),
            (
                "nonempty_wal_rejected".to_owned(),
                FactValueV0::Boolean(true),
            ),
        ]),
        vec![
            "external immutable source rejects non-empty SQLite sidecars".to_owned(),
            "external source remains outside the managed root and is never migrated".to_owned(),
        ],
    );
    if maybe_dump("store", &actual, &after_value) {
        return Ok(());
    }
    validate_fixture(
        STORE_MANIFEST,
        STORE_PROJECTION,
        VerticalIdV0::InformationResourceStore,
        actual,
    )
}

fn partial_plan(
    root: &Path,
    installation_id: &str,
    bytes: &[u8],
) -> Result<InstallPlan, Box<dyn Error>> {
    let source = root.join(format!("{installation_id}.jsonl"));
    fs::write(&source, bytes)?;
    let source_uri = url::Url::from_file_path(&source)
        .map_err(|_| std::io::Error::other("fixture file URI"))?
        .to_string();
    let catalog = build_catalog(source_uri, bytes, jsonl_format(), "jsonl")?;
    resolve_plan(&catalog, installation_id)
}

fn partial_state_value(schema: &str, state: &str, bytes: &[u8]) -> Value {
    json!({
        "schema": schema,
        "state": state,
        "ready": false,
        "artifact_bytes": bytes.len(),
        "artifact_sha256": sha256(bytes)
    })
}

#[test]
fn corrupted_disposable_acquisition_cache_is_cold_reset() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let payload = b"payload";
    assert_manifest_input(
        CORRUPT_CACHE_MANIFEST,
        "information.partial.payload",
        payload,
    )?;
    let store = ManagedStore::open(temporary.path().join("cache-store"))?;
    let plan = partial_plan(temporary.path(), "information-w1-corrupt-cache", payload)?;
    let prepared = store.prepare_install(&plan)?;
    let acquisitions = prepared.directory.join("acquisitions");
    fs::write(acquisitions.join(".journal-malformed.tmp"), b"malformed")?;
    let before_value = directory_state(
        "information.w1.corrupted_disposable_cache.before.v0",
        &acquisitions,
    )?;
    assert_checked_in_value(
        "information.corrupted_disposable_cache.before",
        "tests/fixtures/w1/v0/information-corrupted-disposable-cache-before.json",
        &before_value,
    );

    let recovered = store.prepare_install(&plan)?;
    assert_eq!(recovered.directory, prepared.directory);
    let after_value = directory_state(
        "information.w1.corrupted_disposable_cache.after.v0",
        &acquisitions,
    )?;
    assert_eq!(after_value["entries"], json!([]));
    assert_eq!(store.list_partial_installs()?.len(), 1);
    assert!(store.list()?.managed.is_empty());

    let actual = projection(
        vec![event(
            1,
            "information.corrupted-cache-reset",
            "malformed_acquisition_temporary_removed",
        )],
        vec![DurableStateFactV0 {
            state_id: "information.corrupted_disposable_cache".to_owned(),
            schema_id: "information.w1.corrupted_disposable_cache.v0".to_owned(),
            before: Some(checked_in_identity(
                "information.corrupted_disposable_cache.before",
                "tests/fixtures/w1/v0/information-corrupted-disposable-cache-before.json",
            )),
            after: None,
            disposition: StateDispositionV0::Removed,
        }],
        vec![completed("information.corrupted-cache-reset")],
        BTreeMap::from([
            (
                "malformed_temporary_removed".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "authoritative_state_preserved".to_owned(),
                FactValueV0::Boolean(true),
            ),
            ("network_used".to_owned(), FactValueV0::Boolean(false)),
        ]),
        vec![
            "only disposable .journal-*.tmp acquisition state is removed".to_owned(),
            "the valid staging manifest and authoritative artifact target remain".to_owned(),
        ],
    );
    if maybe_dump("corrupt-cache", &actual, &after_value) {
        return Ok(());
    }
    validate_fixture(
        CORRUPT_CACHE_MANIFEST,
        CORRUPT_CACHE_PROJECTION,
        VerticalIdV0::CorruptedDisposableCaches,
        actual,
    )
}

#[test]
fn partial_publication_states_recover_without_clobber_or_network() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("acquire-source.bin");
    let publication = temporary.path().join("publication");
    fs::create_dir(&publication)?;
    let destination = publication.join("acquire-destination.bin");
    fs::write(&source, b"payload")?;
    assert_manifest_input(PARTIAL_MANIFEST, "information.partial.payload", b"payload")?;
    let mut cancel = |progress: TransferProgress| {
        if progress.phase == TransferPhase::Publishing {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };
    let cancelled_result = AcquireClient::with_defaults()?.fetch_file_artifact_with_progress(
        &source,
        &destination,
        7,
        &sha256(b"payload"),
        1024,
        &mut cancel,
    );
    assert!(matches!(
        cancelled_result,
        Err(information_native_acquire::AcquireError::Cancelled { .. })
    ));
    assert!(!destination.exists());
    let acquisition_before_value =
        directory_state("information.w1.partial_acquisition.before.v0", &publication)?;
    assert_checked_in_value(
        "information.partial.acquisition.before",
        "tests/fixtures/w1/v0/information-partial-acquisition-before.json",
        &acquisition_before_value,
    );
    let acquisition_before = checked_in_identity(
        "information.partial.acquisition.before",
        "tests/fixtures/w1/v0/information-partial-acquisition-before.json",
    );
    let recovered_fetch = AcquireClient::with_defaults()?.fetch_file_artifact(
        &source,
        &destination,
        7,
        &sha256(b"payload"),
        1024,
    )?;
    assert!(!recovered_fetch.network_used);
    let conflict = temporary.path().join("caller-owned.bin");
    fs::write(&conflict, b"caller-owned")?;
    assert!(
        AcquireClient::with_defaults()?
            .fetch_file_artifact(&source, &conflict, 7, &sha256(b"payload"), 1024)
            .is_err()
    );
    assert_eq!(fs::read(&conflict)?, b"caller-owned");

    let payload = b"payload";
    let store = ManagedStore::open(temporary.path().join("partial-store"))?;
    let staged_plan = partial_plan(temporary.path(), "information-w1-staged", payload)?;
    let staged = store.prepare_install(&staged_plan)?;
    fs::write(&staged.artifacts[0].path, payload)?;
    let staged_partial = store.list_partial_installs()?;
    assert_eq!(staged_partial.len(), 1);
    assert_eq!(staged_partial[0].state, PartialInstallState::Staged);
    assert!(store.list()?.managed.is_empty());
    let staged_before_value =
        partial_state_value("information.w1.partial_staged.before.v0", "staged", payload);
    assert_eq!(
        checked_in_identity(
            "information.partial.staged.before",
            "tests/fixtures/w1/v0/information-partial-staged-before.json"
        ),
        identity("information.partial.staged.before", &staged_before_value)
    );
    let staged_transfer = TransferSummary::for_plan(&staged_plan, staged.prepared_at, false);
    store.record_staged_acquisition(&staged_plan, &staged_transfer.acquisitions[0])?;
    let staged_receipt = store.activate(&staged_plan, staged_transfer)?;

    let activated_plan = partial_plan(temporary.path(), "information-w1-activated", payload)?;
    let activated = store.prepare_install(&activated_plan)?;
    fs::write(&activated.artifacts[0].path, payload)?;
    let activated_transfer =
        TransferSummary::for_plan(&activated_plan, activated.prepared_at, false);
    store.record_staged_acquisition(&activated_plan, &activated_transfer.acquisitions[0])?;
    let key = sha256(activated_plan.installation_id.as_str().as_bytes());
    let package = store.root().join("packages").join(key);
    fs::rename(&activated.directory, &package)?;
    let activated_partial = store.list_partial_installs()?;
    assert_eq!(activated_partial.len(), 1);
    assert_eq!(
        activated_partial[0].state,
        PartialInstallState::ActivatedUnregistered
    );
    assert_eq!(store.list()?.managed.len(), 1);
    let activated_before_value = partial_state_value(
        "information.w1.partial_activated.before.v0",
        "activated_unregistered",
        payload,
    );
    assert_eq!(
        checked_in_identity(
            "information.partial.activated.before",
            "tests/fixtures/w1/v0/information-partial-activated-before.json"
        ),
        identity(
            "information.partial.activated.before",
            &activated_before_value
        )
    );
    let activated_receipt = store
        .recover_interrupted_activation(&activated_plan)?
        .ok_or_else(|| std::io::Error::other("activated package was not recovered"))?;
    assert!(store.list_partial_installs()?.is_empty());
    assert_eq!(store.list()?.managed.len(), 2);

    let acquisition_after = json!({
        "schema": "information.w1.partial_acquisition.after.v0",
        "state": "published",
        "bytes": recovered_fetch.bytes,
        "sha256": recovered_fetch.sha256,
        "network_used": recovered_fetch.network_used,
        "caller_owned_conflict_preserved": true
    });
    let staged_after = stable_receipt(&staged_receipt, &normalized_plan_bytes(&staged_plan)?);
    let activated_after =
        stable_receipt(&activated_receipt, &normalized_plan_bytes(&activated_plan)?);
    let actual = projection(
        vec![
            event(
                1,
                "information.acquire-cancel",
                "cancelled_before_publication",
            ),
            event(2, "information.acquire-retry", "published_exact_bytes"),
            event(3, "information.store-staged", "activated_from_staging"),
            event(
                4,
                "information.store-activated",
                "recovered_activated_unregistered",
            ),
        ],
        vec![
            DurableStateFactV0 {
                state_id: "information.partial_acquisition".to_owned(),
                schema_id: "information.w1.partial_acquisition.v0".to_owned(),
                before: Some(acquisition_before),
                after: Some(identity(
                    "information.partial.acquisition.after",
                    &acquisition_after,
                )),
                disposition: StateDispositionV0::Recovered,
            },
            DurableStateFactV0 {
                state_id: "information.partial_staged".to_owned(),
                schema_id: "information.w1.partial_staged.v0".to_owned(),
                before: Some(checked_in_identity(
                    "information.partial.staged.before",
                    "tests/fixtures/w1/v0/information-partial-staged-before.json",
                )),
                after: Some(identity("information.partial.staged.after", &staged_after)),
                disposition: StateDispositionV0::Recovered,
            },
            DurableStateFactV0 {
                state_id: "information.partial_activated".to_owned(),
                schema_id: "information.w1.partial_activated.v0".to_owned(),
                before: Some(checked_in_identity(
                    "information.partial.activated.before",
                    "tests/fixtures/w1/v0/information-partial-activated-before.json",
                )),
                after: Some(identity(
                    "information.partial.activated.after",
                    &activated_after,
                )),
                disposition: StateDispositionV0::Recovered,
            },
        ],
        vec![
            cancelled("information.acquire-cancel"),
            completed("information.acquire-retry"),
            completed("information.store-staged"),
            completed("information.store-activated"),
        ],
        BTreeMap::from([
            (
                "acquisition_partial_distinct".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "staged_install_distinct".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "activated_unregistered_distinct".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "caller_owned_bytes_preserved".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "partials_hidden_until_ready".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "allowed_transitions_completed".to_owned(),
                FactValueV0::Integer(3),
            ),
            ("network_used".to_owned(), FactValueV0::Boolean(false)),
        ]),
        vec![
            "cancelled acquisition never publishes destination bytes".to_owned(),
            "competing caller-owned destination is never clobbered".to_owned(),
            "staged and activated-unregistered packages are never listed ready".to_owned(),
        ],
    );
    if maybe_dump(
        "partial",
        &actual,
        &json!({"acquisition_after": acquisition_after, "staged_after": staged_after, "activated_after": activated_after}),
    ) {
        return Ok(());
    }
    validate_fixture(
        PARTIAL_MANIFEST,
        PARTIAL_PROJECTION,
        VerticalIdV0::PartialPublicationStates,
        actual,
    )
}

#[test]
fn source_fixture_database_is_exact_and_queryable_without_network() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("fixture.sqlite");
    create_database(&path)?;
    let bytes = fs::read(&path)?;
    let identity = sha256_identity("information.database", &bytes);
    if std::env::var_os("INFORMATION_W1_DUMP").is_some() {
        eprintln!("W1_DUMP database {}", serde_json::to_string(&identity)?);
    }
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let block_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM blocks", [], |row| row.get(0))?;
    assert_eq!(block_count, 1);
    Ok(())
}
