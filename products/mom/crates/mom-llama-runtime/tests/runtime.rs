use anyhow::{Result, anyhow};
use mom_llama_runtime::config::{SettingsUpdate, set_data_dir_override_for_tests};
use mom_llama_runtime::{
    ChatSendInput, ChatSendOptions, ConsultStartInput, ConsultStartOptions, EngineCheckOptions,
    KvCachePolicy,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Duration;

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct TestSession {
    _guard: MutexGuard<'static, ()>,
    root: PathBuf,
}

impl TestSession {
    fn new(name: &str) -> Result<Self> {
        let guard = match test_lock().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        mom_llama_runtime::unload_resident_model();
        let root = std::env::temp_dir().join(format!(
            "mom-llama-native-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root)?;
        set_data_dir_override_for_tests(Some(root.clone()));
        Ok(Self {
            _guard: guard,
            root,
        })
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        mom_llama_runtime::unload_resident_model();
        set_data_dir_override_for_tests(None);
    }
}

#[test]
fn engine_check_blocks_without_model_configuration() -> Result<()> {
    let _session = TestSession::new("missing-model")?;
    let result = mom_llama_runtime::engine_check(EngineCheckOptions::default())?;
    assert_eq!(result.status, "blocked");
    assert_eq!(result.readiness, "blocked_missing_model");
    assert_eq!(
        result.blocker.as_ref().map(|blocker| blocker.code.as_str()),
        Some("model_path_missing")
    );
    assert!(!result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    Ok(())
}

#[test]
fn fixture_readiness_never_claims_native_inference() -> Result<()> {
    let _session = TestSession::new("fixture-readiness")?;
    let result = mom_llama_runtime::engine_check(EngineCheckOptions { fake_fixture: true })?;
    assert_eq!(result.readiness, "fake_fixture_exercised");
    assert!(result.receipt.fake_fixture);
    assert!(!result.receipt.real_engine_invoked);
    assert_eq!(
        result.result.as_ref().map(|output| output.runtime.as_str()),
        Some("fake_fixture")
    );
    Ok(())
}

#[test]
fn native_inference_architecture_rejects_network_and_process_authority() -> Result<()> {
    let runtime_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates_dir = runtime_manifest
        .parent()
        .ok_or_else(|| anyhow!("runtime crate has no crates parent"))?;
    let forbidden = [
        "std::net",
        "tokio::net",
        "reqwest",
        "ureq",
        "hyper::",
        "http://",
        "https://",
        "127.0.0.1",
        "localhost",
        "std::process",
        "Command::new",
    ];
    for crate_name in ["llama-native-types", "llama-native-engine"] {
        assert_source_tree_excludes(&crates_dir.join(crate_name).join("src"), &forbidden)?;
        let manifest = fs::read_to_string(crates_dir.join(crate_name).join("Cargo.toml"))?;
        for dependency in ["reqwest", "ureq", "hyper", "tokio"] {
            assert!(
                !manifest.contains(dependency),
                "{crate_name} must not depend on {dependency}"
            );
        }
    }

    let runtime_src = runtime_manifest.join("src");
    for entry in fs::read_dir(&runtime_src)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs")
            || path.file_name().and_then(|value| value.to_str()) == Some("mcp.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{} contains forbidden native inference authority {needle}",
                path.display()
            );
        }
    }
    let mcp = fs::read_to_string(runtime_src.join("mcp.rs"))?;
    assert!(mcp.contains("std::process"));
    assert!(!mcp.contains("std::net"));
    assert!(!mcp.contains("127.0.0.1"));
    Ok(())
}

fn assert_source_tree_excludes(root: &Path, forbidden: &[&str]) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            assert_source_tree_excludes(&path, forbidden)?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{} contains forbidden native inference authority {needle}",
                path.display()
            );
        }
    }
    Ok(())
}

#[test]
fn upstream_sampling_settings_drive_the_native_sampler_dto() -> Result<()> {
    let _session = TestSession::new("sampling")?;
    let values = BTreeMap::from([
        ("temperature".to_string(), json!(0.35)),
        ("dynatemp_range".to_string(), json!(0.2)),
        ("dynatemp_exponent".to_string(), json!(1.2)),
        ("top_k".to_string(), json!(24)),
        ("top_p".to_string(), json!(0.82)),
        ("min_p".to_string(), json!(0.08)),
        ("typ_p".to_string(), json!(0.91)),
        ("xtc_probability".to_string(), json!(0.15)),
        ("xtc_threshold".to_string(), json!(0.12)),
        ("repeat_last_n".to_string(), json!(96)),
        ("repeat_penalty".to_string(), json!(1.08)),
        ("frequency_penalty".to_string(), json!(0.1)),
        ("presence_penalty".to_string(), json!(0.2)),
        ("dry_multiplier".to_string(), json!(0.7)),
        ("dry_base".to_string(), json!(1.9)),
        ("dry_allowed_length".to_string(), json!(3)),
        ("dry_penalty_last_n".to_string(), json!(128)),
        (
            "samplers".to_string(),
            json!("penalties;dry;top_k;typ_p;top_p;min_p;xtc;temperature"),
        ),
        ("max_tokens".to_string(), json!(256)),
    ]);
    let updated = mom_llama_runtime::settings_update(SettingsUpdate {
        upstream_settings: Some(values),
        ..SettingsUpdate::default()
    })?;
    let settings = updated
        .result
        .ok_or_else(|| anyhow!("settings update returned no result"))?;
    let sampling = settings.sampling_config();
    assert_eq!(sampling.temperature, 0.35);
    assert_eq!(sampling.dynamic_temperature_range, 0.2);
    assert_eq!(sampling.dynamic_temperature_exponent, 1.2);
    assert_eq!(sampling.top_k, 24);
    assert_eq!(sampling.top_p, 0.82);
    assert_eq!(sampling.min_p, 0.08);
    assert_eq!(sampling.typical_p, 0.91);
    assert_eq!(sampling.xtc_probability, 0.15);
    assert_eq!(sampling.repeat_last_n, 96);
    assert_eq!(sampling.repeat_penalty, 1.08);
    assert_eq!(sampling.frequency_penalty, 0.1);
    assert_eq!(sampling.presence_penalty, 0.2);
    assert_eq!(sampling.dry_multiplier, 0.7);
    assert_eq!(sampling.dry_base, 1.9);
    assert_eq!(sampling.dry_allowed_length, 3);
    assert_eq!(sampling.dry_penalty_last_n, 128);
    assert_eq!(sampling.max_tokens, 256);
    assert_eq!(sampling.sampler_order.len(), 8);
    Ok(())
}

#[test]
fn fixture_chat_persists_in_encrypted_sqlite_and_stays_labeled() -> Result<()> {
    let session = TestSession::new("fixture-chat")?;
    let result = mom_llama_runtime::chat_send(
        ChatSendInput {
            conversation_id: "fixture-chat".to_string(),
            message: "private fixture phrase 7419".to_string(),
        },
        ChatSendOptions {
            timeout_s: 1.0,
            fake_fixture: true,
        },
    )?;
    assert_eq!(result.readiness, "fake_fixture_exercised");
    assert!(result.receipt.fake_fixture);
    assert!(!result.receipt.real_engine_invoked);
    let selected = mom_llama_runtime::conversation_select("fixture-chat")?;
    assert!(selected.result.as_ref().is_some_and(|conversation| {
        conversation
            .messages
            .iter()
            .any(|message| message.content == "private fixture phrase 7419")
    }));
    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(b"private fixture phrase 7419".len())
            .any(|window| window == b"private fixture phrase 7419")
    );
    Ok(())
}

#[test]
fn conversations_search_skills_and_settings_survive_restart() -> Result<()> {
    let session = TestSession::new("persistence")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Garden planning".to_string()))?
        .result
        .ok_or_else(|| anyhow!("conversation was not created"))?;
    mom_llama_runtime::chat_send(
        ChatSendInput {
            conversation_id: conversation.id.clone(),
            message: "purple basil seedlings".to_string(),
        },
        ChatSendOptions {
            timeout_s: 1.0,
            fake_fixture: true,
        },
    )?;
    let skill = mom_llama_runtime::skill_store::skill_create(
        "Friendly explainer".to_string(),
        "Explain gently".to_string(),
        "Use simple, kind language.".to_string(),
        "Apply when clarity matters.".to_string(),
        KvCachePolicy::PromptPrefix,
    )?
    .result
    .ok_or_else(|| anyhow!("skill was not created"))?;
    let updated = mom_llama_runtime::skill_store::skill_update(
        &skill.id,
        "Friendly guide".to_string(),
        "Explain gently and accurately".to_string(),
        "Use simple, kind language and name uncertainty.".to_string(),
        "Apply when clarity matters.".to_string(),
        KvCachePolicy::KvCacheCandidate,
    )?;
    assert_eq!(
        updated.result.as_ref().map(|skill| skill.name.as_str()),
        Some("Friendly guide")
    );
    mom_llama_runtime::skill_store::skill_apply(&conversation.id, &skill.id)?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        temperature: Some(0.25),
        context_tokens: Some(4096),
        max_parallel_sequences: Some(4),
        ..SettingsUpdate::default()
    })?;

    set_data_dir_override_for_tests(None);
    mom_llama_runtime::unload_resident_model();
    set_data_dir_override_for_tests(Some(session.path().to_path_buf()));

    let search = mom_llama_runtime::conversation_search("basil")?;
    assert_eq!(search.result.as_ref().map(Vec::len), Some(1));
    let skills = mom_llama_runtime::skill_store::skill_list()?;
    assert_eq!(skills.result.as_ref().map(Vec::len), Some(1));
    let settings = mom_llama_runtime::settings_get()?;
    assert_eq!(
        settings
            .result
            .as_ref()
            .map(|settings| settings.default_temperature),
        Some(0.25)
    );
    assert_eq!(
        settings
            .result
            .as_ref()
            .map(|settings| settings.context_tokens),
        Some(4096)
    );
    Ok(())
}

#[test]
fn attachment_payload_is_encrypted_and_multimodal_is_honestly_blocked() -> Result<()> {
    let session = TestSession::new("attachment")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Photo".to_string()))?
        .result
        .ok_or_else(|| anyhow!("conversation was not created"))?;
    let image = session.path().join("garden.png");
    let payload = b"not-a-real-png-private-attachment-5831";
    std::fs::write(&image, payload)?;
    let imported = mom_llama_runtime::attachment_import(&conversation.id, &image)?;
    let output = imported
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("attachment result missing"))?;
    assert!(output.attachment.stored_path.starts_with("encrypted://"));
    assert!(!output.multimodal_ready);
    assert_eq!(
        output
            .multimodal_blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("mmproj_path_missing")
    );
    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(payload.len())
            .any(|window| window == payload)
    );
    let chat = mom_llama_runtime::chat_send(
        ChatSendInput {
            conversation_id: conversation.id,
            message: "Describe the image.".to_string(),
        },
        ChatSendOptions {
            timeout_s: 1.0,
            fake_fixture: true,
        },
    )?;
    assert_eq!(chat.readiness, "blocked_missing_mmproj");
    Ok(())
}

#[test]
fn deprecated_server_aliases_report_only_in_process_residency() -> Result<()> {
    let _session = TestSession::new("resident-alias")?;
    let status = mom_llama_runtime::server_status()?;
    let value = serde_json::to_value(&status)?;
    assert_eq!(value["result"]["transport"], "in_process");
    assert_eq!(value["result"]["running"], false);
    let raw = serde_json::to_string(&value)?;
    assert!(!raw.contains("127.0.0.1"));
    assert!(!raw.contains("http://"));
    assert!(!raw.contains("server_path"));
    assert!(!status.receipt.real_engine_invoked);
    Ok(())
}

#[test]
fn consult_fixture_is_bounded_and_cannot_promote_readiness() -> Result<()> {
    let _session = TestSession::new("consult-fixture")?;
    let result = mom_llama_runtime::consult_start(
        ConsultStartInput {
            conversation_id: "consult-fixture".to_string(),
            prompt: "What assumptions should be checked?".to_string(),
            panel_id: None,
        },
        ConsultStartOptions {
            timeout_s: 1.0,
            fake_fixture: true,
        },
    )?;
    assert_eq!(result.readiness, "fake_fixture_exercised");
    assert!(result.receipt.fake_fixture);
    assert!(!result.receipt.real_engine_invoked);
    assert_eq!(result.result.as_ref().map(|run| run.seats.len()), Some(4));
    assert!(
        result
            .result
            .as_ref()
            .is_some_and(|run| !run.medical_authority)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn mcp_process_authority_is_explicit_bounded_and_receipted() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let session = TestSession::new("mcp")?;
    let response = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "tools": [{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}],
            "content": [{"type":"text","text":"echo ok"}]
        }
    });
    let body = serde_json::to_string(&response)?;
    let executable = session.path().join("mcp-fixture");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf 'Content-Length: {}\\r\\n\\r\\n{}'\n",
            body.len(),
            body.replace('\'', "'\\''")
        ),
    )?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        upstream_settings: Some(BTreeMap::from([(
            "mcpNativeEnabled".to_string(),
            json!(true),
        )])),
        ..SettingsUpdate::default()
    })?;
    mom_llama_runtime::mcp_configure("fixture".to_string(), executable, Vec::new(), true)?;
    let tools = mom_llama_runtime::mcp_list_tools("fixture")?;
    assert_eq!(tools.readiness, "host_integrated");
    assert_eq!(
        tools
            .result
            .as_ref()
            .and_then(|tools| tools.first())
            .map(|tool| tool.name.as_str()),
        Some("echo")
    );
    assert!(!tools.receipt.real_engine_invoked);
    Ok(())
}

fn configured_real_session(name: &str) -> Result<Option<TestSession>> {
    let Some(model_path) = std::env::var_os("MOM_LLAMA_MODEL_PATH").map(PathBuf::from) else {
        return Ok(None);
    };
    if !model_path.is_file() {
        return Ok(None);
    }
    let session = TestSession::new(name)?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(model_path),
        max_tokens: Some(96),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    Ok(Some(session))
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_chat_invokes_no_fixture_and_persists() -> Result<()> {
    let Some(_session) = configured_real_session("real-chat")? else {
        return Ok(());
    };
    let result = mom_llama_runtime::chat_send(
        ChatSendInput {
            conversation_id: "real-chat".to_string(),
            message: "Reply with exactly two friendly words.".to_string(),
        },
        ChatSendOptions::default(),
    )?;
    assert_eq!(result.readiness, "real_prompt_smoke_passed");
    assert!(result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    assert!(
        result
            .result
            .as_ref()
            .is_some_and(|output| !output.assistant_text.trim().is_empty())
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_four_seat_consult_cancels_one_and_synthesizes_terminal_sources() -> Result<()> {
    let Some(_session) = configured_real_session("real-consult")? else {
        return Ok(());
    };
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(192),
        ..SettingsUpdate::default()
    })?;
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let handle = std::thread::spawn(move || {
        mom_llama_runtime::consult_start_stream(
            ConsultStartInput {
                conversation_id: "real-consult".to_string(),
                prompt: "Give a careful short plan for preparing a virtual consultation."
                    .to_string(),
                panel_id: None,
            },
            ConsultStartOptions::default(),
            Some(|event: mom_llama_runtime::ConsultStreamEvent| {
                if event.event == "delta" {
                    let _ = started_tx.try_send(event.run_id);
                }
                Ok(())
            }),
        )
    });
    let run_id = started_rx
        .recv_timeout(Duration::from_secs(120))
        .map_err(|error| anyhow!("consult did not start streaming: {error}"))?;
    let cancelled = mom_llama_runtime::consult_cancel(&run_id, Some("skeptical"))?;
    assert_eq!(
        cancelled
            .result
            .as_ref()
            .map(|result| result.cancelled_sequences),
        Some(1)
    );
    let result = handle
        .join()
        .map_err(|_| anyhow!("consult worker panicked"))??;
    let run = result
        .result
        .ok_or_else(|| anyhow!("consult result missing"))?;
    assert_eq!(run.seats.len(), 4);
    assert!(run.seats.iter().any(|seat| {
        seat.seat_id == "skeptical" && seat.state == llama_native_types::GenerationState::Cancelled
    }));
    assert!(
        run.seats
            .iter()
            .filter(|seat| { seat.state == llama_native_types::GenerationState::Completed })
            .count()
            >= 1
    );
    let synthesis = mom_llama_runtime::consult_synthesize(&run.id, Vec::new())?;
    assert!(synthesis.result.as_ref().is_some_and(|value| {
        value.derived && !value.source_receipt_ids.is_empty() && !value.text.trim().is_empty()
    }));
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_kv_cache_save_restore_proves_equivalence() -> Result<()> {
    let Some(_session) = configured_real_session("real-cache")? else {
        return Ok(());
    };
    let skill = mom_llama_runtime::skill_store::skill_create(
        "Cache proof".to_string(),
        "Deterministic cache verification".to_string(),
        "Answer concisely and accurately.".to_string(),
        "Use for cache verification.".to_string(),
        KvCachePolicy::PromptPrefix,
    )?
    .result
    .ok_or_else(|| anyhow!("cache skill missing"))?;
    let saved = mom_llama_runtime::kv_cache_save(Some(skill.id))?;
    assert_eq!(saved.readiness, "prompt_smoke_verified");
    let cache_id = saved
        .result
        .as_ref()
        .map(|entry| entry.id.clone())
        .ok_or_else(|| anyhow!("cache metadata missing"))?;
    let restored = mom_llama_runtime::kv_cache_restore(Some(cache_id))?;
    assert_eq!(restored.readiness, "prompt_smoke_verified");
    assert!(restored.receipt.real_engine_invoked);
    Ok(())
}
