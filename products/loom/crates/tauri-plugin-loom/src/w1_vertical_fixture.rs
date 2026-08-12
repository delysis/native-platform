use super::*;
use loom_host::{
    GenerationOperationLease, GenerationOperationSnapshot, GenerationSupervisorClosedFacts,
    GenerationSupervisorPhase,
};
use platform_contracts_v0_vertical::{
    EvidenceClaimV0, EvidenceTier, ExecutionKind, TerminalClass, evidence::EVIDENCE_SCHEMA_V0,
};
use platform_vertical_fixtures_v0::{
    DurableStateFactV0, EquivalenceProjectionV0, EventFactV0, FactValueV0, LifecycleFactV0,
    ObservationEnvelopeV0, OwnershipFactsV0, StateDispositionV0, VERTICAL_OBSERVATION_SCHEMA_V0,
    VerticalFixtureManifestV0, VerticalIdV0, sha256_identity, validate_baseline,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

const BASELINE_COMMIT: &str = "a210de57a5a289db74a9cc280839e75fdf5f90db";
const INPUT_BYTES: &[u8] = include_bytes!("../../../fixtures/w1/loom-quit-relaunch-v1.json");
const MANIFEST_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/manifests/loom-quit-relaunch-v0.json");
const PROJECTION_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/projections/loom-quit-relaunch-v1.json");
const SOURCE_DESCRIPTOR_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/source/loom-row8-production-tree-a210de5.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureInput {
    schema: String,
    cancelled_request_id: String,
    relaunched_request_id: String,
}

#[derive(Debug)]
struct FixtureCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ControlledGenerationWorkerCancellation for FixtureCancellation {
    fn cancel_all(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Debug, Default)]
struct WorkerGate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl WorkerGate {
    fn wait(&self) {
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*open {
            open = self
                .changed
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn release(&self) {
        *self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.changed.notify_all();
    }
}

#[derive(Debug)]
struct WorkerEvidence {
    lease: GenerationOperationLease,
    cancelled: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    gate: Option<Arc<WorkerGate>>,
}

#[derive(Debug)]
struct CloseEvidence {
    terminal: GenerationOperationSnapshot,
    closed: GenerationSupervisorClosedFacts,
    expected_worker_ids: Vec<String>,
    joined_worker_ids: Vec<String>,
    joined_worker_count: usize,
    admission_closed: bool,
}

fn attach_fixture_worker(
    state: &PluginState,
    request_id: &str,
    terminal: GenerationTerminalClass,
    wait_for_cancellation: bool,
) -> WorkerEvidence {
    let (ticket, lease) = state
        .generation_lifecycle
        .reserve(request_id)
        .expect("reserve fixture generation lifecycle");
    state
        .generation_lifecycle
        .queue(&lease)
        .expect("queue fixture generation lifecycle");
    state
        .generation_lifecycle
        .start(&lease)
        .expect("start fixture generation lifecycle");
    let admission =
        lock_application_admission(state, "fixture generation").expect("admit fixture worker");
    let reservation = state
        .generation_workers
        .reserve(request_id, &admission)
        .expect("reserve fixture desktop worker");
    let cancelled = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let gate = (!wait_for_cancellation).then(|| Arc::new(WorkerGate::default()));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker_finished = Arc::clone(&finished);
    let worker_gate = gate.clone();
    let lifecycle = state.generation_lifecycle.clone();
    let worker_lease = lease.clone();
    let worker = std::thread::Builder::new()
        .name(format!("loom-w1-{request_id}"))
        .spawn(move || {
            if wait_for_cancellation {
                while !worker_cancelled.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            } else if let Some(gate) = worker_gate {
                gate.wait();
            }
            lifecycle
                .terminal_and_release(&worker_lease, terminal)
                .expect("fixture worker publishes terminal before exit");
            worker_finished.store(true, Ordering::Release);
        })
        .expect("spawn fixture generation worker");
    reservation
        .attach(
            worker,
            GenerationWorkerOwner::controlled(Arc::new(FixtureCancellation {
                cancelled: Arc::clone(&cancelled),
            })),
        )
        .map_err(|error| error.failure)
        .expect("attach fixture generation worker");
    ticket.detach();
    drop(admission);
    WorkerEvidence {
        lease,
        cancelled,
        finished,
        gate,
    }
}

fn close_runtime(state: &PluginState, worker: &WorkerEvidence) -> CloseEvidence {
    let close = begin_application_close(state).expect("begin full application close");
    state
        .generation_lifecycle
        .quiesce()
        .expect("quiesce generation lifecycle");
    let admission_closed = lock_application_admission(state, "late fixture work").is_err();
    let desktop_workers = state
        .join_desktop_workers()
        .expect("cancel and join every desktop worker");
    let terminal = state
        .generation_lifecycle
        .wait_released(&worker.lease, Duration::from_secs(2))
        .expect("wait for fixture release")
        .expect("fixture terminal snapshot");
    let expected_worker_ids = desktop_workers
        .generation_workers
        .expected_worker_ids
        .clone();
    let joined_worker_ids = desktop_workers.generation_workers.joined_worker_ids.clone();
    let native_runtime = state
        .native_runtime
        .shutdown_joined()
        .expect("join empty native runtime");
    let proof = ApplicationShutdownProof::from_graceful(native_runtime, desktop_workers);
    let joined_worker_count = proof.joined_worker_count();
    let closed = state
        .generation_lifecycle
        .close()
        .expect("close drained generation lifecycle");
    let ready = close.authorize(proof);
    assert_eq!(
        *state.application.lock().expect("application phase"),
        ApplicationPhase::ExitAuthorized
    );
    drop(ready);
    CloseEvidence {
        terminal,
        closed,
        expected_worker_ids,
        joined_worker_ids,
        joined_worker_count,
        admission_closed,
    }
}

#[test]
fn full_application_close_joins_fake_generation_owner_and_fresh_runtime_admits_work() {
    let input: FixtureInput = serde_json::from_slice(INPUT_BYTES).expect("parse fixture input");
    assert_eq!(input.schema, "delysis.loom.quit_relaunch_fixture.v1");

    let first = PluginState::default();
    let cancelled = attach_fixture_worker(
        &first,
        &input.cancelled_request_id,
        GenerationTerminalClass::Cancelled,
        true,
    );
    let first_close = close_runtime(&first, &cancelled);
    assert!(cancelled.cancelled.load(Ordering::Acquire));
    assert!(cancelled.finished.load(Ordering::Acquire));
    assert_eq!(
        first_close.closed.lifecycle,
        GenerationSupervisorPhase::Closed
    );
    assert_eq!(first_close.closed.active_operations, 0);
    assert_eq!(first_close.closed.retained_tasks, 0);
    assert_eq!(
        first_close.closed.expected_workers,
        first_close.closed.joined_workers
    );
    assert_eq!(
        first_close.expected_worker_ids,
        first_close.joined_worker_ids
    );
    assert_eq!(first_close.joined_worker_count, 1);

    let fresh = PluginState::default();
    let fresh_runtime_running = fresh
        .generation_lifecycle
        .phase()
        .expect("fresh lifecycle phase")
        == GenerationSupervisorPhase::Running;
    assert!(fresh_runtime_running);
    ensure_application_running(&fresh, "fresh fixture work").expect("fresh runtime admits work");
    let completed = attach_fixture_worker(
        &fresh,
        &input.relaunched_request_id,
        GenerationTerminalClass::Completed,
        false,
    );
    completed.gate.as_ref().expect("completion gate").release();
    let fresh_close = close_runtime(&fresh, &completed);
    assert!(completed.finished.load(Ordering::Acquire));
    assert_eq!(
        fresh_close.closed.lifecycle,
        GenerationSupervisorPhase::Closed
    );
    assert_eq!(fresh_close.closed.active_operations, 0);
    assert_eq!(fresh_close.closed.retained_tasks, 0);
    assert_eq!(
        fresh_close.closed.expected_workers,
        fresh_close.closed.joined_workers
    );
    assert_eq!(
        fresh_close.expected_worker_ids,
        fresh_close.joined_worker_ids
    );
    assert_eq!(fresh_close.joined_worker_count, 1);

    validate_projection(quit_relaunch_projection(
        &cancelled,
        &first_close,
        &completed,
        &fresh_close,
        fresh_runtime_running,
    ));
}

fn quit_relaunch_projection(
    cancelled: &WorkerEvidence,
    first_close: &CloseEvidence,
    completed: &WorkerEvidence,
    fresh_close: &CloseEvidence,
    fresh_runtime_running: bool,
) -> EquivalenceProjectionV0 {
    let correlation_id = Some("loom-w1-quit-relaunch".to_owned());
    EquivalenceProjectionV0 {
        ordered_events: vec![
            EventFactV0 {
                sequence: 0,
                operation_id: cancelled.lease.identity().operation_id.clone(),
                attempt_id: Some(cancelled.lease.identity().attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "cancelled".to_owned(),
                payload: None,
            },
            EventFactV0 {
                sequence: 1,
                operation_id: completed.lease.identity().operation_id.clone(),
                attempt_id: Some(completed.lease.identity().attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "completed".to_owned(),
                payload: None,
            },
        ],
        durable_state: vec![
            DurableStateFactV0 {
                state_id: "loom.application.lifecycle.first".to_owned(),
                schema_id: "loom.application_lifecycle.v1".to_owned(),
                before: Some(sha256_identity("loom.lifecycle.running", b"running")),
                after: Some(sha256_identity("loom.lifecycle.closed", b"closed")),
                disposition: StateDispositionV0::Updated,
            },
            DurableStateFactV0 {
                state_id: "loom.application.lifecycle.fresh".to_owned(),
                schema_id: "loom.application_lifecycle.v1".to_owned(),
                before: Some(sha256_identity("loom.lifecycle.running", b"running")),
                after: Some(sha256_identity("loom.lifecycle.closed", b"closed")),
                disposition: StateDispositionV0::Updated,
            },
        ],
        lifecycle: vec![
            LifecycleFactV0 {
                operation_id: cancelled.lease.identity().operation_id.clone(),
                attempt_id: Some(cancelled.lease.identity().attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                terminal: terminal_class(&first_close.terminal),
                released: true,
            },
            LifecycleFactV0 {
                operation_id: completed.lease.identity().operation_id.clone(),
                attempt_id: Some(completed.lease.identity().attempt_id.clone()),
                correlation_id,
                terminal: terminal_class(&fresh_close.terminal),
                released: true,
            },
        ],
        ownership: OwnershipFactsV0 {
            active_operations: first_close.closed.active_operations
                + fresh_close.closed.active_operations,
            retained_tasks: first_close.closed.retained_tasks + fresh_close.closed.retained_tasks,
            expected_workers: first_close.expected_worker_ids.len()
                + fresh_close.expected_worker_ids.len(),
            joined_workers: first_close.joined_worker_ids.len()
                + fresh_close.joined_worker_ids.len(),
        },
        output_facts: quit_relaunch_output_facts(
            cancelled,
            first_close,
            completed,
            fresh_close,
            fresh_runtime_running,
        ),
        fail_closed_facts: vec![
            "application admission closed before fake-owner cancellation and join".to_owned(),
            "fresh runtime did not reuse the closed generation supervisor".to_owned(),
        ],
    }
}

fn quit_relaunch_output_facts(
    cancelled: &WorkerEvidence,
    first_close: &CloseEvidence,
    completed: &WorkerEvidence,
    fresh_close: &CloseEvidence,
    fresh_runtime_running: bool,
) -> BTreeMap<String, FactValueV0> {
    BTreeMap::from([
        (
            "first_admission_closed".to_owned(),
            FactValueV0::Boolean(first_close.admission_closed),
        ),
        (
            "first_cancellation_observed".to_owned(),
            FactValueV0::Boolean(cancelled.cancelled.load(Ordering::Acquire)),
        ),
        (
            "first_expected_joined_exact".to_owned(),
            FactValueV0::Boolean(first_close.expected_worker_ids == first_close.joined_worker_ids),
        ),
        (
            "first_lifecycle_closed".to_owned(),
            FactValueV0::Boolean(first_close.closed.lifecycle == GenerationSupervisorPhase::Closed),
        ),
        (
            "fresh_completed".to_owned(),
            FactValueV0::Boolean(completed.finished.load(Ordering::Acquire)),
        ),
        (
            "fresh_expected_joined_exact".to_owned(),
            FactValueV0::Boolean(fresh_close.expected_worker_ids == fresh_close.joined_worker_ids),
        ),
        (
            "fresh_runtime_running".to_owned(),
            FactValueV0::Boolean(fresh_runtime_running),
        ),
    ])
}

fn terminal_class(snapshot: &GenerationOperationSnapshot) -> TerminalClass {
    match snapshot
        .authoritative_terminal
        .as_ref()
        .expect("authoritative terminal")
        .class
    {
        GenerationTerminalClass::Completed => TerminalClass::Completed,
        GenerationTerminalClass::Cancelled => TerminalClass::Cancelled,
        GenerationTerminalClass::Failed => TerminalClass::Failed,
    }
}

fn validate_projection(projection: EquivalenceProjectionV0) {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(MANIFEST_BYTES).expect("parse quit/relaunch manifest");
    let case = manifest.cases.first().expect("one Loom row-8 case");
    assert_eq!(manifest.vertical_id, VerticalIdV0::QuitRelaunchFakeOwners);
    assert_eq!(case.source.commit, BASELINE_COMMIT);
    assert_eq!(
        sha256_identity(
            case.source.production_tree.id.clone(),
            SOURCE_DESCRIPTOR_BYTES
        ),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity("loom.quit-relaunch.input", INPUT_BYTES),
        case.inputs[0].identity
    );
    assert_eq!(
        sha256_identity("loom.quit-relaunch.projection", PROJECTION_BYTES),
        case.expected_projection
    );
    authenticate_production_root();
    let observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: VerticalIdV0::QuitRelaunchFakeOwners,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: Vec::new(),
        evidence: EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model:
                "feature-gated deterministic Loom fake owner; no model, network, credential, or GUI process authority"
                    .to_owned(),
            exact_source: case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: case.expected_projection.digest.clone(),
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection,
    };
    validate_baseline(
        &manifest,
        &case.case_id,
        PROJECTION_BYTES,
        &[],
        &observation,
    )
    .expect("validate exact Loom row-8 projection");
}

fn authenticate_production_root() {
    let descriptor: serde_json::Value =
        serde_json::from_slice(SOURCE_DESCRIPTOR_BYTES).expect("parse source descriptor");
    let source_roots = descriptor["source_roots"]
        .as_object()
        .expect("source_roots object");
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    for (path, expected) in source_roots {
        let expected = expected.as_str().expect("production source object ID");
        let revision_path = format!("{BASELINE_COMMIT}:{path}");
        let output = Command::new("git")
            .args(["rev-parse", &revision_path])
            .current_dir(repository)
            .output()
            .expect("read baseline production source object");
        assert!(
            output.status.success(),
            "baseline production source exists: {path}"
        );
        assert_eq!(
            std::str::from_utf8(&output.stdout)
                .expect("UTF-8 object ID")
                .trim(),
            expected,
            "production source object drifted: {path}"
        );
    }
}
