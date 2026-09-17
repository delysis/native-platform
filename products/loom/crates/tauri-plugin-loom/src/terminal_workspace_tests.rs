use super::*;

fn request(fixture: &TerminalFixture, expression: &str) -> WorkspacePaneRequest {
    let configuration = fixture.with_store(|store| {
        store.create_document_if_absent(".loom.md", DocumentContent::Prose("# Workspace configuration stays out of prompts.\n\n```loom-workspace\n[panes.chat]\ncontext = []\n```\n".into()), "workspace").unwrap();
        store.read_document(".loom.md").unwrap()
    });
    let (owner, session) = fixture.owner_identity();
    WorkspacePaneRequest {
        workspace_id: owner.to_string(),
        workspace_session_id: session.to_string(),
        command_id: CommandId::new().to_string(),
        pane_id: "terminal".into(),
        configuration_revision_id: configuration.revision_id.to_string(),
        expression: expression.into(),
        input: "Only the submitted input.".into(),
        captured_document: None,
    }
}

fn run(
    fixture: &TerminalFixture,
    request: WorkspacePaneRequest,
) -> Result<TerminalRun, IpcFailure> {
    match submission(fixture, request)? {
        WorkspacePaneSubmission::Accepted { run } => Ok(*run),
        WorkspacePaneSubmission::Rejected { error } => Err(error),
    }
}

fn submission(
    fixture: &TerminalFixture,
    request: WorkspacePaneRequest,
) -> Result<WorkspacePaneSubmission, IpcFailure> {
    tauri::async_runtime::block_on(workspace_pane_run(
        request,
        fixture.app.handle().clone(),
        fixture.app.state(),
    ))
}

fn wait(fixture: &TerminalFixture, run_id: &str) -> RunReceipt {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(receipt) = read_receipt(&fixture.root(), run_id, true).unwrap() {
            return receipt;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "workspace run did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn child(fixture: &mut TerminalFixture, text: &str) {
    let (mut store, _) =
        ProjectStore::initialize(fixture.directory.path().join("Child"), "Child").unwrap();
    store
        .create_document_if_absent(
            "Draft.md",
            DocumentContent::Prose(text.into()),
            "child source",
        )
        .unwrap();
    fixture.source = store.read_document("Draft.md").unwrap();
    fixture.project_id = store.manifest().project_id.to_string();
    let token = CommandId::new();
    fixture.session_id = token.to_string();
    let state = fixture.app.state::<PluginState>();
    let mut session = state.session.lock().unwrap();
    crate::workspace_owner::park_active(&mut session);
    session.store = Some(store);
    session.active_session_id = Some(token);
    session.phase = SessionPhase::Open;
}

#[test]
fn pane_uses_owner_documents_and_anchor_without_reading_unused_capture() {
    let mut fixture = TerminalFixture::new();
    let owner_text = fixture.source.text.clone();
    let mut request = request(&fixture, "=@Draft");
    request.captured_document = Some(scope::CapturedDocument {
        project_id: "not an authorized project".into(),
        session_id: "unused".into(),
        document_id: "unused".into(),
        revision_id: "unused".into(),
        visible_blob_id: "unused".into(),
    });
    child(
        &mut fixture,
        "The child has the same filename but different contents.",
    );
    let started = run(&fixture, request.clone()).unwrap();
    let receipt = wait(&fixture, &started.run_id);
    assert_eq!(receipt.run.status, "completed", "{:?}", receipt.run.error);
    assert_eq!(receipt.scope, RunScope::Workspace);
    assert!(receipt.workspace_configuration.is_some());
    assert!(!receipt.bindings.contains_key("document"));
    assert!(receipt.media.is_empty());
    let state = fixture.app.state::<PluginState>();
    {
        let session = state.session.lock().unwrap();
        let owner = crate::workspace_owner::store(&session).unwrap();
        assert_eq!(
            receipt.source_document_id,
            owner.read_document(".loom.md").unwrap().document_id
        );
        assert_eq!(
            owner.read_blob(receipt.input_blob_id).unwrap(),
            request.input.as_bytes()
        );
        let output = owner
            .read_document(receipt.run.output_relative_path.as_ref().unwrap())
            .unwrap();
        assert_eq!(output.text, owner_text);
        assert!(
            session
                .store
                .as_ref()
                .unwrap()
                .registered_document(output.document_id)
                .unwrap()
                .is_none()
        );
    }
    let again = run(&fixture, request).unwrap();
    assert_eq!(again.output_document_id, receipt.run.output_document_id);
    assert!(
        tauri::async_runtime::block_on(workspace_pane_list(
            fixture.owner_identity().0.to_string(),
            fixture.owner_identity().1.to_string(),
            fixture.app.state(),
        ))
        .unwrap()
        .iter()
        .any(|run| run.run_id == started.run_id)
    );
}

#[test]
fn configured_or_explicit_current_document_requires_exact_capture() {
    let fixture = TerminalFixture::new();
    let mut request = request(&fixture, "=@document");
    assert!(
        run(&fixture, request.clone())
            .unwrap_err()
            .message
            .contains("captured active document")
    );
    request.captured_document = Some(scope::CapturedDocument {
        project_id: fixture.project_id.clone(),
        session_id: fixture.session_id.clone(),
        document_id: fixture.source.document_id.to_string(),
        revision_id: fixture.source.revision_id.to_string(),
        visible_blob_id: BlobId::digest(b"changed").to_string(),
    });
    assert!(
        run(&fixture, request)
            .unwrap_err()
            .message
            .contains("source changed")
    );
    std::fs::write(
        fixture.root().join(".loom.md"),
        "```loom-workspace\n[panes.chat]\ncontext=['@document']\n```\n",
    )
    .unwrap();
    let configuration = fixture.with_store(|store| {
        crate::workspace_template::load_template(store)
            .unwrap()
            .unwrap()
    });
    let (owner, token) = fixture.owner_identity();
    let configured = WorkspacePaneRequest {
        workspace_id: owner.to_string(),
        workspace_session_id: token.to_string(),
        command_id: CommandId::new().to_string(),
        pane_id: "chat".into(),
        configuration_revision_id: configuration.revision_id.to_string(),
        expression: "Hello".into(),
        input: "Hello".into(),
        captured_document: None,
    };
    assert!(
        run(&fixture, configured)
            .unwrap_err()
            .message
            .contains("captured active document")
    );
}

#[test]
fn submission_distinguishes_rejection_from_unreadable_admission() {
    let fixture = TerminalFixture::new();
    let invalid = request(&fixture, "=@document");
    let rejected = submission(&fixture, invalid.clone()).unwrap();
    assert!(matches!(rejected, WorkspacePaneSubmission::Rejected { .. }));
    assert_eq!(
        serde_json::to_value(&rejected).unwrap()["status"],
        "rejected"
    );
    assert!(
        read_receipt(&fixture.root(), &invalid.command_id, false)
            .unwrap()
            .is_none()
    );

    let mut valid = invalid;
    valid.command_id = CommandId::new().to_string();
    valid.expression = "=@Draft".into();
    let accepted = submission(&fixture, valid.clone()).unwrap();
    let WorkspacePaneSubmission::Accepted { run } = accepted else {
        panic!("valid submission was rejected");
    };
    wait(&fixture, &run.run_id);
    let replay = submission(&fixture, valid.clone()).unwrap();
    assert_eq!(serde_json::to_value(&replay).unwrap()["status"], "accepted");

    // A failed read cannot establish that this command was never admitted.
    let path = receipt_directory(&fixture.root())
        .unwrap()
        .join(format!("{}.started.json", valid.command_id));
    std::fs::write(&path, b"{broken").unwrap();
    assert!(submission(&fixture, valid).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"{broken");
}

#[test]
fn frozen_active_document_remains_usable_after_its_session_closes() {
    let mut fixture = TerminalFixture::new();
    let mut request = request(&fixture, "=@document");
    child(
        &mut fixture,
        "Exact child text survives the root switch.\r\n",
    );
    request.captured_document = Some(scope::CapturedDocument {
        project_id: fixture.project_id.clone(),
        session_id: fixture.session_id.clone(),
        document_id: fixture.source.document_id.to_string(),
        revision_id: fixture.source.revision_id.to_string(),
        visible_blob_id: fixture.source.blob_id.to_string(),
    });
    let started = run(&fixture, request).unwrap();
    let mut receipt = wait(&fixture, &started.run_id);
    assert_eq!(receipt.run.status, "completed", "{:?}", receipt.run.error);
    let captured = receipt.bindings["document"].clone();
    let id = CommandId::new();
    receipt.run.run_id = id.to_string();
    receipt.run.status = "running".into();
    receipt.run.output_document_id = None;
    receipt.run.output_relative_path = None;
    let state = fixture.app.state::<PluginState>();
    let (owner_project_id, owner_session_id) = fixture.owner_identity();
    let source = {
        let session = state.session.lock().unwrap();
        crate::workspace_owner::store(&session)
            .unwrap()
            .read_document(".loom.md")
            .unwrap()
    };
    let identity = GenerationFamilyIdentity {
        request_id: RunScope::Workspace.request_id(&id.to_string()),
        project_id: owner_project_id,
        session_id: owner_session_id,
        document_id: source.document_id,
    };
    let control = Arc::new(TerminalControl::default());
    state
        .generations
        .reserve(
            identity.clone(),
            vec![(GenerationRunId::new(), BranchId::new())],
        )
        .unwrap();
    state
        .generations
        .attach_cancellation(&identity.request_id, control.clone())
        .unwrap();
    close_project_with_wait(
        &state,
        fixture.project_id.clone(),
        fixture.session_id.clone(),
        CommandId::new(),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(
        !control.cancelled.load(Ordering::Acquire),
        "Closing the child must not cancel owner pane runs."
    );
    let mut evaluator = Evaluator {
        state: &state,
        identity: &identity,
        owner_project_id,
        owner_session_id,
        model: None,
        control: &control,
        source: &source,
        input: String::new(),
        receipt,
        media: Vec::new(),
        step: 0,
        folder_scan_budget: material_context::FolderScanBudget::default(),
    };
    let consulted = evaluator.consult(&captured, "", 64 * 1024).unwrap();
    assert_eq!(
        material_context::exact(&consulted).unwrap(),
        fixture.source.text
    );
    evaluator.finish(Ok(consulted)).unwrap();
    state
        .generations
        .complete_family(&identity.request_id)
        .unwrap();
    assert!(
        read_receipt(&fixture.root(), &id.to_string(), true)
            .unwrap()
            .is_some()
    );
}

#[test]
fn owner_cancellation_is_scope_specific_and_drain_waits_for_publication() {
    let fixture = TerminalFixture::new();
    let request = request(&fixture, "=@Draft");
    let state = fixture.app.state::<PluginState>();
    let (owner, owner_token) = fixture.owner_identity();
    let run_id = CommandId::new().to_string();
    let identity = GenerationFamilyIdentity {
        request_id: RunScope::Workspace.request_id(&run_id),
        project_id: owner,
        session_id: owner_token,
        document_id: fixture.source.document_id,
    };
    let control = Arc::new(TerminalControl::default());
    state
        .generations
        .reserve(
            identity.clone(),
            vec![(GenerationRunId::new(), BranchId::new())],
        )
        .unwrap();
    state
        .generations
        .attach_cancellation(&identity.request_id, control.clone())
        .unwrap();
    assert!(
        tauri::async_runtime::block_on(terminal_cancel(
            owner.to_string(),
            owner_token.to_string(),
            run_id.clone(),
            fixture.app.state(),
        ))
        .is_err()
    );
    assert!(!control.cancelled.load(Ordering::Acquire));
    {
        let _held = state.session.lock().unwrap();
        tauri::async_runtime::block_on(workspace_pane_cancel(
            request.workspace_id,
            request.workspace_session_id,
            run_id,
            fixture.app.state(),
        ))
        .unwrap();
    }
    assert!(
        control.cancelled.load(Ordering::Acquire),
        "A live route permits Stop while source retrieval holds the store lock."
    );
    assert_eq!(
        drain_workspace_runs(&state, Duration::ZERO)
            .unwrap_err()
            .code,
        "workspace_run_cancellation_in_progress"
    );
    state
        .generations
        .complete_family(&identity.request_id)
        .unwrap();
    drain_workspace_runs(&state, Duration::ZERO).unwrap();
}

#[test]
fn prior_owner_pane_history_is_readable_without_rebinding_its_run_authority() {
    let fixture = TerminalFixture::new();
    let mut request = request(&fixture, "=@Draft");
    let id = CommandId::new();
    fixture
        .run_with_presentation(
            id,
            "=@Draft",
            Some(TerminalPresentation {
                pane_id: "chat".into(),
                input: "Previous pane input".into(),
            }),
        )
        .unwrap();
    let previous = fixture.wait(id);
    let before = crate::terminal_receipts::read(&fixture.root(), &id.to_string(), true)
        .unwrap()
        .unwrap();
    let history = tauri::async_runtime::block_on(workspace_pane_list(
        request.workspace_id.clone(),
        request.workspace_session_id.clone(),
        fixture.app.state(),
    ))
    .unwrap();
    assert!(history.iter().any(|run| run.run_id == id.to_string()));
    fixture.with_store(|store| {
        assert_eq!(
            output_document(store, &id.to_string())
                .unwrap()
                .map(|id| id.to_string()),
            previous.output_document_id
        );
    });
    request.command_id = id.to_string();
    assert!(
        run(&fixture, request)
            .unwrap_err()
            .message
            .contains("different experiment")
    );
    assert_eq!(
        crate::terminal_receipts::read(&fixture.root(), &id.to_string(), true)
            .unwrap()
            .unwrap(),
        before
    );
}

#[test]
fn function_format_uses_the_admitted_configuration_snapshot() {
    let fixture = TerminalFixture::new();
    let _ = request(&fixture, "=@Draft");
    let raw = "```loom-workspace\n[functions]\nformat='raw'\n```\n";
    std::fs::write(fixture.root().join(".loom.md"), raw).unwrap();
    let admitted = fixture.with_store(|store| {
        crate::workspace_template::load_template(store)
            .unwrap()
            .unwrap()
    });
    std::fs::write(
        fixture.root().join(".loom.md"),
        "```loom-workspace\n[functions]\nformat='model'\n```\n",
    )
    .unwrap();
    fixture.with_store(|store| {
        let current = crate::workspace_template::load_template(store)
            .unwrap()
            .unwrap();
        assert_ne!(current.revision_id, admitted.revision_id);
        let recipe =
            crate::workspace_template::function_recipe_from_snapshot(store, &admitted).unwrap();
        assert_eq!(
            recipe.format,
            crate::workspace_template::FunctionFormat::Raw
        );
        let configuration = recipe.configuration.unwrap();
        assert_eq!(configuration.revision_id, admitted.revision_id);
        assert_eq!(configuration.text, raw);
    });
}

#[test]
fn workspace_count_retains_owner_library_scope_without_model_or_snippet_totals() {
    let mut fixture = TerminalFixture::new();
    let path = fixture.directory.path().join("count-library.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("materials/alexandria-fixture.sql"))
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    let mut request = request(&fixture, "=count(@Research)");
    fixture
        .with_store(|store| crate::materials::add_library(store, &path, Some("Research")).unwrap());
    request.configuration_revision_id = fixture.with_store(|store| {
        store
            .read_document(".loom.md")
            .unwrap()
            .revision_id
            .to_string()
    });
    child(&mut fixture, "The active child is not the library owner.");
    let started = run(&fixture, request.clone()).unwrap();
    let receipt = wait(&fixture, &started.run_id);
    assert_eq!(receipt.run.status, "completed", "{:?}", receipt.run.error);
    assert!(receipt.model.is_none());
    assert!(receipt.steps.is_empty());
    assert!(receipt.searches.is_empty());
    assert_eq!(receipt.counts.len(), 1);
    let retained = &receipt.counts[0];
    let Value::Scoped { origin, .. } = retained else {
        panic!("count lost origin");
    };
    let Value::Count { count } = retained.unscoped() else {
        panic!("count lost type");
    };
    assert_eq!(count.result.value, 1);
    let state = fixture.app.state::<PluginState>();
    {
        let session = state.session.lock().unwrap();
        let owner = crate::workspace_owner::store(&session).unwrap();
        let attribution = serde_json::to_value(origin).unwrap();
        assert_eq!(
            attribution["project_id"],
            owner.manifest().project_id.to_string()
        );
        assert_eq!(attribution["root"], owner.root().to_str().unwrap());
        assert_ne!(
            attribution["project_id"],
            session
                .store
                .as_ref()
                .unwrap()
                .manifest()
                .project_id
                .to_string()
        );
        assert_eq!(
            owner
                .read_document(receipt.run.output_relative_path.as_ref().unwrap())
                .unwrap()
                .text,
            count.text()
        );
        assert!(
            material_context::local_artifact_ids(session.store.as_ref().unwrap(), [retained])
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // Replaying the admitted command reads retained truth, not today's library.
    std::fs::remove_file(&path).unwrap();
    let replay = run(&fixture, request).unwrap();
    assert_eq!(replay.run_id, started.run_id);
    assert_eq!(
        read_receipt(&fixture.root(), &started.run_id, true)
            .unwrap()
            .unwrap()
            .counts
            .len(),
        1
    );
}
