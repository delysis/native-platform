//! Actual executable and public native-owner controls over an owned SQLite file.
//! This is component evidence, not native UI, model or packaged acceptance.
use information_native_backend_sqlite::{AlexandriaBackend, AlexandriaBackendConfig};
use information_native_retrieval::{LookupRequest, ResourceBackend, RetrievalRouter};
use information_native_types::{
    EvidenceLocator, ExternalAccessMode, RedistributionPolicy, ReleaseId, RepresentationId,
    ResourceId, RetrievalPurpose, RightsStatement, UsePermission, UsePolicy, evidence_text_sha256,
};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

const RESOURCE: &str = "cli-alexandria-fixture";
const RELEASE: &str = "owned";
const REPRESENTATION: &str = "blocks-v1";
const BLOCK: &str = "D1:B000002";
const EXACT: &str = "The prayer of quiet gathers the powers for contemplation.";
const EXACT_CONTEXT: &str = concat!(
    "[D1:B000002; index=2; location=chapter 1]\n",
    "The prayer of quiet gathers the powers for contemplation."
);

struct Fixture {
    _temporary: Option<tempfile::TempDir>,
    root: PathBuf,
    database: PathBuf,
    retained_for_witness: bool,
}

impl Fixture {
    fn new(label: &str, status: &str) -> Result<Self, Box<dyn Error>> {
        let (temporary, root, retained_for_witness) =
            match std::env::var_os("INFORMATION_CLI_FIXTURE_ROOT") {
                Some(root) => {
                    fs::create_dir_all(&root)?;
                    let directory = tempfile::Builder::new().prefix(label).tempdir_in(root)?;
                    (None, directory.keep(), true)
                }
                None => {
                    let directory = tempfile::tempdir()?;
                    let root = directory.path().to_owned();
                    (Some(directory), root, false)
                }
            };
        let database = root.join("library.db");
        let connection = Connection::open(&database)?;
        connection.execute_batch(include_str!("fixtures/alexandria.sql"))?;
        connection.execute(
            "UPDATE documents SET rights_status = ?1 WHERE doc_id = 'D1'",
            [status],
        )?;
        drop(connection);
        if retained_for_witness {
            eprintln!("owned_cli_fixture={}", root.display());
        }
        Ok(Self {
            _temporary: temporary,
            root,
            database,
            retained_for_witness,
        })
    }

    fn command(&self, operation: &str, arguments: &[&str]) -> Result<Output, Box<dyn Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_information-native"))
            .args(["alexandria", operation, "--database"])
            .arg(&self.database)
            .args([
                "--resource-id",
                RESOURCE,
                "--release-id",
                RELEASE,
                "--representation-id",
                REPRESENTATION,
                "--publisher",
                "Owned fixture",
                "--context-radius",
                "0",
                "--max-context-chars",
                "1000",
            ])
            .args(arguments)
            .output()?)
    }

    fn snapshot(&self, label: &str) -> Result<Snapshot, Box<dyn Error>> {
        let snapshot = Snapshot {
            database: fs::read(&self.database)?,
            wal: optional_bytes(&sidecar(&self.database, "-wal"))?,
            shm: optional_bytes(&sidecar(&self.database, "-shm"))?,
            journal: optional_bytes(&sidecar(&self.database, "-journal"))?,
        };
        if self.retained_for_witness {
            let path = self.root.join(format!("{label}.json"));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            file.write_all(&serde_json::to_vec(&snapshot)?)?;
        }
        Ok(snapshot)
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct Snapshot {
    database: Vec<u8>,
    wal: Option<Vec<u8>>,
    shm: Option<Vec<u8>>,
    journal: Option<Vec<u8>>,
}

fn sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn optional_bytes(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn request(collection: Option<String>) -> Result<LookupRequest, Box<dyn Error>> {
    Ok(LookupRequest {
        resource_id: ResourceId::parse(RESOURCE)?,
        release_id: ReleaseId::parse(RELEASE)?,
        representation_id: RepresentationId::parse(REPRESENTATION)?,
        purpose: RetrievalPurpose::LocalUi,
        collection,
        key: BLOCK.into(),
        max_context_chars: 1000,
        timeout_ms: 5000,
    })
}

#[test]
fn alexandria_block_uses_the_router_canonical_collection() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("alexandria-ordinary-", "public_domain")?;
    let before = fixture.snapshot("before")?;
    let mut config = AlexandriaBackendConfig::new(
        "owned-sdk",
        "Owned SDK fixture",
        ResourceId::parse(RESOURCE)?,
        ReleaseId::parse(RELEASE)?,
        RepresentationId::parse(REPRESENTATION)?,
        &fixture.database,
        ExternalAccessMode::LiveReadOnly,
        "Owned fixture",
    );
    config.context_radius = 0;
    let backend = Arc::new(AlexandriaBackend::open(config)?);
    let canonical = backend.lookup(&request(None)?)?;
    assert_eq!(canonical.hit.snippet, EXACT);
    assert_eq!(canonical.hit.context, EXACT_CONTEXT);
    assert_eq!(
        canonical.hit.excerpt_sha256,
        evidence_text_sha256(EXACT, EXACT_CONTEXT)
    );
    canonical.hit.validate()?;

    // The native profile accepts blocks, but the Router's canonical contract
    // still refuses it. Fix the caller without weakening that actual guard.
    let router = RetrievalRouter::from_backends([backend as Arc<dyn ResourceBackend>])?;
    let mismatch = router
        .lookup(&request(Some("blocks".into()))?)
        .expect_err("noncanonical collection must still refuse");
    assert_eq!(mismatch.code, "all_backends_failed");
    assert!(
        mismatch
            .safe_message
            .contains("backend_lookup_key_mismatch")
    );

    let search = fixture.command("search", &["--text", "prayer quiet"])?;
    assert!(
        search.status.success(),
        "ordinary actual CLI search failed: {:?}",
        search
    );
    let search_result: Value = serde_json::from_slice(&search.stdout)?;
    assert!(
        search_result["hits"]
            .as_array()
            .is_some_and(|hits| hits.iter().any(|hit| hit["passage_id"] == BLOCK))
    );
    assert_eq!(fixture.snapshot("after-positive-controls")?, before);

    let block = fixture.command("block", &["--block-id", BLOCK])?;
    assert_eq!(fixture.snapshot("after-block")?, before);
    // BEFORE is predicted to fail here with the truthful outer CLI message
    // containing the inner key mismatch. Assertions below are then unreached.
    assert!(
        block.status.success(),
        "ordinary actual CLI block must succeed; status={:?}, stdout={}, stderr={}",
        block.status.code(),
        String::from_utf8_lossy(&block.stdout),
        String::from_utf8_lossy(&block.stderr)
    );
    let result: Value = serde_json::from_slice(&block.stdout)?;
    assert_eq!(
        result["hit"]["locator"],
        serde_json::to_value(&canonical.hit.locator)?
    );
    assert!(matches!(
        canonical.hit.locator,
        EvidenceLocator::SqliteBlock { .. }
    ));
    assert_eq!(result["hit"]["resource_id"], RESOURCE);
    assert_eq!(result["hit"]["release_id"], RELEASE);
    assert_eq!(result["hit"]["representation_id"], REPRESENTATION);
    assert_eq!(result["hit"]["snippet"], EXACT);
    assert_eq!(result["hit"]["context"], EXACT_CONTEXT);
    assert_eq!(
        result["hit"]["excerpt_sha256"],
        canonical.hit.excerpt_sha256
    );
    assert!(block.stderr.is_empty());
    Ok(())
}

#[test]
fn alexandria_block_preserves_purpose_and_private_rights_refusals() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("alexandria-private-", "private donor archive")?;
    let before = fixture.snapshot("before")?;
    let model = fixture.command(
        "block",
        &["--block-id", BLOCK, "--purpose", "model-context"],
    )?;
    assert!(!model.status.success());
    assert!(model.stdout.is_empty());
    let report: Value = serde_json::from_slice(&model.stderr)?;
    assert_eq!(report["ok"], false);
    assert!(report["error"]["message"].as_str().is_some_and(|message| {
        message.contains("does not explicitly allow this retrieval purpose")
    }));
    assert_eq!(fixture.snapshot("after-model-refusal")?, before);

    let policy = UsePolicy {
        excerpt_export: UsePermission::Allowed,
        redistribution: UsePermission::Allowed,
        attribution_required: false,
        ..UsePolicy::default()
    };
    let policy_path = fixture.root.join("export-policy.json");
    fs::write(&policy_path, serde_json::to_vec(&policy)?)?;
    let rights_path = fixture.root.join("source-rights.json");
    fs::write(
        &rights_path,
        serde_json::to_vec(&vec![RightsStatement {
            scope: "corpus".into(),
            expression: "approved owned fixture".into(),
            license_url: None,
            license_text_sha256: None,
            attribution: Some("Owned fixture".into()),
            obligations: Vec::new(),
            redistribution: RedistributionPolicy::Allowed,
        }])?,
    )?;
    let export = Command::new(env!("CARGO_BIN_EXE_information-native"))
        .args(["alexandria", "block", "--database"])
        .arg(&fixture.database)
        .args([
            "--resource-id",
            RESOURCE,
            "--release-id",
            RELEASE,
            "--representation-id",
            REPRESENTATION,
            "--publisher",
            "Owned fixture",
            "--block-id",
            BLOCK,
            "--purpose",
            "excerpt-export",
            "--use-policy-json",
        ])
        .arg(&policy_path)
        .arg("--rights-json")
        .arg(&rights_path)
        .output()?;
    assert!(!export.status.success());
    assert!(export.stdout.is_empty());
    let report: Value = serde_json::from_slice(&export.stderr)?;
    assert_eq!(report["ok"], false);
    assert!(
        report["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("retrieval_purpose_not_permitted"))
    );
    assert_eq!(fixture.snapshot("after-private-export-refusal")?, before);
    Ok(())
}
