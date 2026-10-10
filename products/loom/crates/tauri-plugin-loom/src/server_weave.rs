//! Server execution shares Loom's durable admission and promotion boundaries.
//! Server responses carry their own evidence class, never synthetic native proof.
use super::*;
use fte_types::{
    CompletionPrompt, ContentBlock, GatewayRequest, GatewayResponse, GenerationInput,
    ModelSelector, OutputItem, PrivacyPolicy, RouteProfile,
};
use loom_host::{GenerationConsumerTicket, GenerationOperationLease};
use loom_types::{GenerationMetrics, GenerationProvenance, InferenceEvidenceKind, TokenTrace};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) enum Engine {
    Native(Box<AuthorizedWeaveModel>),
    Server {
        scope: inference::Scope,
        policy: ValidatedWeavePolicy,
    },
}

impl Engine {
    pub fn bind(policy: ValidatedWeavePolicy, state: &PluginState) -> Result<Self, IpcFailure> {
        if let Some(scope) = state.inference.as_ref().and_then(|service| {
            service.scope(matches!(
                policy,
                ValidatedWeavePolicy::AutomaticV2
                    | ValidatedWeavePolicy::AutomaticV3
                    | ValidatedWeavePolicy::AutomaticVisualV4
                    | ValidatedWeavePolicy::LoompadV2 { .. }
            ))
        }) {
            if policy.first_word_choices().is_some() {
                return Err(IpcFailure::new(
                    "distinct_words_unavailable",
                    "This configured writer cannot sample distinct word choices.",
                    false,
                ));
            }
            return Ok(Self::Server { scope, policy });
        }
        AuthorizedWeaveModel::bind(
            policy,
            loaded_model_for_state(state)?,
            &state.build_model_policy,
        )
        .map(|model| Self::Native(Box::new(model)))
    }
    pub fn branch_count(&self) -> u32 {
        match self {
            Self::Native(model) => model.branch_count(),
            Self::Server { policy, .. } => policy.branch_count(),
        }
    }
    pub fn bind_document_kind(
        &self,
        kind: DocumentKind,
    ) -> Result<ResolvedWeavePolicy, IpcFailure> {
        match self {
            Self::Native(model) => model.bind_document_kind(kind),
            Self::Server { policy, .. } => policy.bind_document_kind(kind),
        }
    }
    pub fn admit(&self, gate: &AgencyGate) -> Result<(), IpcFailure> {
        match self {
            Self::Native(model) => model.admit(gate),
            Self::Server { scope, .. } => (if scope.automatic {
                gate.admit_automation()
            } else {
                gate.admit_manual_generation()
            })
            .map_err(|error| IpcFailure::new("generation_blocked", error.to_string(), false)),
        }
    }
    pub fn is_automatic(&self) -> bool {
        match self {
            Self::Native(model) => model.automatic_writer().is_some(),
            Self::Server { scope, .. } => scope.automatic,
        }
    }
    pub fn context_tokens(&self) -> u32 {
        match self {
            Self::Native(model) => resident_context_tokens(model.loaded()),
            Self::Server { scope, .. } => scope.context_tokens,
        }
    }
    pub fn max_cases(&self) -> u32 {
        match self {
            Self::Native(model) => model
                .loaded()
                .descriptor
                .capabilities
                .max_cases
                .min(model.loaded().profile.max_parallel_cases),
            Self::Server { .. } => 4,
        }
    }
    pub fn environment(&self) -> Result<ModelEnvironment, IpcFailure> {
        match self {
            Self::Native(model) => model_environment_from_verified(&model.loaded().descriptor)
                .map_err(|error| IpcFailure::backend(&error)),
            Self::Server { scope, .. } => Ok(scope.environment()),
        }
    }
    pub fn validate_media(
        &self,
        media: &[llama_native_types::MediaInput],
    ) -> Result<(), IpcFailure> {
        match self {
            Self::Native(model) => {
                validate_media_against_resident_model(media, &model.loaded().descriptor)
            }
            Self::Server { .. } => Err(IpcFailure::new(
                "inference_media_unsupported",
                "configured raw-completion servers accept text context; image and audio context require a native multimodal model",
                false,
            )),
        }
    }
}

pub(super) struct Prepared {
    pub identity: GenerationFamilyIdentity,
    pub exact_prefix: String,
    pub context_preamble: String,
    pub prompt_recipe: PromptRecipe,
    pub cases: Vec<ContinuationCase>,
    pub queued_branches: Vec<BranchSnapshot>,
    pub runs: Vec<(GenerationRunId, BranchId)>,
    pub lifecycle_ticket: GenerationConsumerTicket,
    pub lifecycle_lease: GenerationOperationLease,
    pub command_id: CommandId,
    pub source_revision_id: RevisionId,
    pub speculation: Option<LoompadBatch>,
}

#[derive(Debug)]
pub(super) struct Cancellation {
    branches: BTreeMap<BranchId, CancellationToken>,
}

impl Cancellation {
    pub fn cancel_all(&self) {
        for token in self.branches.values() {
            token.cancel();
        }
    }
}

impl BranchCancellation for Cancellation {
    fn cancel_branch(&self, branch_id: BranchId) -> bool {
        self.branches.get(&branch_id).is_some_and(|token| {
            let changed = !token.is_cancelled();
            token.cancel();
            changed
        })
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn submit<R: Runtime>(
    state: &PluginState,
    app: &AppHandle<R>,
    admission: &MutexGuard<'_, ApplicationPhase>,
    scope: inference::Scope,
    prepared: Prepared,
) -> Result<WeaveStarted, IpcFailure> {
    let Prepared {
        identity,
        exact_prefix,
        context_preamble,
        prompt_recipe,
        cases,
        queued_branches,
        runs,
        lifecycle_ticket,
        lifecycle_lease,
        command_id,
        source_revision_id,
        speculation,
    } = prepared;
    let cancellation = Arc::new(Cancellation {
        branches: runs
            .iter()
            .map(|(_, branch)| (*branch, CancellationToken::new()))
            .collect(),
    });
    let setup = || -> Result<_, IpcFailure> {
        state
            .generation_lifecycle
            .start(&lifecycle_lease)
            .map_err(|error| {
                IpcFailure::new(
                    "generation_lifecycle_start_failed",
                    error.to_string(),
                    false,
                )
            })?;
        state
            .generations
            .attach_cancellation(&identity.request_id, cancellation.clone())
            .map_err(|error| IpcFailure::generation_registry(&error))?;
        state
            .generation_workers
            .reserve(&identity.request_id, admission)
    };
    let reservation = match setup() {
        Ok(reservation) => reservation,
        Err(error) => {
            fail_and_release_open_runs(state, &identity, &runs, &error.message, app)?;
            return Err(error);
        }
    };
    let worker_app = app.clone();
    let worker_identity = identity.clone();
    let worker_runs = runs.clone();
    let worker_cancellation = cancellation.clone();
    let gate = Arc::new(GenerationWorkerStartGate::default());
    let worker_gate = gate.clone();
    let exact_prompt_blob_id = BlobId::digest(exact_prefix.as_bytes());
    let worker = match std::thread::Builder::new()
        .name("loom-server-generation".into())
        .spawn(move || {
            worker_gate.wait();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tauri::async_runtime::block_on(run(
                    &worker_app,
                    &worker_identity,
                    &scope,
                    &exact_prefix,
                    &context_preamble,
                    &prompt_recipe,
                    cases,
                    &worker_cancellation,
                ))
            }))
            .unwrap_or_else(|_| {
                Err(IpcFailure::new(
                    "inference_worker_panicked",
                    "server generation stopped unexpectedly",
                    false,
                ))
            });
            let state = worker_app.state::<PluginState>();
            let (persisted, terminal) = match result {
                Ok(class) => (Ok(()), class),
                Err(error) => (
                    fail_open_runs(
                        &state,
                        &worker_identity,
                        &worker_runs,
                        &error.message,
                        &worker_app,
                    ),
                    GenerationTerminalClass::Failed,
                ),
            };
            let finalized = persisted.and_then(|()| {
                release_family_after_terminal_persistence(
                    &state,
                    &worker_identity,
                    &worker_runs,
                    terminal,
                )
            });
            if let Err(error) = finalized {
                let _ = state
                    .generations
                    .mark_terminal_persistence_failure(&worker_identity.request_id, error.message);
            }
        }) {
        Ok(worker) => worker,
        Err(error) => {
            fail_and_release_open_runs(state, &identity, &runs, &error.to_string(), app)?;
            return Err(IpcFailure::new(
                "generation_worker_spawn_failed",
                "could not start the server generation worker",
                true,
            ));
        }
    };
    if let Err(GenerationWorkerAttachError {
        failure,
        worker,
        owner,
    }) = reservation.attach(worker, GenerationWorkerOwner::Server(cancellation))
    {
        owner.cancel_all();
        gate.release();
        let _ = worker.join();
        let _ = owner.shutdown_joined();
        fail_and_release_open_runs(state, &identity, &runs, &failure.message, app)?;
        return Err(failure);
    }
    gate.release();
    lifecycle_ticket.detach();
    Ok(WeaveStarted {
        command_id: command_id.to_string(),
        request_id: identity.request_id,
        project_id: identity.project_id.to_string(),
        session_id: identity.session_id.to_string(),
        document_id: identity.document_id.to_string(),
        source_revision_id: source_revision_id.to_string(),
        exact_prompt_blob_id: exact_prompt_blob_id.to_string(),
        branches: queued_branches,
        speculation,
    })
}

fn request(scope: &inference::Scope, case: &ContinuationCase, prompt: String) -> GatewayRequest {
    GatewayRequest {
        request_id: fte_types::RequestId(case.generation.run_id.to_string()),
        client_id: "loom".into(),
        model: ModelSelector::Priority {
            routes: scope.routes.clone(),
        },
        input: GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: prompt,
                add_bos: false,
            }],
        },
        sampling: fte_types::SamplingOptions {
            max_output_tokens: Some(case.sampling.max_tokens),
            temperature: Some(case.sampling.temperature),
            seed: Some(case.sampling.seed),
            ..Default::default()
        },
        routing: fte_types::RoutingPolicy {
            privacy: PrivacyPolicy::HostedAllowed,
            profile: RouteProfile::Auto,
            retry_before_output: true,
            ..Default::default()
        },
        deadline: fte_types::DeadlinePolicy {
            total_ms: Some(120_000),
            ..Default::default()
        },
        response_format: fte_types::ResponseFormat::default(),
        tools: vec![],
        tool_policy: fte_types::ToolPolicy::default(),
        cache: fte_types::CachePolicy::default(),
        storage: fte_types::StoragePolicy::default(),
        stream: fte_types::StreamPolicy::default(),
        provider_extensions: BTreeMap::new(),
    }
}

async fn complete(
    scope: &inference::Scope,
    request: GatewayRequest,
    cancellation: &CancellationToken,
) -> Result<GatewayResponse, IpcFailure> {
    let request_id = request.request_id.clone();
    let started = AtomicBool::new(false);
    let operation = async {
        started.store(true, Ordering::Release);
        let ticket = scope.service.gateway.execute(request).await?;
        ticket.final_response().await
    };
    tokio::pin!(operation);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            // Cancellation is remembered before admission. If an operation was
            // admitted, explicitly cancel its queue/backend then await release.
            if started.load(Ordering::Acquire) {
                scope.service.gateway.cancel(&request_id, fte_types::CancelTarget::Request);
                let _ = operation.await;
            }
            Err(IpcFailure::new("inference_cancelled", "server generation was cancelled", false))
        }
        result = &mut operation => result.map_err(|error| IpcFailure::new("inference_failed", format!("configured inference failed ({})", error.code), true)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    scope: &inference::Scope,
    exact_prefix: &str,
    context_preamble: &str,
    recipe: &PromptRecipe,
    cases: Vec<ContinuationCase>,
    cancellation: &Cancellation,
) -> Result<GenerationTerminalClass, IpcFailure> {
    let prompt = if context_preamble.is_empty() {
        exact_prefix.to_owned()
    } else {
        format!("{context_preamble}\n\n{exact_prefix}")
    };
    let mut terminal = GenerationTerminalClass::Completed;
    // Independent branches keep independent cancellation and never share a
    // provider response. Bounded native family size also bounds these requests.
    for case in cases {
        let token = &cancellation.branches[&case.generation.branch_id];
        let request = request(scope, &case, prompt.clone());
        let response = complete(scope, request.clone(), token).await;
        let state = app.state::<PluginState>();
        let events = {
            let mut session = lock_session_internal(&state)?;
            let store = require_bound_store(
                &mut session,
                &identity.project_id.to_string(),
                &identity.session_id.to_string(),
            )?;
            if token.is_cancelled() {
                terminal = GenerationTerminalClass::Cancelled;
                let outcome = store
                    .finish_generation(
                        case.generation.run_id,
                        GenerationTerminalStatus::Cancelled,
                        None,
                    )
                    .map_err(IpcFailure::store)?;
                vec![LoomEvent::GenerationTerminal(outcome)]
            } else {
                let response = response?;
                let text = response_text(&request, &response)?;
                let receipt = serde_json::to_vec(&serde_json::json!({
                    "kind": "configured_server_response", "request": request, "response": response,
                    "generation": case.generation, "manuscript_prompt": recipe,
                    "configuration_scope": scope.model_id,
                    "context_preamble_sha256": BlobId::digest(context_preamble.as_bytes()),
                }))
                .map_err(|_| {
                    IpcFailure::new(
                        "inference_receipt_invalid",
                        "could not encode the server receipt",
                        false,
                    )
                })?;
                let receipt_blob = store
                    .store_provenance_blob(&receipt)
                    .map_err(IpcFailure::store)?;
                // Preserve the normalized gateway response explicitly as such.
                // Provider wire bytes, token IDs and native checkpoints are unavailable.
                let response_blob = store
                    .store_provenance_blob(&serde_json::to_vec(&serde_json::json!({"kind": "normalized_gateway_response", "raw_provider_body": "not_retained", "response": response})).map_err(|_| {
                        IpcFailure::new(
                            "inference_receipt_invalid",
                            "could not encode the server response",
                            false,
                        )
                    })?)
                    .map_err(IpcFailure::store)?;
                let trace = TokenTrace {
                    generated_token_ids: vec![],
                    observations: vec![],
                    raw_event_stream_blob_id: response_blob,
                    provenance: Some(GenerationProvenance {
                        evidence_kind: InferenceEvidenceKind::ServerResponse,
                        metrics: GenerationMetrics {
                            prompt_tokens: response.usage.input_tokens,
                            completion_tokens: response.usage.output_tokens,
                            duration_ms: response.usage.total_ms,
                            ..Default::default()
                        },
                        backend_receipt_blob_id: Some(receipt_blob),
                        sequence_state_blob_id: None,
                    }),
                };
                let outcome = store
                    .finish_unverified_generation_candidate_for_diagnostics(
                        case.generation.run_id,
                        TerminalCandidateInput {
                            output_bytes: text.into_bytes(),
                            token_trace: trace,
                        },
                    )
                    .map_err(IpcFailure::store)?;
                vec![
                    LoomEvent::Generation(outcome.candidate_ready_event),
                    LoomEvent::GenerationTerminal(outcome.terminal_event),
                ]
            }
        };
        for event in events {
            let _ = emit_desktop_event(app, identity, event);
        }
    }
    Ok(terminal)
}

fn response_text(
    request: &GatewayRequest,
    response: &GatewayResponse,
) -> Result<String, IpcFailure> {
    let ModelSelector::Priority { routes } = &request.model else {
        unreachable!("server requests use explicit priority")
    };
    if response.request_id != request.request_id
        || response.status != fte_types::TerminalStatus::Completed
        || !routes.iter().any(|route| {
            route.backend_id == response.route.backend_id
                && route.model_id == response.route.model_id
        })
        || response.output.len() != 1
    {
        return Err(IpcFailure::new(
            "inference_response_mismatch",
            "server response does not match the admitted completion route",
            false,
        ));
    }
    let OutputItem::Message { content, .. } = &response.output[0] else {
        return Err(IpcFailure::new(
            "inference_response_mismatch",
            "server returned non-text completion output",
            false,
        ));
    };
    let mut text = String::new();
    for part in content {
        let ContentBlock::Text { text: part } = part else {
            return Err(IpcFailure::new(
                "inference_response_mismatch",
                "server returned non-text completion content",
                false,
            ));
        };
        text.push_str(part);
    }
    if text.len() as u64 > MAX_BRANCH_BODY_BYTES {
        return Err(IpcFailure::new(
            "inference_response_too_large",
            "server completion exceeds the branch size bound",
            false,
        ));
    }
    Ok(text)
}

// These integration fixtures require the supported private project store.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use serde_json::json;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    #[allow(clippy::too_many_lines)]
    fn exercise(cancel: bool, policy: WeavePolicySnapshot) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let arrived = Arc::new(tokio::sync::Notify::new());
        let failed_calls = calls.clone();
        let good_calls = calls.clone();
        let request_arrived = arrived.clone();
        let (address, server) = tauri::async_runtime::block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route("/offline", post(move || {
                let calls = failed_calls.clone();
                async move { calls.lock().unwrap().push("offline"); StatusCode::SERVICE_UNAVAILABLE }
            })).route("/writer", post(move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                let calls = good_calls.clone();
                let arrived = request_arrived.clone();
                async move {
                    assert!(!headers.contains_key("authorization"));
                    assert_eq!(body["prompt"], "Once ");
                    assert_eq!(body["model"], "raw-writer");
                    calls.lock().unwrap().push("writer");
                    arrived.notify_one();
                    if cancel { tokio::time::sleep(Duration::from_secs(2)).await; }
                    Json(json!({ "id": "synthetic-response", "model": "raw-writer", "choices": [{ "index": 0, "text": "there was a quiet room.", "finish_reason": "stop" }], "usage": { "prompt_tokens": 2, "completion_tokens": 6 } }))
                }
            }));
            let server = tauri::async_runtime::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            (address, server)
        });
        let temporary = tempfile::tempdir().unwrap();
        let config_path = temporary.path().join(".loom.toml");
        std::fs::write(&config_path, format!("version = 1\n[inference]\nsuggestions = ['z_offline', 'a_writer']\nweave = ['z_offline', 'a_writer']\n[inference.servers.z_offline]\nendpoint = 'http://{address}/offline'\nmodel = 'raw-writer'\ncontext_tokens = 4096\nauth = 'none'\n[inference.servers.a_writer]\nendpoint = 'http://{address}/writer'\nmodel = 'raw-writer'\ncontext_tokens = 4096\nauth = 'none'\n")).unwrap();
        let service = inference::Service::read(&config_path).unwrap().unwrap();
        let mut state = PluginState::default();
        state.inference = Some(service.clone());
        let mut store =
            initialize_project(&temporary.path().join("Writing"), "Writing".into()).unwrap();
        let original = store.read_document(INITIAL_DOCUMENT).unwrap();
        store
            .save_document_if_source(
                INITIAL_DOCUMENT,
                DocumentContent::Prose("Once ".into()),
                "fixture",
                original.revision_id,
                original.blob_id,
            )
            .unwrap();
        let source = store.read_document(INITIAL_DOCUMENT).unwrap();
        let project = store.manifest().project_id;
        let session_id = CommandId::new();
        {
            let mut session = state.session.lock().unwrap();
            crate::workspace_owner::establish(&mut session, &store);
            session.store = Some(store);
            session.active_session_id = Some(session_id);
            session.phase = SessionPhase::Open;
            session.agency.set_automation_enabled(true);
        }
        let app = mock_builder()
            .manage(state)
            .build(mock_context(noop_assets()))
            .unwrap();
        let state = app.state::<PluginState>();
        assert!(matches!(*state.model.lock().unwrap(), ModelRegistry::Empty));
        let command = CommandId::new();
        // Focus blocks even explicitly configured servers before durable work.
        state.session.lock().unwrap().agency.set_focus_mode(true);
        let start = || {
            tauri::async_runtime::block_on(weave_start(
                project.to_string(),
                session_id.to_string(),
                command.to_string(),
                source.document_id.to_string(),
                INITIAL_DOCUMENT.into(),
                source.revision_id.to_string(),
                source.blob_id.to_string(),
                5,
                policy,
                app.handle().clone(),
                app.state::<PluginState>(),
            ))
        };
        if matches!(
            policy,
            WeavePolicySnapshot::AutomaticV3 {}
                | WeavePolicySnapshot::AutomaticVisualV4 {}
                | WeavePolicySnapshot::LoompadV2 { .. }
        ) {
            assert_eq!(start().unwrap_err().code, "distinct_words_unavailable");
            assert!(calls.lock().unwrap().is_empty());
            tauri::async_runtime::block_on(service.gateway.shutdown()).unwrap();
            server.abort();
            return;
        }
        assert_eq!(start().unwrap_err().code, "generation_blocked");
        assert!(calls.lock().unwrap().is_empty());
        state.session.lock().unwrap().agency.set_focus_mode(false);
        let started = start().unwrap();
        assert_eq!(
            started.speculation.is_some(),
            matches!(policy, WeavePolicySnapshot::LoompadV2 { .. })
        );
        if cancel {
            tauri::async_runtime::block_on(async {
                tokio::time::timeout(Duration::from_secs(5), arrived.notified())
                    .await
                    .unwrap();
            });
            state.generations.cancel_all().unwrap();
        }
        assert!(
            state
                .generations
                .wait_for_session_idle(project, session_id, Duration::from_secs(10))
                .unwrap()
        );
        {
            let mut session = state.session.lock().unwrap();
            let store = session.store.as_mut().unwrap();
            assert_eq!(
                store.read_document(INITIAL_DOCUMENT).unwrap().blob_id,
                source.blob_id
            );
            assert_eq!(started.branches.len(), 4);
            for branch in &started.branches {
                let run = branch.run_id.parse().unwrap();
                assert_eq!(store.generation_terminal_count(run).unwrap(), 1);
                let record = store
                    .branch_record(source.document_id, run, MAX_BRANCH_BODY_BYTES)
                    .unwrap()
                    .unwrap();
                if cancel {
                    assert_eq!(record.status, StoredBranchStatus::Cancelled);
                } else {
                    assert_eq!(record.status, StoredBranchStatus::Completed);
                    assert_eq!(
                        record.output_text.as_deref(),
                        Some("there was a quiet room.")
                    );
                    let evidence = store.generation_terminal_evidence(run).unwrap().unwrap();
                    assert!(evidence.token_trace.generated_token_ids.is_empty());
                    assert_eq!(
                        evidence.token_trace.provenance.unwrap().evidence_kind,
                        InferenceEvidenceKind::ServerResponse
                    );
                }
            }
        }
        assert_eq!(&calls.lock().unwrap()[..2], &["offline", "writer"]);
        let before_replay = calls.lock().unwrap().len();
        // Exact replay must recover durable evidence before model/agency or
        // archive preparation. A newly invalid opt-in file cannot break it.
        {
            let session = state.session.lock().unwrap();
            std::fs::write(
                session
                    .workspace
                    .as_ref()
                    .unwrap()
                    .root
                    .join(::archive_friends::DOTFILE),
                "not valid = [",
            )
            .unwrap();
            session.agency.set_focus_mode(true);
            session.agency.set_automation_enabled(false);
        }

        assert_eq!(start().unwrap().request_id, started.request_id);
        assert_eq!(calls.lock().unwrap().len(), before_replay);
        if cancel {
            assert_eq!(before_replay, 2);
        }
        tauri::async_runtime::block_on(service.gateway.shutdown()).unwrap();
        server.abort();
    }

    #[test]
    fn server_weave_preserves_document_receipts_fallback_and_replay_without_native_model() {
        exercise(false, WeavePolicySnapshot::AutomaticV2 {});
    }

    #[test]
    fn manual_weave_uses_its_explicit_server_scope() {
        exercise(
            false,
            WeavePolicySnapshot::ManualV2 {
                branch_count: 4,
                max_tokens: 32,
                temperature: 0.5,
            },
        );
    }

    #[test]
    fn cancellation_drains_server_work_and_never_dispatches_remaining_branches() {
        exercise(true, WeavePolicySnapshot::AutomaticV2 {});
    }
    #[test]
    fn visual_v4_never_falls_back_to_a_server_without_native_admission_evidence() {
        exercise(false, WeavePolicySnapshot::AutomaticVisualV4 {});
    }

    #[test]
    fn automatic_v3_never_falls_back_to_a_server_without_native_admission_evidence() {
        exercise(false, WeavePolicySnapshot::AutomaticV3 {});
    }
    #[test]
    fn loompad_never_relabels_independent_server_draws_as_distinct_words() {
        exercise(
            false,
            WeavePolicySnapshot::LoompadV2 {
                sample_target: 4,
                batch_offset: 0,
            },
        );
    }
}
