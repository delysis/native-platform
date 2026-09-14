//! Exercise the actual terminal command, worker, receipt, and document store.
//! A pure expression needs no loaded model and creates no inference fixture.

use super::*;

struct TerminalFixture {
    // Drop the app, including its store lease, before deleting the directory.
    app: tauri::App<tauri::test::MockRuntime>,
    directory: tempfile::TempDir,
    project_id: String,
    session_id: String,
    source: LoadedDocument,
}

impl TerminalFixture {
    fn new() -> Self {
        Self::with_model_discovery(true)
    }

    fn with_model_discovery(isolated: bool) -> Self {
        let directory = tempfile::tempdir().expect("temporary terminal project");
        let root = directory.path().join("Writing");
        let (mut store, _) = ProjectStore::initialize(&root, "Writing").expect("project store");
        store
            .create_document_if_absent(
                "Draft.md",
                DocumentContent::Prose("The first idea is α. @Unresolved stays literal.\n".into()),
                "source writing",
            )
            .expect("source document");
        let source = store.read_document("Draft.md").expect("source snapshot");
        let project_id = store.manifest().project_id.to_string();
        let session_id = CommandId::new();
        let state = PluginState::with_app_local_data_root(
            Some(directory.path().join("app-data")),
            isolated,
            BuildModelPolicy::default(),
        );
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
        }
        let app = tauri::test::mock_app();
        assert!(app.manage(state));
        Self {
            app,
            directory,
            project_id,
            session_id: session_id.to_string(),
            source,
        }
    }

    fn root(&self) -> PathBuf {
        self.directory.path().join("Writing")
    }

    fn run(&self, id: CommandId, expression: &str) -> Result<TerminalRun, IpcFailure> {
        self.run_with_presentation(id, expression, None)
    }

    fn run_with_presentation(
        &self,
        id: CommandId,
        expression: &str,
        presentation: Option<TerminalPresentation>,
    ) -> Result<TerminalRun, IpcFailure> {
        self.run_with_boundary(id, expression, presentation, None)
    }

    fn run_with_boundary(
        &self,
        id: CommandId,
        expression: &str,
        presentation: Option<TerminalPresentation>,
        boundary: Option<TerminalTurnBoundary>,
    ) -> Result<TerminalRun, IpcFailure> {
        tauri::async_runtime::block_on(terminal_run(
            self.project_id.clone(),
            self.session_id.clone(),
            id.to_string(),
            self.source.document_id.to_string(),
            self.source.revision_id.to_string(),
            self.source.blob_id.to_string(),
            0,
            0,
            expression.into(),
            presentation,
            None,
            boundary,
            self.app.handle().clone(),
            self.app.state::<PluginState>(),
        ))
    }

    fn list(&self) -> Vec<TerminalRun> {
        tauri::async_runtime::block_on(terminal_list(
            self.project_id.clone(),
            self.session_id.clone(),
            self.app.state::<PluginState>(),
        ))
        .expect("list retained terminal runs")
    }

    fn wait(&self, id: CommandId) -> TerminalRun {
        self.wait_for(id, Duration::from_secs(5))
    }

    fn wait_for(&self, id: CommandId, timeout: Duration) -> TerminalRun {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(run) = self
                .list()
                .into_iter()
                .find(|run| run.run_id == id.to_string())
                && run.status != "running"
            {
                return run;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "terminal worker did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn with_store<T>(&self, operation: impl FnOnce(&mut ProjectStore) -> T) -> T {
        let state = self.app.state::<PluginState>();
        let mut session = state.session.lock().expect("session lock");
        operation(session.store.as_mut().expect("open project store"))
    }
}

impl Drop for TerminalFixture {
    fn drop(&mut self) {
        let _joined = self
            .app
            .state::<PluginState>()
            .join_desktop_workers_for_exit();
    }
}

#[test]
fn reference_run_retains_source_value_and_replays_without_duplicate_writing() {
    let fixture = TerminalFixture::new();
    let id = CommandId::new();
    let started = fixture
        .run(id, "=@Draft")
        .expect("run without a loaded model");
    assert_eq!(started.status, "running");
    let completed = fixture.wait(id);
    assert_eq!(completed.status, "completed", "{:?}", completed.error);
    let output_path = completed
        .output_relative_path
        .as_ref()
        .expect("retained output path");
    fixture.with_store(|store| {
        let source = store.read_document("Draft.md").expect("unchanged source");
        assert_eq!(source.text, fixture.source.text);
        assert_eq!(source.revision_id, fixture.source.revision_id);
        assert_eq!(source.blob_id, fixture.source.blob_id);
        let output = store
            .read_document(output_path)
            .expect("ordinary output document");
        assert_eq!(output.text, source.text);
        assert_ne!(output.document_id, source.document_id);
        assert_eq!(
            Some(output.document_id.to_string()),
            completed.output_document_id
        );
        let provenance = store
            .revision_provenance(output.revision_id)
            .expect("output provenance");
        assert_eq!(provenance.segments.len(), 1);
        assert_eq!(
            provenance.segments[0].contribution,
            loom_types::ContributionKind::Source
        );
        assert_eq!(store.list_documents().expect("document registry").len(), 2);
    });
    let receipt = read_receipt(&fixture.root(), &id.to_string(), true)
        .expect("read finished receipt")
        .expect("finished receipt exists");
    assert!(receipt.model.is_none());
    assert!(
        receipt.steps.is_empty(),
        "a reference must not execute inference"
    );
    assert_eq!(receipt.sources.len(), 1);
    assert_eq!(receipt.sources[0].revision_id, fixture.source.revision_id);
    let started_bytes = crate::terminal_receipts::read(&fixture.root(), &id.to_string(), false)
        .unwrap()
        .unwrap();
    let finished_bytes = crate::terminal_receipts::read(&fixture.root(), &id.to_string(), true)
        .unwrap()
        .unwrap();

    let replay = fixture.run(id, "=@Draft").expect("idempotent replay");
    assert_eq!(
        serde_json::to_value(&replay).unwrap(),
        serde_json::to_value(&completed).unwrap()
    );
    let conflict = fixture
        .run(id, "=\"different value\"")
        .expect_err("conflicting replay must fail");
    assert!(conflict.message.contains("different experiment"));
    assert_eq!(
        crate::terminal_receipts::read(&fixture.root(), &id.to_string(), false)
            .unwrap()
            .unwrap(),
        started_bytes
    );
    assert_eq!(
        crate::terminal_receipts::read(&fixture.root(), &id.to_string(), true)
            .unwrap()
            .unwrap(),
        finished_bytes
    );
    fixture.with_store(|store| assert_eq!(store.list_documents().unwrap().len(), 2));
    assert_eq!(fixture.list().len(), 1);
}

#[test]
fn literal_run_retains_the_value_as_an_ordinary_document_without_a_model() {
    let fixture = TerminalFixture::new();
    for (expression, expected) in [
        (
            "=\"An idea\\nwith two lines: β\"",
            "An idea\nwith two lines: β",
        ),
        ("=\"\"", ""),
    ] {
        let id = CommandId::new();
        fixture.run(id, expression).expect("literal run");
        let completed = fixture.wait(id);
        assert_eq!(completed.status, "completed", "{:?}", completed.error);
        let output_path = completed.output_relative_path.expect("retained literal");
        assert_eq!(
            std::fs::read_to_string(fixture.root().join(output_path)).unwrap(),
            expected
        );
    }
    fixture.with_store(|store| {
        let source = store.read_document("Draft.md").unwrap();
        assert_eq!(source.revision_id, fixture.source.revision_id);
        assert_eq!(source.text, fixture.source.text);
    });
}

#[test]
fn changed_source_is_rejected_before_reserving_a_run_or_creating_output() {
    let fixture = TerminalFixture::new();
    fixture.with_store(|store| {
        store
            .save_document(
                "Draft.md",
                DocumentContent::Prose("A newer idea.\n".into()),
                "edit before run",
            )
            .unwrap();
    });
    let id = CommandId::new();
    let error = fixture
        .run(id, "=@Draft")
        .expect_err("stale source must fail");
    assert!(error.message.contains("source changed"));
    assert!(fixture.list().is_empty());
    assert!(!fixture.root().join("Runs").exists());
    assert!(
        read_receipt(&fixture.root(), &id.to_string(), false)
            .unwrap()
            .is_none()
    );
    fixture.with_store(|store| {
        assert_eq!(store.list_documents().unwrap().len(), 1);
        assert_eq!(
            store.read_document("Draft.md").unwrap().text,
            "A newer idea.\n"
        );
    });
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH and loads the real local model for retained terminal inference"]
fn real_native_terminal_retains_raw_inference_without_changing_source() {
    let fixture = TerminalFixture::with_model_discovery(false);
    let model_path = std::env::var("MOM_LLAMA_MODEL_PATH").expect("explicit real model path");
    tauri::async_runtime::block_on(crate::model_load(
        model_path,
        fixture.app.handle().clone(),
        fixture.app.state::<PluginState>(),
    ))
    .expect("load and verify the actual native model");
    let prompt = "Word: rain\nImage: silver threads against the window.\nWord: dawn\nImage:";
    let id = CommandId::new();
    fixture.run(id, prompt).expect("start real raw inference");
    let completed = fixture.wait_for(id, Duration::from_mins(2));
    assert_eq!(completed.status, "completed", "{:?}", completed.error);
    let receipt = read_receipt(&fixture.root(), &id.to_string(), true)
        .expect("read immutable finished run")
        .expect("finished run exists");
    assert_eq!(receipt.steps.len(), 1);
    let model = receipt.model.as_ref().expect("verified model identity");
    let output_path = completed
        .output_relative_path
        .as_deref()
        .expect("retained output file");
    fixture.with_store(|store| {
        let evidence: loom_backend_llama::ExactContinuationResult = serde_json::from_slice(
            &store.read_blob(receipt.steps[0]).expect("durable native step evidence"),
        ).expect("native result receipt");
        assert_eq!(evidence.exact_manuscript_prefix, prompt);
        assert_eq!(evidence.model.model_sha256, model.model_sha256);
        assert_eq!(evidence.candidates.len(), 1);
        let candidate = &evidence.candidates[0];
        assert_eq!(candidate.terminal.status, GenerationTerminalStatus::Completed);
        assert!(!candidate.token_trace.generated_token_ids.is_empty(), "real model must generate tokens");
        assert!(!candidate.output_text.is_empty(), "real output must reach retained writing");
        assert_eq!(candidate.token_trace.provenance.as_ref().expect("native provenance").evidence_kind,
            loom_types::InferenceEvidenceKind::LiveInference);
        let backend: serde_json::Value = serde_json::from_slice(&candidate.backend_receipt_bytes).expect("native backend receipt");
        assert_eq!(backend["input_contract"], "raw_completion");
        let output = store.read_document(output_path).expect("registered ordinary result document");
        assert_eq!(output.text, candidate.output_text);
        assert_eq!(Some(output.document_id.to_string()), completed.output_document_id);
        let provenance = store.revision_provenance(output.revision_id).expect("generated authorship");
        assert_eq!(provenance.segments.len(), 1);
        assert_eq!(provenance.segments[0].contribution, loom_types::ContributionKind::Generated);
        let source = store.read_document("Draft.md").expect("source still opens");
        assert_eq!(source.revision_id, fixture.source.revision_id);
        assert_eq!(source.blob_id, fixture.source.blob_id);
        assert_eq!(source.text, fixture.source.text);
        eprintln!("Retained live raw inference: run={}, model_sha256={}, tokens={}, output={}, output_sha256={}",
            id, model.model_sha256, candidate.token_trace.generated_token_ids.len(), output_path, output.blob_id);
    });
    // MockRuntime does not drive the desktop exit lifecycle. Exercise the
    // actual unload command after joining the worker, so native resources are
    // released before Metal's process-global device teardown.
    let state = fixture.app.state::<PluginState>();
    let _joined = state.join_desktop_workers_for_exit();
    let unloaded = tauri::async_runtime::block_on(crate::model_unload(state))
        .expect("release the real native model");
    assert!(unloaded.resident_slot_released);
}

#[test]
fn pane_presentation_is_retained_and_bound_to_replay_identity() {
    let fixture = TerminalFixture::new();
    let id = CommandId::new();
    let presentation = TerminalPresentation {
        pane_id: "chat".into(),
        input: "A human-facing question".into(),
    };
    fixture
        .run_with_presentation(id, "=\"Retained reply\"", Some(presentation.clone()))
        .unwrap();
    let completed = fixture.wait(id);
    assert_eq!(
        completed.source_document_id,
        fixture.source.document_id.to_string()
    );
    assert_eq!(
        completed.presentation.as_ref().unwrap().input,
        presentation.input
    );
    fixture
        .run_with_presentation(id, "=\"Retained reply\"", Some(presentation.clone()))
        .unwrap();
    let changed = TerminalPresentation {
        pane_id: "other".into(),
        ..presentation
    };
    assert!(
        fixture
            .run_with_presentation(id, "=\"Retained reply\"", Some(changed))
            .is_err()
    );
    let listed = fixture
        .list()
        .into_iter()
        .find(|run| run.run_id == id.to_string())
        .unwrap();
    assert_eq!(listed.presentation.unwrap().pane_id, "chat");
}

#[test]
fn explicit_prompt_context_does_not_interpret_literal_history() {
    let literal =
        "User: What is @Missing?\nAssistant: A literal @mention.\nUser: Continue.\nAssistant:";
    assert!(
        prompt_reference_names(literal, Some(&[]), None)
            .unwrap()
            .is_empty()
    );
    let references = vec!["Draft".to_owned(), "Draft".to_owned()];
    assert_eq!(
        prompt_reference_names(literal, Some(&references), Some("Use @Voice")).unwrap(),
        BTreeSet::from(["Draft".into(), "Voice".into()])
    );
    assert_eq!(
        prompt_reference_names(literal, None, None).unwrap(),
        BTreeSet::from(["Missing".into(), "mention".into()])
    );
    assert!(validate_explicit_references(&vec!["Draft".into(); 65]).is_err());
    assert!(validate_explicit_references(&["x".repeat(1025)]).is_err());
}

#[test]
fn chat_turn_boundary_binds_sampling_and_command_replay() {
    let command = CommandId::new();
    let ordinary = terminal_sampling(command, 1, None);
    let chat = terminal_sampling(command, 1, Some(TerminalTurnBoundary::Chat));
    assert!(ordinary.stop.is_empty());
    assert_eq!(chat.stop, ["\nUser:", "\nAssistant:"]);
    assert_eq!(chat.seed, ordinary.seed);
    assert!(serde_json::from_str::<TerminalTurnBoundary>("\"unknown\"").is_err());

    let fixture = TerminalFixture::new();
    fixture.run(command, "=@Draft").unwrap();
    let completed = fixture.wait(command);
    let conflict = fixture
        .run_with_boundary(command, "=@Draft", None, Some(TerminalTurnBoundary::Chat))
        .expect_err("changing the output grammar is a different command");
    assert!(conflict.message.contains("different experiment"));
    assert_eq!(fixture.list().len(), 1);
    assert_eq!(
        fixture.run(command, "=@Draft").unwrap().output_document_id,
        completed.output_document_id
    );
    let invalid = fixture
        .run_with_boundary(
            CommandId::new(),
            "=@Draft",
            None,
            Some(TerminalTurnBoundary::Chat),
        )
        .expect_err("chat boundaries cannot silently change function expressions");
    assert!(invalid.message.contains("plain prompt"));
}
