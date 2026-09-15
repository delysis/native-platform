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
            media: Vec::new(),
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

#[cfg(unix)]
#[test]
fn peer_images_and_audio_bind_native_bytes_and_reject_malformed_or_unclaimed_inputs() {
    use loom_backend_llama::{VerifiedMediaCapability, VerifiedMediaKind};
    use loom_cabal::compute::{ComputeMedia, ComputeMediaFormat, ComputeModality};
    use std::io::Cursor;
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("PNG");
    let png = png.into_inner();
    let mut wav = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut wav,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .expect("WAV");
    writer.write_sample(42_i16).expect("sample");
    writer.finalize().expect("finalize");
    let wav = wav.into_inner();
    let mut model =
        crate::tests::test_loaded_model(Path::new("media-fixture.gguf"), "media-fixture");
    assert!(
        model_claim(&model).unwrap().media.is_empty(),
        "a base model alone proves no media support"
    );
    for kind in [VerifiedMediaKind::Image, VerifiedMediaKind::Audio] {
        model
            .descriptor
            .capabilities
            .media
            .push(VerifiedMediaCapability {
                kind,
                projector_required: true,
                accepted_mime_types: None,
                max_objects_per_request: Some(1),
                max_bytes_per_object: Some(1024),
                max_total_bytes_per_request: Some(1024),
            });
    }
    let mut job = input_job(model_claim(&model).unwrap());
    assert_eq!(
        job.grant.model.media,
        [ComputeModality::Image, ComputeModality::Audio]
    );
    job.input.media = vec![
        ComputeMedia::new(ComputeMediaFormat::Png, &png).unwrap(),
        ComputeMedia::new(ComputeMediaFormat::Wav, &wav).unwrap(),
    ];
    let directory = tempfile::tempdir().unwrap();
    let mut store = private_store(directory.path()).unwrap();
    let prepared = prepare(&mut store, &model, &job).expect("native prepared input");
    assert_eq!(prepared.request.media[0].bytes, png);
    assert_eq!(prepared.request.media[1].bytes, wav);
    assert_eq!(
        prepared.request.media[0].kind,
        llama_native_types::MediaKind::Image
    );
    assert_eq!(
        prepared.request.media[1].kind,
        llama_native_types::MediaKind::Audio
    );
    assert_eq!(prepared.request.exact_manuscript_prefix, job.input.prompt);
    let roundtrip = crate::peer_media::encode(&prepared.request.media, &job.grant.model).unwrap();
    assert_eq!(roundtrip, job.input.media);
    job.id = Uuid::new_v4();
    job.input.media[0].format = ComputeMediaFormat::Jpeg;
    assert!(matches!(
        prepare(&mut store, &model, &job),
        Err(ComputeFailure::InputUnsupported)
    ));
    job.input.media[0].format = ComputeMediaFormat::Png;
    job.input.media[1] = ComputeMedia::new(ComputeMediaFormat::Wav, &wav[..wav.len() - 1]).unwrap();
    assert!(matches!(
        prepare(&mut store, &model, &job),
        Err(ComputeFailure::InputUnsupported)
    ));
    job.input.media[1] = ComputeMedia::new(ComputeMediaFormat::Wav, &wav).unwrap();
    job.grant.model.media = vec![ComputeModality::Image];
    assert!(matches!(
        prepare(&mut store, &model, &job),
        Err(ComputeFailure::InputUnsupported)
    ));
    job.grant.model = model_claim(&model).unwrap();
    model.descriptor.capabilities.media[0].max_bytes_per_object = Some(1);
    assert!(matches!(
        prepare(&mut store, &model, &job),
        Err(ComputeFailure::InputUnsupported)
    ));
    assert_eq!(
        store.list_documents().unwrap().len(),
        1,
        "bad input never reached the private manuscript store"
    );
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
fn real_native_model_job_crosses_quic_without_borrowing_the_active_manuscript() {
    real_native_model_job(false);
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at the pinned Gemma 4 catalog model with its projector; executes real image and audio inference through peer compute"]
fn real_native_gemma4_image_and_audio_job_crosses_quic_with_exact_media_evidence() {
    real_native_model_job(true);
}

#[cfg(unix)]
#[allow(clippy::too_many_lines)]
fn real_native_model_job(with_media: bool) {
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
    let model_path = std::env::var("MOM_LLAMA_MODEL_PATH").expect("explicit native model path");
    if with_media {
        tauri::async_runtime::block_on(crate::model_load_catalog_candidate(
            "google.gemma-4-12b-it-qat-q4_0".into(),
            model_path,
            app.handle().clone(),
            app.state::<PluginState>(),
        ))
        .expect("load pinned Gemma 4 model and projector");
    } else {
        tauri::async_runtime::block_on(crate::model_load(
            model_path,
            app.handle().clone(),
            app.state::<PluginState>(),
        ))
        .expect("load verified native model");
    }
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
        if with_media {
            assert!(model.descriptor.projector_sha256.is_some());
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                64,
                64,
                image::Rgb([20, 80, 220]),
            ))
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("image fixture");
            job.input.media.push(
                loom_cabal::compute::ComputeMedia::new(
                    loom_cabal::compute::ComputeMediaFormat::Png,
                    png.get_ref(),
                )
                .expect("exact image input"),
            );
            let mut wav = std::io::Cursor::new(Vec::new());
            let mut writer = hound::WavWriter::new(
                &mut wav,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16_000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .expect("audio fixture");
            for index in 0..16_000 {
                let amplitude: i16 = if index % 40 < 20 { 1_000 } else { -1_000 };
                writer.write_sample(amplitude).expect("audio sample");
            }
            writer.finalize().expect("audio complete");
            job.input.media.push(
                loom_cabal::compute::ComputeMedia::new(
                    loom_cabal::compute::ComputeMediaFormat::Wav,
                    wav.get_ref(),
                )
                .expect("exact audio input"),
            );
            job.input.prompt = "Describe the image and the sound:".into();
        }
        let host_network = Network::start(&host_identity, NetworkMode::Direct {})
            .await
            .expect("host endpoint");
        host_network
            .add(Arc::new(Mutex::new(cabal)))
            .expect("host membership");
        let host = host_network
            .host_compute(&directory.path().join("ledger"), Arc::new(executor))
            .expect("native executor");
        host.grant(job.grant.clone()).expect("explicit grant");
        let peer = Network::start(&peer_identity, NetworkMode::Direct {})
            .await
            .expect("peer endpoint");
        let client_path = directory.path().join("caller");
        let mut client =
            loom_cabal::compute::ComputeClient::open(&client_path, peer_identity.public_key())
                .expect("private caller ledger");
        client
            .prepare(loom_cabal::compute::ClientRequest {
                id: job.id,
                host: host_identity.public_key(),
                grant: job.grant.clone(),
                input: job.input.clone(),
            })
            .expect("persist exact intent before sending");
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
        // Simulate losing the admission response and restarting the caller.
        drop(client);
        let mut client =
            loom_cabal::compute::ComputeClient::open(&client_path, peer_identity.public_key())
                .expect("reopen caller without resubmitting");
        assert!(
            client
                .get(job.id)
                .expect("saved request")
                .expect("prepared job")
                .receipt
                .is_none()
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
        client
            .record((*completed).clone())
            .expect("bind the actual result to saved intent");
        drop(client);
        let client =
            loom_cabal::compute::ComputeClient::open(&client_path, peer_identity.public_key())
                .expect("reopen retained remote result");
        assert_eq!(
            client
                .get(job.id)
                .expect("saved result")
                .expect("job")
                .receipt
                .expect("terminal")
                .hash()
                .expect("retained digest"),
            completed.hash().expect("remote digest")
        );
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
        assert_eq!(
            backend["input_contract"],
            if with_media {
                "raw_completion_with_media_prefix"
            } else {
                "raw_completion"
            }
        );
        assert_eq!(evidence.context_binding.media.len(), job.input.media.len());
        for (binding, input) in evidence.context_binding.media.iter().zip(&job.input.media) {
            assert_eq!(binding.sha256, input.sha256);
            assert_eq!(
                binding.byte_count,
                u64::try_from(input.decode().unwrap().len()).unwrap()
            );
            assert_eq!(binding.mime, input.format.mime());
        }
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
            "Real peer model job: job={}, model_sha256={}, projector_sha256={:?}, media={}, output_sha256={}, bytes={}",
            job.id,
            model.descriptor.model_sha256,
            model.descriptor.projector_sha256,
            evidence.context_binding.media.len(),
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
