//! Actual local QUIC and native terminal ownership with a labelled fixture
//! executor. These tests make no claim about model execution or Internet NAT.
use super::*;
use crate::cabals::requesting::tests::Pair;

fn target(pair: &Pair) -> PeerTarget {
    PeerTarget {
        host: pair.request.host.to_string(),
        grant: pair.request.grant.clone(),
        roster_hash: pair.roster_hash.clone(),
    }
}

fn source(pair: &Pair) -> LoadedDocument {
    pair.app
        .state::<PluginState>()
        .session
        .lock()
        .unwrap()
        .store
        .as_mut()
        .unwrap()
        .read_document("Draft.md")
        .unwrap()
}

async fn wait(pair: &Pair, id: CommandId) -> TerminalRun {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let run = terminal_list(pair.project.clone(), pair.session.clone(), pair.app.state())
            .await
            .unwrap()
            .into_iter()
            .find(|run| run.run_id == id.to_string())
            .unwrap();
        if run.status != "running" {
            return run;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "terminal worker did not settle"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn start(pair: &Pair, id: CommandId, expression: &str) -> TerminalRun {
    let source = source(pair);
    terminal_run_peer(
        PeerTerminalRequest {
            project_id: pair.project.clone(),
            session_id: pair.session.clone(),
            command_id: id.to_string(),
            document_id: source.document_id.to_string(),
            source_revision_id: source.revision_id.to_string(),
            expected_visible_blob_id: source.blob_id.to_string(),
            source_start_byte: 0,
            source_end_byte: 0,
            expression: expression.into(),
            presentation: None,
            context_references: None,
            turn_boundary: None,
            literal_input: None,
            remote_target: target(pair),
        },
        pair.app.handle().clone(),
        pair.app.state(),
    )
    .await
    .unwrap()
}

async fn recover(pair: &Pair, id: CommandId, mode: RecoveryMode) -> TerminalRun {
    terminal_recover(
        pair.project.clone(),
        pair.session.clone(),
        id.to_string(),
        mode,
        pair.app.handle().clone(),
        pair.app.state(),
    )
    .await
    .unwrap();
    wait(pair, id).await
}

// Capture an immutable two-call expression at the exact interruption boundary:
// the first remote result exists, but the pipeline owner has not finished it.
async fn interrupted_pipeline(pair: &Pair) -> CommandId {
    let id = CommandId::new();
    let source = source(pair);
    let root = pair.temporary.path().join("writing");
    let input_blob_id = pair
        .app
        .state::<PluginState>()
        .session
        .lock()
        .unwrap()
        .store
        .as_mut()
        .unwrap()
        .store_provenance_blob(source.text.as_bytes())
        .unwrap();
    let receipt = RunReceipt {
        remote: Some(target(pair)),
        literal_input: false,
        run: TerminalRun {
            run_id: id.to_string(),
            status: "running".into(),
            expression: "=@Polish(@Polish(@Draft))".into(),
            source_document_id: source.document_id.to_string(),
            presentation: None,
            turn_boundary: None,
            title: "Polish".into(),
            output_document_id: None,
            output_relative_path: None,
            preview: String::new(),
            error: None,
            created_at_ms: now_unix_ms(),
            remote: Some(PeerModel::from(&target(pair))),
        },
        request_fingerprint: BlobId::digest(b"interrupted fixture request"),
        source_document_id: source.document_id,
        source_revision_id: source.revision_id,
        input_blob_id,
        context_references: None,
        media: Vec::new(),
        model: None,
        bindings: BTreeMap::from([
            ("Polish".into(), "Polish these words.".into()),
            ("Draft".into(), source.text.clone()),
        ]),
        sources: Vec::new(),
        steps: Vec::new(),
    };
    write_receipt(&root, &receipt, false).unwrap();
    let app = pair.app.handle().clone();
    let identity = GenerationFamilyIdentity {
        request_id: format!("terminal-{id}"),
        project_id: pair.project.parse().unwrap(),
        session_id: pair.session.parse().unwrap(),
        document_id: source.document_id,
    };
    tokio::task::spawn_blocking(move || {
        let control = TerminalControl::default();
        let mut evaluator = Evaluator {
            state: app.state(),
            identity: &identity,
            model: None,
            control: &control,
            source: None,
            root: &root,
            input: source.text,
            receipt,
            media: Vec::new(),
            step: 0,
            recovery: RecoveryMode::Resume,
        };
        evaluator
            .evaluate_command(&parse_neural_command("=@Polish(@Draft)").unwrap())
            .unwrap();
        // Deliberately no finish: models a requester process lost after import.
    })
    .await
    .unwrap();
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 1);
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_terminal_uses_the_selected_host_without_a_local_model_and_preserves_provenance() {
    let pair = Pair::new().await;
    let id = CommandId::new();
    let initial = start(&pair, id, "A shared garden 🌱").await;
    assert_eq!(initial.remote.unwrap().model, pair.request.grant.model);
    let run = wait(&pair, id).await;
    assert_eq!(run.status, "completed", "{:?}", run.error);
    assert_eq!(run.preview, "Remote assertion: A shared garden 🌱");
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 1);
    {
        let state = pair.app.state::<PluginState>();
        assert_eq!(state.generations.active_branch_count().unwrap(), 0);
        assert!(loaded_model(&state).is_err());
        let mut session = state.session.lock().unwrap();
        let store = session.store.as_mut().unwrap();
        assert_eq!(
            store.read_document("Draft.md").unwrap().text,
            "My untouched writing."
        );
        let output = store
            .read_document(run.output_relative_path.unwrap())
            .unwrap();
        let provenance = store.revision_provenance(output.revision_id).unwrap();
        assert_eq!(
            provenance.segments[0].contribution,
            loom_types::ContributionKind::Source
        );
        let receipt = read_receipt(store.root(), &id.to_string(), true)
            .unwrap()
            .unwrap();
        assert!(receipt.model.is_none());
        let evidence: serde_json::Value =
            serde_json::from_slice(&store.read_blob(receipt.steps[0]).unwrap()).unwrap();
        assert_eq!(
            evidence["receipt"]["payload"]["kind"],
            "loom_remote_execution_v1"
        );
        assert!(
            evidence
                .to_string()
                .contains(&pair.request.host.to_string())
        );
        assert!(!evidence.to_string().contains("live_inference"));
    }
    // Reusing the exact terminal command returns the saved result, no model call.
    assert_eq!(
        start(&pair, id, "A shared garden 🌱").await.status,
        "completed"
    );
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 1);
    pair.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_never_creates_a_later_step_and_resume_reuses_results_after_requester_reopen() {
    let pair = Pair::new().await;
    let id = interrupted_pipeline(&pair).await;
    let root = pair.temporary.path().join("writing");
    // Simulate the ledger owner reopening and a newer visible manuscript.
    pair.reopen_requester().await;
    std::fs::write(root.join("Draft.md"), "New human writing must stay.").unwrap();
    let first_output = root.join(format!("Runs/{id}/1/Polish.md"));
    std::fs::write(&first_output, "My edited result stays too.").unwrap();
    let checked = recover(&pair, id, RecoveryMode::Check).await;
    assert_eq!(checked.status, "unconfirmed");
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 1);
    assert!(
        find_job(
            &pair.project,
            &pair.session,
            job_id(&id.to_string(), 2),
            &pair.app.state::<PluginState>()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        read_receipt(&root, &id.to_string(), true)
            .unwrap()
            .is_none()
    );
    let resumed = recover(&pair, id, RecoveryMode::Resume).await;
    assert_eq!(resumed.status, "completed", "{:?}", resumed.error);
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 2);
    let final_job = find_job(
        &pair.project,
        &pair.session,
        job_id(&id.to_string(), 2),
        &pair.app.state::<PluginState>(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        final_job
            .request
            .input
            .prompt
            .contains("My untouched writing.")
    );
    assert!(!final_job.request.input.prompt.contains("New human writing"));
    assert_eq!(
        std::fs::read_to_string(root.join("Draft.md")).unwrap(),
        "New human writing must stay."
    );
    recover(&pair, id, RecoveryMode::Resume).await;
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(first_output).unwrap(),
        "My edited result stays too."
    );
    pair.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_an_interrupted_pipeline_persists_across_reopen_and_blocks_later_steps() {
    let pair = Pair::new().await;
    let id = interrupted_pipeline(&pair).await;
    terminal_cancel(
        pair.project.clone(),
        pair.session.clone(),
        id.to_string(),
        pair.app.state(),
    )
    .await
    .unwrap();
    pair.reopen_requester().await;
    let recovered = recover(&pair, id, RecoveryMode::Resume).await;
    assert_eq!(recovered.status, "cancelled", "{:?}", recovered.error);
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 1);
    assert!(
        find_job(
            &pair.project,
            &pair.session,
            job_id(&id.to_string(), 2),
            &pair.app.state::<PluginState>()
        )
        .await
        .unwrap()
        .is_none()
    );
    let root = pair.temporary.path().join("writing");
    assert!(crate::terminal_receipts::cancel_requested(&root, &id.to_string()).unwrap());
    assert_eq!(
        std::fs::read_dir(root.join("Runs").join(id.to_string()))
            .unwrap()
            .count(),
        1
    );
    pair.close().await;
}
