use super::*;
use loom_cabal::compute::{ComputeGrant, ComputeInput, ComputeReply, ComputeStatus};
use loom_cabal::{Cabal, Identity, Network, NetworkMode};
use uuid::Uuid;

fn input_job(model: ComputeModel) -> HostComputeJob {
    let peer = Identity::generate().expect("test identity").public_key();
    HostComputeJob {
        id: Uuid::new_v4(),
        peer,
        grant: ComputeGrant {
            id: Uuid::new_v4(),
            cabal: Uuid::new_v4(),
            epoch: 0,
            peer,
            model,
            max_output_tokens: 16,
            max_seconds: 120,
            jobs: 1,
        },
        input: ComputeInput {
            prompt: "@Private =function() stays literal. 🌱".into(),
            max_output_tokens: 16,
            seed: 42,
        },
    }
}

#[test]
fn foreground_priority_cancels_and_drains_without_admitting_a_replacement_job() {
    let owner = Arc::new(IdleComputeOwner::new(Duration::ZERO));
    let job = owner.reserve(&CancellationToken::new()).expect("idle job");
    let (send, receive) = std::sync::mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..2 {
        let owner = owner.clone();
        let send = send.clone();
        threads.push(std::thread::spawn(move || {
            send.send(owner.foreground()).expect("foreground observer");
        }));
    }
    tauri::async_runtime::block_on(async {
        tokio::time::timeout(Duration::from_secs(2), job.cancel.cancelled())
            .await
            .expect("preemption signal");
    });
    assert!(
        receive.recv_timeout(Duration::from_millis(25)).is_err(),
        "foreground must wait for the job owner"
    );
    assert!(owner.reserve(&CancellationToken::new()).is_err());
    drop(job);
    let first = receive
        .recv_timeout(Duration::from_secs(2))
        .expect("first foreground owner");
    let second = receive
        .recv_timeout(Duration::from_secs(2))
        .expect("second foreground owner");
    drop(first);
    assert!(!owner.idle());
    drop(second);
    assert!(owner.reserve(&CancellationToken::new()).is_ok());
    for thread in threads {
        thread.join().expect("joined foreground observer");
    }
}

#[test]
fn closing_waits_for_the_exact_job_lease_and_permanently_stops_admission() {
    let owner = Arc::new(IdleComputeOwner::new(Duration::ZERO));
    let job = owner.reserve(&CancellationToken::new()).expect("idle job");
    let closed_owner = owner.clone();
    let close = std::thread::spawn(move || closed_owner.close_and_drain());
    tauri::async_runtime::block_on(async {
        tokio::time::timeout(Duration::from_secs(2), job.cancel.cancelled())
            .await
            .expect("close signal");
    });
    assert!(!close.is_finished());
    drop(job);
    close.join().expect("joined close");
    assert!(!owner.idle());
    assert!(owner.reserve(&CancellationToken::new()).is_err());
}

#[test]
fn reserved_peer_job_cannot_deadlock_foreground_application_admission() {
    let directory = tempfile::tempdir().expect("fixture");
    let mut state = PluginState::with_app_local_data_root(
        Some(directory.path().into()),
        true,
        BuildModelPolicy::default(),
    );
    state.peer_compute = Arc::new(IdleComputeOwner::new(Duration::ZERO));
    let executor = NativeExecutor::from_state(&state);
    let model = crate::tests::test_loaded_model(Path::new("not-a-real-model.gguf"), "test-model");
    let job = input_job(model_claim(&model).expect("model claim"));
    *state.model.lock().expect("model registry") = ModelRegistry::Loaded(Box::new(model));
    let application = state.application.lock().expect("foreground owns admission");
    let result = tauri::async_runtime::block_on(executor.execute(job, CancellationToken::new()));
    assert_eq!(result, Err(ComputeFailure::HostBusy));
    assert!(state.peer_compute.idle());
    assert!(!directory.path().join("peer-compute-writing").exists());
    drop(application);
}

#[test]
fn model_grant_cannot_follow_a_different_loaded_model() {
    let directory = tempfile::tempdir().expect("fixture");
    let mut state = PluginState::with_app_local_data_root(
        Some(directory.path().into()),
        true,
        BuildModelPolicy::default(),
    );
    state.peer_compute = Arc::new(IdleComputeOwner::new(Duration::ZERO));
    let executor = NativeExecutor::from_state(&state);
    let first = crate::tests::test_loaded_model(Path::new("first.gguf"), "first");
    let second = crate::tests::test_loaded_model(Path::new("second.gguf"), "second");
    let job = input_job(model_claim(&first).expect("first model"));
    *state.model.lock().expect("model registry") = ModelRegistry::Loaded(Box::new(second));
    assert!(!executor.available(&job.grant.model));
    assert_eq!(
        tauri::async_runtime::block_on(executor.execute(job, CancellationToken::new())),
        Err(ComputeFailure::ModelUnavailable)
    );
    assert!(!directory.path().join("peer-compute-writing").exists());
}

#[cfg(unix)]
#[test]
fn peer_input_is_literal_derived_writing_with_private_provenance_and_no_duplicate_execution() {
    let directory = tempfile::tempdir().expect("fixture");
    let mut store = private_store(directory.path()).expect("private store");
    let model = crate::tests::test_loaded_model(Path::new("test-model.gguf"), "test-model");
    let job = input_job(model_claim(&model).expect("model claim"));
    let prepared = prepare(&mut store, &model, &job).expect("prepare literal input");
    assert_eq!(prepared.request.exact_manuscript_prefix, job.input.prompt);
    assert!(prepared.request.context_preamble.is_empty());
    assert!(prepared.request.media.is_empty());
    assert_eq!(prepared.request.cases.len(), 1);
    let source = store
        .read_document(format!("Requests/{}/{}.md", job.peer, job.id))
        .expect("retained request");
    assert_eq!(source.text, job.input.prompt);
    let provenance = store
        .revision_provenance(source.revision_id)
        .expect("derived provenance");
    assert_eq!(
        provenance.segments[0].contribution,
        loom_types::ContributionKind::Source
    );
    assert!(
        prepare(&mut store, &model, &job).is_err(),
        "even an accidental second adapter invocation cannot rerun a recorded job"
    );
    assert_eq!(store.list_documents().expect("documents").len(), 1);
}

#[cfg(not(unix))]
#[test]
fn unsupported_private_storage_fails_before_creating_received_writing() {
    let directory = tempfile::tempdir().expect("fixture");
    let root = directory.path().join("peer-compute-writing");
    assert!(matches!(
        private_store(&root),
        Err(ComputeFailure::InputUnsupported)
    ));
    assert!(!root.exists());
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH and executes a real local model through authenticated peer compute"]
#[allow(clippy::too_many_lines)]
fn real_native_model_job_crosses_quic_without_borrowing_the_active_manuscript() {
    let directory = tempfile::tempdir().expect("native peer fixture");
    let writing = directory.path().join("Writing");
    let (mut store, _) = ProjectStore::initialize(&writing, "My writing").expect("project");
    store
        .create_document_if_absent(
            "Draft.md",
            DocumentContent::Prose("My private manuscript stays mine.\n".into()),
            "source",
        )
        .expect("source");
    let original = store.read_document("Draft.md").expect("original");
    let state = PluginState::with_app_local_data_root(
        Some(directory.path().join("app-data")),
        false,
        BuildModelPolicy::default(),
    );
    {
        let mut session = state.session.lock().expect("session");
        session.store = Some(store);
        session.phase = SessionPhase::Open;
        session.active_session_id = Some(CommandId::new());
    }
    let app = tauri::test::mock_app();
    assert!(app.manage(state));
    tauri::async_runtime::block_on(crate::model_load(
        std::env::var("MOM_LLAMA_MODEL_PATH").expect("explicit native model path"),
        app.handle().clone(),
        app.state::<PluginState>(),
    ))
    .expect("load verified native model");
    let state = app.state::<PluginState>();
    let model = loaded_model_for_state(&state).expect("verified model");
    let executor = NativeExecutor::from_state(&state);
    tauri::async_runtime::block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !state.peer_compute.idle() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("host idle");
        let host_identity = Identity::generate().expect("host identity");
        let peer_identity = Identity::generate().expect("peer identity");
        let mut cabal = Cabal::create(
            &directory.path().join("cabal.db"),
            host_identity.clone(),
            "Garden",
            "Host",
        )
        .expect("cabal");
        let invitation = cabal
            .invite(host_identity.public_key().into())
            .expect("invitation");
        cabal
            .admit(&invitation.token, peer_identity.public_key(), "Peer")
            .expect("admission");
        let mut job = input_job(model_claim(&model).expect("native model claim"));
        job.peer = peer_identity.public_key();
        job.grant.peer = job.peer;
        job.grant.cabal = cabal.id();
        job.input.prompt =
            "Word: rain\nImage: silver threads against the window.\nWord: dawn\nImage:".into();
        let host_network = Network::start(&host_identity, NetworkMode::Local)
            .await
            .expect("host endpoint");
        host_network
            .add(Arc::new(Mutex::new(cabal)))
            .expect("host membership");
        let host = host_network
            .host_compute(&directory.path().join("ledger"), Arc::new(executor))
            .expect("native executor");
        host.grant(job.grant.clone()).expect("explicit grant");
        let peer = Network::start(&peer_identity, NetworkMode::Local)
            .await
            .expect("peer endpoint");
        let accepted = peer
            .compute_submit(
                host_network.address(),
                job.id,
                job.grant.id,
                job.input.clone(),
            )
            .await
            .expect("submit native job");
        assert!(
            matches!(accepted, ComputeReply::Receipt { .. }),
            "{accepted:?}"
        );
        let completed = tokio::time::timeout(Duration::from_mins(2), async {
            loop {
                let reply = peer
                    .compute_status(host_network.address(), job.id)
                    .await
                    .expect("job status");
                let ComputeReply::Receipt { receipt } = reply else {
                    panic!("{reply:?}");
                };
                if receipt.payload.status.is_terminal() {
                    break receipt;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("native job deadline");
        completed.verify().expect("host signature");
        let ComputeStatus::Completed { text } = &completed.payload.status else {
            panic!("native result: {:?}", completed.payload.status);
        };
        assert!(!text.is_empty(), "actual generated text");
        assert_eq!(completed.payload.model, job.grant.model);
        let private = private_store(&directory.path().join("app-data/peer-compute-writing"))
            .expect("private evidence");
        let output = private
            .read_document(format!("Results/{}/{}.md", job.peer, job.id))
            .expect("native result document");
        assert_eq!(&output.text, text);
        let provenance = private
            .revision_provenance(output.revision_id)
            .expect("local native provenance");
        assert_eq!(
            provenance.segments[0].contribution,
            loom_types::ContributionKind::Generated
        );
        let database = rusqlite::Connection::open_with_flags(
            private.root().join(".loom/loom.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("read immutable artifact metadata");
        let metadata: String = database
            .query_row(
                "SELECT metadata_json FROM artifacts WHERE artifact_id = ?",
                [provenance.segments[0].artifact_id.to_string()],
                |row| row.get(0),
            )
            .expect("generated artifact");
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata).expect("artifact metadata");
        let evidence_blob: BlobId = metadata["evidence_blob_id"]
            .as_str()
            .expect("evidence link")
            .parse()
            .expect("evidence identity");
        let evidence: ExactContinuationResult = serde_json::from_slice(
            &private
                .read_blob(evidence_blob)
                .expect("native evidence blob"),
        )
        .expect("native result");
        assert_eq!(evidence.exact_manuscript_prefix, job.input.prompt);
        assert_eq!(evidence.model.model_sha256, model.descriptor.model_sha256);
        let native = &evidence.candidates[0];
        assert!(!native.token_trace.generated_token_ids.is_empty());
        assert!(native.token_trace.generated_token_ids.len() <= 16);
        assert_eq!(native.output_text, *text);
        assert_eq!(
            native
                .token_trace
                .provenance
                .as_ref()
                .expect("native provenance")
                .evidence_kind,
            loom_types::InferenceEvidenceKind::LiveInference
        );
        let backend: serde_json::Value =
            serde_json::from_slice(&native.backend_receipt_bytes).expect("native backend receipt");
        assert_eq!(backend["input_contract"], "raw_completion");
        assert_eq!(
            private
                .list_documents()
                .expect("retained source and output")
                .len(),
            2
        );
        // Exact model evidence stays private to its executing host. The public
        // reply is an authenticated assertion, not a serialized native witness.
        let public = serde_json::to_value(&completed).expect("public assertion");
        assert_eq!(public["payload"]["kind"], "loom_remote_execution_v1");
        assert!(public["payload"].get("token_trace").is_none());
        let retry = peer
            .compute_submit(host_network.address(), job.id, job.grant.id, job.input)
            .await
            .expect("exact retry");
        let ComputeReply::Receipt { receipt } = retry else {
            panic!("{retry:?}");
        };
        assert_eq!(
            receipt.hash().expect("retry hash"),
            completed.hash().expect("completed hash")
        );
        {
            let session = state.session.lock().expect("active session");
            let original_after = session
                .store
                .as_ref()
                .expect("active store")
                .read_document("Draft.md")
                .expect("private manuscript");
            assert_eq!(original_after, original);
        }
        eprintln!(
            "Real peer model job: job={}, model_sha256={}, output_sha256={}, bytes={}",
            job.id,
            model.descriptor.model_sha256,
            output.blob_id,
            text.len()
        );
        host_network.shutdown().await.expect("host joined");
        peer.shutdown().await.expect("peer joined");
    });
    let unloaded = tauri::async_runtime::block_on(crate::model_unload(app.state::<PluginState>()))
        .expect("unload actual model");
    assert!(unloaded.resident_slot_released);
}
