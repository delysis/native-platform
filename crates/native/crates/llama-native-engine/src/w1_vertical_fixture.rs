#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use platform_contracts_v0_vertical::{
    ArtifactIdentityV0, EvidenceClaimV0, EvidenceTier, ExecutionKind, TerminalClass,
};
use platform_vertical_fixtures_v0::{
    DurableStateFactV0, EquivalenceProjectionV0, EventFactV0, FactValueV0, LifecycleFactV0,
    ObservationEnvelopeV0, OwnershipFactsV0, StateDispositionV0, VERTICAL_OBSERVATION_SCHEMA_V0,
    VerticalFixtureManifestV0, VerticalIdV0, sha256_identity, validate_baseline, validate_manifest,
    verify_prerequisite_chunks,
};
use serde::{Deserialize, Serialize};

use crate::operation_registry::{
    ControlledRequest, RequestPhase, RequestRegistry, RequestSnapshot, RequestTerminalClass,
};
use crate::{
    COMMAND_CAPACITY, CompletionPrompt, GenerationBatchRequest, GenerationCase,
    GenerationEventKind, GenerationInput, GenerationRequest, GenerationState, ModelRuntimeState,
    NativeDevice, NativeErrorCode, NativeModelConfig, NativeModelHandle, NativeModelInner,
    NativeModelOwner, NativeTransport, ResidentModelStatus, SamplingConfig, SpecialTokenPolicy,
    WorkerIdentity,
};
use crossbeam_channel::bounded;

const MANIFEST_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-current-qwen-manifest-v0.json");
const BUNDLE_MANIFEST: &str = include_str!("../../../fixtures/w1/MANIFEST.sha256");
const INPUT_BYTES: &[u8] = include_bytes!("../../../fixtures/w1/native-current-qwen-input-v0.json");
const SOURCE_TREE_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-current-qwen-source-tree-v0.txt");
const EXPECTED_PROJECTION_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-current-qwen-projection-v0.json");
const QUIT_RELAUNCH_MANIFEST_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-quit-relaunch-manifest-v0.json");
const QUIT_RELAUNCH_INPUT_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-quit-relaunch-input-v0.json");
const QUIT_RELAUNCH_SOURCE_TREE_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-quit-relaunch-source-tree-v0.txt");
const QUIT_RELAUNCH_PROJECTION_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-quit-relaunch-projection-v0.json");
const QUIT_RELAUNCH_BASELINE_COMMIT: &str = "897dd86a961707c66021d1eaabcfd19314cb05f7";
const RECEIPT_SCHEMA: &str = "delysis.native.fake_owner_receipt.v0";
static RECEIPT_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFixture {
    schema: String,
    prompt: String,
    seed: u32,
    temperature: f32,
    max_tokens: u32,
    required_model_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuitRelaunchInput {
    schema: String,
    quit_model_id: String,
    quit_operation_id: String,
    relaunch_model_id: String,
    relaunch_operation_id: String,
    receipt_file_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableReceipt {
    schema: String,
    sequence: u64,
    epoch: String,
    kind: String,
    operation_id: String,
    terminal: String,
    active_operations: usize,
    retained_tasks: usize,
    expected_workers: usize,
    joined_workers: usize,
}

struct ReceiptDirectory {
    path: PathBuf,
}

impl ReceiptDirectory {
    fn new() -> std::io::Result<Self> {
        let epoch_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        loop {
            let sequence = RECEIPT_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "llama-native-w1-quit-relaunch-{epoch_nanos}-{sequence}"
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for ReceiptDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct DurableReceiptStore {
    path: PathBuf,
    receipts: Vec<DurableReceipt>,
}

impl DurableReceiptStore {
    fn open(path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let receipts: Vec<DurableReceipt> = if bytes.is_empty() {
            Vec::new()
        } else {
            std::str::from_utf8(&bytes)?
                .lines()
                .map(serde_json::from_str)
                .collect::<Result<Vec<_>, _>>()?
        };
        for (sequence, receipt) in receipts.iter().enumerate() {
            if receipt.schema != RECEIPT_SCHEMA || receipt.sequence != sequence as u64 {
                return Err("durable receipt sequence or schema is invalid".into());
            }
        }
        Ok(Self { path, receipts })
    }

    fn append(&mut self, receipt: DurableReceipt) -> Result<(), Box<dyn std::error::Error>> {
        if receipt.schema != RECEIPT_SCHEMA || receipt.sequence != self.receipts.len() as u64 {
            return Err("durable receipt append is out of sequence".into());
        }
        let encoded = serde_json::to_vec(&receipt)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        self.receipts.push(receipt);
        Ok(())
    }

    fn bytes(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(&self.path)
    }
}

struct FileChunks {
    file: File,
}

impl Iterator for FileChunks {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut bytes = vec![0_u8; 1024 * 1024];
        let count = self.file.read(&mut bytes).expect("read exact Qwen fixture");
        if count == 0 {
            return None;
        }
        bytes.truncate(count);
        Some(bytes)
    }
}

fn artifact_fact(identity: &ArtifactIdentityV0) -> FactValueV0 {
    FactValueV0::Digest(identity.digest.clone())
}

fn fake_model_owner(
    model_id: &str,
) -> (
    NativeModelOwner,
    NativeModelHandle,
    Arc<RequestRegistry>,
    Arc<AtomicBool>,
) {
    let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded(1);
    let worker_id = format!("w1-fake-model-worker-{model_id}");
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stopped = Arc::clone(&stopped);
    let join = std::thread::Builder::new()
        .name(worker_id.clone())
        .spawn(move || {
            crossbeam_channel::select_biased! {
                recv(shutdown_rx) -> _ => {},
                recv(command_rx) -> _ => {},
            }
            worker_stopped.store(true, Ordering::Release);
        })
        .expect("start deterministic fake model owner");
    let requests = Arc::new(RequestRegistry::with_external_worker(worker_id.clone()));
    let inner = Arc::new(NativeModelInner {
        worker_identity: Arc::new(WorkerIdentity),
        worker_id,
        command_tx,
        shutdown_tx,
        closing: AtomicBool::new(false),
        admission: Mutex::new(()),
        requests: Arc::clone(&requests),
        status: Arc::new(RwLock::new(ResidentModelStatus {
            model_id: model_id.to_owned(),
            model_path: PathBuf::new(),
            state: ModelRuntimeState::Ready,
            fingerprint: None,
            descriptor: None,
            active_sequences: 0,
            max_sequences: 1,
        })),
    });
    let handle = NativeModelHandle {
        inner: Arc::clone(&inner),
    };
    (
        NativeModelOwner {
            inner,
            join: Some(join),
        },
        handle,
        requests,
        stopped,
    )
}

fn terminal_name(class: RequestTerminalClass) -> &'static str {
    match class {
        RequestTerminalClass::Completed => "completed",
        RequestTerminalClass::Cancelled => "cancelled",
        RequestTerminalClass::Failed => "failed",
    }
}

fn observe_terminal(
    registry: &RequestRegistry,
    operation: &ControlledRequest,
    class: RequestTerminalClass,
) -> Result<RequestSnapshot, Box<dyn std::error::Error>> {
    registry.request_controlled_terminal(operation, class)?;
    let snapshot = registry.wait_controlled_released(operation, Duration::from_secs(5))?;
    assert_eq!(snapshot.phase, RequestPhase::Released);
    assert_eq!(
        snapshot
            .authoritative_terminal
            .map(|terminal| terminal.class),
        Some(class)
    );
    assert_eq!(snapshot.final_projection, snapshot.authoritative_terminal);
    Ok(snapshot)
}

fn receipt(
    sequence: u64,
    epoch: &str,
    kind: &str,
    operation_id: &str,
    terminal: RequestTerminalClass,
    ownership: OwnershipFactsV0,
) -> DurableReceipt {
    DurableReceipt {
        schema: RECEIPT_SCHEMA.to_owned(),
        sequence,
        epoch: epoch.to_owned(),
        kind: kind.to_owned(),
        operation_id: operation_id.to_owned(),
        terminal: terminal_name(terminal).to_owned(),
        active_operations: ownership.active_operations,
        retained_tasks: ownership.retained_tasks,
        expected_workers: ownership.expected_workers,
        joined_workers: ownership.joined_workers,
    }
}

fn worker_sets_match(joined: &crate::JoinedNativeModel) -> bool {
    joined.expected_worker_ids().iter().collect::<BTreeSet<_>>()
        == joined.joined_worker_ids().iter().collect::<BTreeSet<_>>()
}

#[test]
fn w1_quit_relaunch_fake_owners_manifest_is_authenticated() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(QUIT_RELAUNCH_MANIFEST_BYTES).expect("parse quit/relaunch manifest");
    validate_manifest(&manifest).expect("validate quit/relaunch manifest");
    let case = manifest.cases.first().expect("one quit/relaunch case");
    assert_eq!(manifest.vertical_id, VerticalIdV0::QuitRelaunchFakeOwners);
    assert_eq!(case.source.commit, QUIT_RELAUNCH_BASELINE_COMMIT);
    assert_eq!(
        sha256_identity("native-quit-relaunch-input", QUIT_RELAUNCH_INPUT_BYTES),
        case.inputs[0].identity
    );
    assert_eq!(
        sha256_identity(
            "native-quit-relaunch-source-tree",
            QUIT_RELAUNCH_SOURCE_TREE_BYTES,
        ),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity(
            "native-quit-relaunch-projection",
            QUIT_RELAUNCH_PROJECTION_BYTES,
        ),
        case.expected_projection
    );
    serde_json::from_slice::<EquivalenceProjectionV0>(QUIT_RELAUNCH_PROJECTION_BYTES)
        .expect("parse quit/relaunch projection");
}

#[test]
fn w1_quit_relaunch_fake_owners_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(QUIT_RELAUNCH_MANIFEST_BYTES)?;
    let input: QuitRelaunchInput = serde_json::from_slice(QUIT_RELAUNCH_INPUT_BYTES)?;
    let expected_projection: EquivalenceProjectionV0 =
        serde_json::from_slice(QUIT_RELAUNCH_PROJECTION_BYTES)?;
    let case = manifest.cases.first().expect("one quit/relaunch case");
    assert_eq!(input.schema, "delysis.native.quit_relaunch_input.v0");

    let directory = ReceiptDirectory::new()?;
    let store_path = directory.path.join(&input.receipt_file_name);
    let mut store = DurableReceiptStore::open(store_path.clone())?;
    assert!(store.receipts.is_empty());

    let (quit_owner, quit_handle, quit_registry, quit_model_stopped) =
        fake_model_owner(&input.quit_model_id);
    let quit_operation = quit_registry.spawn_controlled(&input.quit_operation_id)?;
    quit_owner.begin_shutdown();
    assert!(quit_registry.cancellation_requested_by_id(&input.quit_operation_id));
    assert_eq!(
        quit_handle
            .snapshot_sequence(0)
            .expect_err("quit closes model command admission")
            .code,
        NativeErrorCode::WorkerStopped
    );
    let quit_terminal = observe_terminal(
        &quit_registry,
        &quit_operation,
        RequestTerminalClass::Cancelled,
    )?;
    assert_eq!(quit_registry.active_count(), 0);
    assert_eq!(quit_registry.retained_task_count(), 1);
    store.append(receipt(
        0,
        "quit",
        "terminal",
        &quit_terminal.identity.operation_id,
        RequestTerminalClass::Cancelled,
        OwnershipFactsV0 {
            active_operations: quit_registry.active_count(),
            retained_tasks: quit_registry.retained_task_count(),
            expected_workers: 2,
            joined_workers: 0,
        },
    ))?;
    quit_registry.allow_controlled_exit(&quit_operation)?;
    quit_registry.reap_controlled(&quit_operation)?;
    let quit_joined = quit_owner.shutdown_joined()?;
    assert!(quit_model_stopped.load(Ordering::Acquire));
    assert!(quit_joined.belongs_to(&quit_handle));
    assert_eq!(quit_joined.expected_worker_count(), 2);
    assert_eq!(quit_joined.joined_worker_count(), 2);
    assert!(worker_sets_match(&quit_joined));
    assert_eq!(quit_registry.active_count(), 0);
    assert_eq!(quit_registry.retained_task_count(), 0);
    store.append(receipt(
        1,
        "quit",
        "joined",
        &input.quit_operation_id,
        RequestTerminalClass::Cancelled,
        OwnershipFactsV0 {
            active_operations: quit_registry.active_count(),
            retained_tasks: quit_registry.retained_task_count(),
            expected_workers: quit_joined.expected_worker_count(),
            joined_workers: quit_joined.joined_worker_count(),
        },
    ))?;
    let quit_store_bytes = store.bytes()?;
    let quit_store_identity =
        sha256_identity("native-fake-owner-store-after-quit", &quit_store_bytes);
    drop(store);

    let mut relaunched_store = DurableReceiptStore::open(store_path)?;
    assert_eq!(relaunched_store.receipts.len(), 2);
    assert_eq!(relaunched_store.bytes()?, quit_store_bytes);
    let (relaunch_owner, relaunch_handle, relaunch_registry, relaunch_model_stopped) =
        fake_model_owner(&input.relaunch_model_id);
    assert!(!quit_handle.is_same_worker(&relaunch_handle));
    let relaunch_operation = relaunch_registry.spawn_controlled(&input.relaunch_operation_id)?;
    let relaunch_terminal = observe_terminal(
        &relaunch_registry,
        &relaunch_operation,
        RequestTerminalClass::Completed,
    )?;
    assert_eq!(relaunch_registry.active_count(), 0);
    assert_eq!(relaunch_registry.retained_task_count(), 1);
    relaunched_store.append(receipt(
        2,
        "relaunch",
        "terminal",
        &relaunch_terminal.identity.operation_id,
        RequestTerminalClass::Completed,
        OwnershipFactsV0 {
            active_operations: relaunch_registry.active_count(),
            retained_tasks: relaunch_registry.retained_task_count(),
            expected_workers: 2,
            joined_workers: 0,
        },
    ))?;
    relaunch_registry.allow_controlled_exit(&relaunch_operation)?;
    relaunch_registry.reap_controlled(&relaunch_operation)?;
    relaunch_owner.begin_shutdown();
    assert_eq!(
        relaunch_handle
            .snapshot_sequence(0)
            .expect_err("relaunch quit closes model command admission")
            .code,
        NativeErrorCode::WorkerStopped
    );
    let relaunch_joined = relaunch_owner.shutdown_joined()?;
    assert!(relaunch_model_stopped.load(Ordering::Acquire));
    assert!(relaunch_joined.belongs_to(&relaunch_handle));
    assert_eq!(relaunch_joined.expected_worker_count(), 2);
    assert_eq!(relaunch_joined.joined_worker_count(), 2);
    assert!(worker_sets_match(&relaunch_joined));
    assert_eq!(relaunch_registry.active_count(), 0);
    assert_eq!(relaunch_registry.retained_task_count(), 0);
    relaunched_store.append(receipt(
        3,
        "relaunch",
        "joined",
        &input.relaunch_operation_id,
        RequestTerminalClass::Completed,
        OwnershipFactsV0 {
            active_operations: relaunch_registry.active_count(),
            retained_tasks: relaunch_registry.retained_task_count(),
            expected_workers: relaunch_joined.expected_worker_count(),
            joined_workers: relaunch_joined.joined_worker_count(),
        },
    ))?;
    let final_store_bytes = relaunched_store.bytes()?;
    let final_store_identity =
        sha256_identity("native-fake-owner-store-after-relaunch", &final_store_bytes);

    let projection = quit_relaunch_projection(
        &input,
        quit_store_identity,
        final_store_identity.clone(),
        quit_joined.expected_worker_count() + relaunch_joined.expected_worker_count(),
        quit_joined.joined_worker_count() + relaunch_joined.joined_worker_count(),
    );
    assert_eq!(projection, expected_projection);
    let observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: VerticalIdV0::QuitRelaunchFakeOwners,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: Vec::new(),
        evidence: EvidenceClaimV0 {
            schema: "delysis.evidence_claim.v0".to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: "deterministic fake native owners with filesystem receipt replay"
                .to_owned(),
            exact_source: case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: final_store_identity.digest,
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection,
    };
    validate_baseline(
        &manifest,
        &case.case_id,
        QUIT_RELAUNCH_PROJECTION_BYTES,
        &[],
        &observation,
    )?;
    Ok(())
}

fn quit_relaunch_projection(
    input: &QuitRelaunchInput,
    quit_store_identity: ArtifactIdentityV0,
    final_store_identity: ArtifactIdentityV0,
    expected_workers: usize,
    joined_workers: usize,
) -> EquivalenceProjectionV0 {
    let operation_facts = [
        (
            input.quit_operation_id.as_str(),
            TerminalClass::Cancelled,
            "quit_terminal",
            0,
        ),
        (
            input.relaunch_operation_id.as_str(),
            TerminalClass::Completed,
            "relaunch_completed",
            2,
        ),
    ];
    let mut ordered_events = Vec::new();
    let mut lifecycle = Vec::new();
    for (operation_id, terminal, kind, sequence) in operation_facts {
        ordered_events.push(EventFactV0 {
            sequence,
            operation_id: operation_id.to_owned(),
            attempt_id: None,
            correlation_id: Some("native-quit-relaunch".to_owned()),
            kind: kind.to_owned(),
            payload: None,
        });
        ordered_events.push(EventFactV0 {
            sequence: sequence + 1,
            operation_id: operation_id.to_owned(),
            attempt_id: None,
            correlation_id: Some("native-quit-relaunch".to_owned()),
            kind: "owner_joined".to_owned(),
            payload: None,
        });
        lifecycle.push(LifecycleFactV0 {
            operation_id: operation_id.to_owned(),
            attempt_id: None,
            correlation_id: Some("native-quit-relaunch".to_owned()),
            terminal,
            released: true,
        });
    }
    let mut output_facts = BTreeMap::new();
    output_facts.insert("fake_model_owners".to_owned(), FactValueV0::Integer(2));
    output_facts.insert(
        "fresh_owner_identity".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert("real_model_loaded".to_owned(), FactValueV0::Boolean(false));
    output_facts.insert(
        "relaunch_operation_completed".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert(
        "same_durable_store_reopened".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert("durable_receipts".to_owned(), FactValueV0::Integer(4));
    output_facts.insert(
        "worker_id_sets_match".to_owned(),
        FactValueV0::Boolean(true),
    );
    EquivalenceProjectionV0 {
        ordered_events,
        durable_state: vec![
            DurableStateFactV0 {
                state_id: "receipt-store-after-quit".to_owned(),
                schema_id: "delysis.native.fake_owner_receipt_store.v0".to_owned(),
                before: None,
                after: Some(quit_store_identity.clone()),
                disposition: StateDispositionV0::Created,
            },
            DurableStateFactV0 {
                state_id: "receipt-store-after-relaunch".to_owned(),
                schema_id: "delysis.native.fake_owner_receipt_store.v0".to_owned(),
                before: Some(quit_store_identity),
                after: Some(final_store_identity),
                disposition: StateDispositionV0::Updated,
            },
        ],
        lifecycle,
        ownership: OwnershipFactsV0 {
            active_operations: 0,
            retained_tasks: 0,
            expected_workers,
            joined_workers,
        },
        output_facts,
        fail_closed_facts: vec![
            "quit closes model command admission before joining workers".to_owned(),
            "joined evidence requires exact expected and joined worker ID sets".to_owned(),
        ],
    }
}

#[test]
fn w1_current_exact_qwen_manifest_is_authenticated() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(MANIFEST_BYTES).expect("parse exact-Qwen manifest");
    validate_manifest(&manifest).expect("validate exact-Qwen manifest");
    let case = manifest.cases.first().expect("one exact-Qwen case");
    assert_eq!(manifest.vertical_id, VerticalIdV0::CurrentExactQwen);
    assert_eq!(
        sha256_identity("native-current-qwen-input", INPUT_BYTES),
        case.inputs[0].identity
    );
    assert_eq!(
        sha256_identity("llama-native-engine-source-tree", SOURCE_TREE_BYTES),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity("native-current-qwen-projection", EXPECTED_PROJECTION_BYTES),
        case.expected_projection
    );
    serde_json::from_slice::<EquivalenceProjectionV0>(EXPECTED_PROJECTION_BYTES)
        .expect("parse exact-Qwen projection");

    let fixtures = [
        ("native-current-qwen-input-v0.json", INPUT_BYTES),
        ("native-current-qwen-manifest-v0.json", MANIFEST_BYTES),
        (
            "native-current-qwen-projection-v0.json",
            EXPECTED_PROJECTION_BYTES,
        ),
        ("native-current-qwen-source-tree-v0.txt", SOURCE_TREE_BYTES),
        (
            "native-quit-relaunch-input-v0.json",
            QUIT_RELAUNCH_INPUT_BYTES,
        ),
        (
            "native-quit-relaunch-manifest-v0.json",
            QUIT_RELAUNCH_MANIFEST_BYTES,
        ),
        (
            "native-quit-relaunch-projection-v0.json",
            QUIT_RELAUNCH_PROJECTION_BYTES,
        ),
        (
            "native-quit-relaunch-source-tree-v0.txt",
            QUIT_RELAUNCH_SOURCE_TREE_BYTES,
        ),
    ];
    let lines = BUNDLE_MANIFEST.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), fixtures.len());
    for (line, (name, bytes)) in lines.into_iter().zip(fixtures) {
        let (expected_digest, listed_name) = line
            .split_once("  ")
            .expect("bundle manifest uses sha256sum format");
        assert_eq!(listed_name, name);
        assert_eq!(sha256_identity(name, bytes).digest.hex, expected_digest);
    }
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH, MOM_LLAMA_MODEL_SHA256, and the exact W1 Qwen GGUF"]
fn w1_current_exact_qwen_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(MANIFEST_BYTES)?;
    let input: InputFixture = serde_json::from_slice(INPUT_BYTES)?;
    let expected_projection: EquivalenceProjectionV0 =
        serde_json::from_slice(EXPECTED_PROJECTION_BYTES)?;
    let case = manifest
        .cases
        .first()
        .expect("the exact-Qwen manifest has one case");
    assert_eq!(manifest.vertical_id, VerticalIdV0::CurrentExactQwen);
    assert_eq!(input.schema, "delysis.native.current_qwen_input.v0");
    assert_eq!(
        sha256_identity("native-current-qwen-input", INPUT_BYTES),
        case.inputs[0].identity
    );
    assert_eq!(
        sha256_identity("llama-native-engine-source-tree", SOURCE_TREE_BYTES),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity("native-current-qwen-projection", EXPECTED_PROJECTION_BYTES),
        case.expected_projection
    );

    let prerequisite = case.prerequisites.first().expect("exact Qwen prerequisite");
    assert_eq!(
        input.required_model_sha256,
        prerequisite.identity.digest.hex
    );
    let model_path = PathBuf::from(std::env::var("MOM_LLAMA_MODEL_PATH")?);
    assert_eq!(
        std::env::var("MOM_LLAMA_MODEL_SHA256")?,
        prerequisite.identity.digest.hex
    );
    let verified_model = verify_prerequisite_chunks(
        prerequisite.prerequisite_id.clone(),
        &prerequisite.identity,
        FileChunks {
            file: File::open(&model_path)?,
        },
    )?;

    let mut config = NativeModelConfig::local(model_path);
    config.expected_model_sha256 = Some(prerequisite.identity.digest.hex.clone());
    config.device = NativeDevice::Cpu;
    let owner = NativeModelOwner::load(config)?;
    let handle = owner.handle();
    let status = handle.status();
    let fingerprint = status
        .fingerprint
        .as_ref()
        .expect("an admitted real model has a fingerprint");
    assert_eq!(fingerprint.model_sha256, prerequisite.identity.digest.hex);

    let prepared = handle.prepare_input(GenerationInput::Completion {
        prompts: vec![CompletionPrompt::Text {
            text: input.prompt,
            special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
        }],
    })?;
    let prompt_tokens = prepared
        .first()
        .expect("one prepared exact-Qwen prompt")
        .token_ids
        .clone();
    assert!(!prompt_tokens.is_empty());

    let operation_id = "native-current-qwen";
    let request = GenerationBatchRequest {
        request_id: operation_id.to_owned(),
        model_id: status.model_id.clone(),
        cases: vec![GenerationCase {
            case_id: operation_id.to_owned(),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens {
                    token_ids: prompt_tokens,
                }],
            },
            sampling: SamplingConfig {
                seed: input.seed,
                temperature: input.temperature,
                max_tokens: input.max_tokens,
                ..SamplingConfig::default()
            },
            cached_prefix: None,
        }],
    };
    let verified = handle.generate_batch(request.clone())?.wait_verified()?;
    assert_eq!(verified.request(), &request);
    assert_eq!(verified.model_fingerprint(), fingerprint);
    let result = verified.outputs().first().expect("one exact-Qwen result");
    assert_eq!(result.state, GenerationState::Completed);
    assert!(result.real_engine_invoked);
    assert!(!result.fake_fixture);
    assert_eq!(result.transport, NativeTransport::InProcess);
    assert!(!result.text.trim().is_empty());
    let terminal_events = verified
        .events()
        .iter()
        .filter(|event| {
            event.request_id == operation_id
                && event.branch_id == operation_id
                && matches!(
                    event.event,
                    GenerationEventKind::State {
                        state: GenerationState::Completed
                    }
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_events.len(), 1);
    let active_operations = handle.status().active_sequences;
    assert_eq!(active_operations, 0);

    let fim_error = handle
        .generate(GenerationRequest {
            request_id: "native-current-qwen-fim-denied".to_owned(),
            model_id: status.model_id,
            input: GenerationInput::FillInMiddle {
                prefix: "fn main() {".to_owned(),
                suffix: "}".to_owned(),
            },
            sampling: SamplingConfig::default(),
            media: Vec::new(),
            cached_prefix: None,
        })
        .expect_err("unverified fill-in-middle must fail closed");
    assert_eq!(fim_error.code, NativeErrorCode::UnsupportedPromptForm);
    let joined = owner.shutdown_joined()?;
    assert!(joined.belongs_to(&handle));
    assert_eq!(joined.model_id(), request.model_id);
    assert_eq!(joined.expected_worker_ids(), joined.joined_worker_ids());
    assert_eq!(joined.expected_worker_count(), joined.joined_worker_count());
    let retained_tasks = joined
        .expected_worker_count()
        .saturating_sub(joined.joined_worker_count());

    let mut output_facts = BTreeMap::new();
    output_facts.insert(
        "exact_model_sha256".to_owned(),
        artifact_fact(&prerequisite.identity),
    );
    output_facts.insert("fake_fixture".to_owned(), FactValueV0::Boolean(false));
    output_facts.insert(
        "generated_text_nonempty".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert("real_engine_invoked".to_owned(), FactValueV0::Boolean(true));
    output_facts.insert(
        "transport".to_owned(),
        FactValueV0::Text("in_process".to_owned()),
    );
    output_facts.insert(
        "unsupported_fim_denied".to_owned(),
        FactValueV0::Boolean(true),
    );
    let projection = EquivalenceProjectionV0 {
        ordered_events: vec![EventFactV0 {
            sequence: 0,
            operation_id: operation_id.to_owned(),
            attempt_id: None,
            correlation_id: None,
            kind: "completed".to_owned(),
            payload: None,
        }],
        durable_state: vec![DurableStateFactV0 {
            state_id: "exact-model-artifact".to_owned(),
            schema_id: "gguf-v3".to_owned(),
            before: Some(prerequisite.identity.clone()),
            after: Some(prerequisite.identity.clone()),
            disposition: StateDispositionV0::Unchanged,
        }],
        lifecycle: vec![LifecycleFactV0 {
            operation_id: operation_id.to_owned(),
            attempt_id: None,
            correlation_id: None,
            terminal: match result.state {
                GenerationState::Completed => TerminalClass::Completed,
                _ => unreachable!("the strict seal admitted one completed result"),
            },
            released: joined.expected_worker_ids() == joined.joined_worker_ids(),
        }],
        ownership: OwnershipFactsV0 {
            active_operations,
            retained_tasks,
            expected_workers: joined.expected_worker_count(),
            joined_workers: joined.joined_worker_count(),
        },
        output_facts,
        fail_closed_facts: vec![
            "exact model digest is verified before runtime admission".to_owned(),
            "unverified fill-in-middle is rejected".to_owned(),
        ],
    };
    assert_eq!(projection, expected_projection);
    let observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: VerticalIdV0::CurrentExactQwen,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: case.prerequisites.clone(),
        evidence: EvidenceClaimV0 {
            schema: "delysis.evidence_claim.v0".to_owned(),
            tier: EvidenceTier::Operational,
            threat_model: "exact local GGUF execution on the product owner thread".to_owned(),
            exact_source: case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: prerequisite.identity.digest.clone(),
            execution_kind: ExecutionKind::LocalRuntime,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection,
    };
    validate_baseline(
        &manifest,
        &case.case_id,
        EXPECTED_PROJECTION_BYTES,
        &[verified_model],
        &observation,
    )?;
    Ok(())
}
