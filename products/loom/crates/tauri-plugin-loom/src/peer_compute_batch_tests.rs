use super::*;
use loom_cabal::Identity;
use loom_cabal::compute::{ComputeGrant, ComputeInput, ComputePromptFormat};
use uuid::Uuid;

fn job(model: ComputeModel) -> HostComputeJob {
    let peer = Identity::generate().expect("identity").public_key();
    HostComputeJob {
        id: Uuid::new_v4(),
        peer,
        grant: ComputeGrant {
            id: Uuid::new_v4(),
            cabal: Uuid::new_v4(),
            epoch: 1,
            peer,
            model,
            max_output_tokens: 16,
            max_seconds: 120,
            jobs: 1,
        },
        input: ComputeInput {
            prompt: "A separately authorized raw prompt".into(),
            format: ComputePromptFormat::Raw,
            max_output_tokens: 16,
            seed: 42,
            media: Vec::new(),
        },
    }
}

#[test]
fn batching_does_not_infer_permission_to_share_unknown_prefixes_or_reframe_input() {
    let model = crate::tests::test_loaded_model(Path::new("fixture.gguf"), "fixture");
    let first = job(model_claim(&model).expect("claim"));
    let mut second = first.clone();
    second.id = Uuid::new_v4();
    second.input.seed += 1;
    assert!(compatible(&first, &second));
    second.input.prompt.push_str(" private suffix");
    assert!(!compatible(&first, &second));
    second = first.clone();
    second.input.format = ComputePromptFormat::Function;
    assert!(!compatible(&first, &second));
    second = first.clone();
    second.grant.cabal = Uuid::new_v4();
    assert!(!compatible(&first, &second));
    second = first.clone();
    second.grant.epoch += 1;
    assert!(!compatible(&first, &second));
    second = first.clone();
    second.input.seed = u32::MAX;
    assert!(!compatible(&first, &second));
}

#[cfg(unix)]
#[test]
fn independent_preparation_never_relabels_one_peers_source_as_anothers() {
    let directory = tempfile::tempdir().expect("fixture");
    let mut store = private_store(directory.path()).expect("store");
    let model = crate::tests::test_loaded_model(Path::new("fixture.gguf"), "fixture");
    let first = job(model_claim(&model).expect("claim"));
    let mut second = first.clone();
    second.peer = Identity::generate().expect("other peer").public_key();
    second.grant.peer = second.peer;
    second.grant.id = Uuid::new_v4();
    // The same job UUID is legal when authenticated peers are different.
    let first_prepared = prepare(&mut store, &model, &first).expect("first source");
    let second_prepared = prepare(&mut store, &model, &second).expect("second source");
    assert_ne!(first_prepared.request_id, second_prepared.request_id);
    let first_generation = &first_prepared.request.cases[0].generation;
    let second_generation = &second_prepared.request.cases[0].generation;
    assert_ne!(first_generation.document_id, second_generation.document_id);
    assert_ne!(
        first_generation.source_revision_id,
        second_generation.source_revision_id
    );
    assert_ne!(first_generation.branch_id, second_generation.branch_id);
    let requests = native_requests(&[first_prepared, second_prepared]);
    assert_eq!(requests.len(), 2);
    assert_ne!(requests[0].request_id, requests[1].request_id);
    assert_ne!(requests[0].case_id, requests[1].case_id);
    assert_eq!(requests[0].prompt, requests[1].prompt);
    assert_eq!(store.list_documents().expect("sources").len(), 2);
}

#[test]
fn a_batch_cannot_wait_on_foreground_admission_while_holding_its_idle_lease() {
    let directory = tempfile::tempdir().expect("fixture");
    let mut state = PluginState::with_app_local_data_root(
        Some(directory.path().into()),
        true,
        BuildModelPolicy::default(),
    );
    state.peer_compute = Arc::new(IdleComputeOwner::new(Duration::ZERO));
    let model = crate::tests::test_loaded_model(Path::new("fixture.gguf"), "fixture");
    let first = job(model_claim(&model).expect("claim"));
    let mut second = first.clone();
    second.id = Uuid::new_v4();
    *state.model.lock().expect("model") = ModelRegistry::Loaded(Box::new(model));
    let executor = NativeExecutor::from_state(&state);
    let admission = state.application.lock().expect("foreground owns admission");
    let outcomes = tauri::async_runtime::block_on(executor.execute_batch(vec![
        ComputeBatchJob {
            job: first,
            cancel: CancellationToken::new(),
        },
        ComputeBatchJob {
            job: second,
            cancel: CancellationToken::new(),
        },
    ]));
    assert_eq!(outcomes.len(), 2);
    assert!(
        outcomes
            .iter()
            .all(|item| item.result == Err(ComputeFailure::HostBusy))
    );
    assert!(state.peer_compute.idle());
    assert!(!directory.path().join("peer-compute-writing").exists());
    drop(admission);
}
