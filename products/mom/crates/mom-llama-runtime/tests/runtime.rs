use anyhow::{Context, Result, anyhow};
use llama_native_types::{
    CompletionPrompt, GenerationInput as NativeGenerationInput,
    GenerationRequest as NativeGenerationRequest, GenerationState, SamplingConfig,
    SpecialTokenPolicy,
};
use mom_llama_runtime::config::{SettingsUpdate, set_data_dir_override_for_tests};
use mom_llama_runtime::{
    AttachmentPreviewAnchor, AttachmentPreviewCatalog, ChatDispatchOutput, ChatSendInput,
    ChatSendOptions, ChatSendOutput, Conversation, ConversationExecutionProfile, ConversationKind,
    EngineCheckOptions, KvCachePolicy, MentionDispatchInput, Message, MessageAttribution,
    MessageRole, MessageSpeakerKind, PersonaFreezeInput, PersonaHistoryMode,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

const VALID_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\
    \x00\x00\x00\x0dIHDR\x00\x00\x00\x02\x00\x00\x00\x04\x08\x02\x00\x00\x00\x2b\x8d\x79\x6e\
    \x00\x00\x00\x09pHYs\x00\x00\x00\x01\x00\x00\x00\x01\x00\x4f\x25\xc4\xd6\
    \x00\x00\x00\x10IDAT\x78\x9c\x63\xfc\xc3\x00\x02\x2c\x0c\x58\x28\x00\x1b\x74\x01\x0a\x5f\x82\xdc\x5d\
    \x00\x00\x00\x00IEND\xae\x42\x60\x82";

fn primary_preview_anchor(catalog: &AttachmentPreviewCatalog) -> Result<AttachmentPreviewAnchor> {
    let artifact = catalog
        .primary
        .as_ref()
        .ok_or_else(|| anyhow!("canonical preview has no primary artifact"))?;
    Ok(AttachmentPreviewAnchor {
        attachment_id: catalog.attachment_id.clone(),
        root_sha256: catalog.root_sha256.clone(),
        artifact_id: artifact.artifact_id.clone(),
        policy_fingerprint: catalog.policy_fingerprint.clone(),
    })
}

type EncryptedDocumentSnapshot = BTreeMap<String, (Vec<u8>, Vec<u8>, i64)>;

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn encrypted_document_snapshot(data_dir: &Path) -> Result<EncryptedDocumentSnapshot> {
    let connection = rusqlite::Connection::open(data_dir.join("runtime.sqlite3"))?;
    let mut statement = connection.prepare(
        "SELECT namespace, nonce, ciphertext, updated_at
         FROM encrypted_documents ORDER BY namespace",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, i64>(3)?,
            ),
        ))
    })?;
    Ok(rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()?)
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
        set_data_dir_override_for_tests(None);
    }
}

fn attributed_history_fixture(id: &str) -> Conversation {
    let message = |message_id: &str,
                   parent_id: Option<&str>,
                   role: MessageRole,
                   content: &str,
                   attribution: Option<MessageAttribution>| Message {
        id: message_id.to_string(),
        conversation_id: id.to_string(),
        role,
        content: content.to_string(),
        created_at: message_id.to_string(),
        parent_id: parent_id.map(str::to_string),
        model: None,
        receipt_id: None,
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution,
        attachment_ids: Vec::new(),
    };
    let attribution = |source: &str, handle: &str, order: usize| MessageAttribution {
        kind: MessageSpeakerKind::Persona,
        source_id: source.to_string(),
        handle: handle.to_string(),
        label: handle.to_string(),
        version: 1,
        invocation_id: "shared-invocation".to_string(),
        target_order: order,
        incomplete: false,
    };
    Conversation {
        id: id.to_string(),
        title: "Attributed response history".to_string(),
        created_at: "0".to_string(),
        updated_at: "5".to_string(),
        kind: ConversationKind::Chat,
        execution_profile: ConversationExecutionProfile::default(),
        selected_model_path: None,
        source_conversation_id: None,
        source_message_id: None,
        branch_root_message_id: None,
        active_leaf_message_id: Some("5-host-answer".to_string()),
        recipient_ids: Vec::new(),
        current_skill_ids: Vec::new(),
        messages: vec![
            message("1-user", None, MessageRole::User, "Ask both", None),
            message(
                "2-first-peer",
                Some("1-user"),
                MessageRole::Assistant,
                "First perspective",
                Some(attribution("first", "first", 0)),
            ),
            message(
                "3-second-peer",
                Some("2-first-peer"),
                MessageRole::Assistant,
                "Second perspective",
                Some(attribution("second", "second", 1)),
            ),
            message(
                "4-follow-up",
                Some("3-second-peer"),
                MessageRole::User,
                "Follow up after both",
                None,
            ),
            message(
                "5-host-answer",
                Some("4-follow-up"),
                MessageRole::Assistant,
                "Host answer after both",
                None,
            ),
        ],
        inference_window: None,
    }
}

#[test]
fn engine_check_blocks_without_model_configuration() -> Result<()> {
    let _session = TestSession::new("missing-model")?;
    let owner = initialize_product_runtime()?;
    let scope = owner.operation_scope();
    let result = mom_llama_runtime::engine_check(&scope, EngineCheckOptions::default())?;
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
fn settings_paths_can_be_replaced_and_explicitly_cleared() -> Result<()> {
    let _session = TestSession::new("settings-clear-paths")?;
    let model = PathBuf::from("/models/local.gguf");
    let mmproj = PathBuf::from("/models/local-mmproj.gguf");
    let replaced = mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(Some(model.clone())),
        mmproj_path: Some(Some(mmproj.clone())),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("replacement settings missing"))?;
    assert_eq!(replaced.model_path, Some(model.clone()));
    assert_eq!(replaced.mmproj_path, Some(mmproj.clone()));

    let unchanged = mom_llama_runtime::settings_update(SettingsUpdate::default())?
        .result
        .ok_or_else(|| anyhow!("unchanged settings missing"))?;
    assert_eq!(unchanged.model_path, replaced.model_path);
    assert_eq!(unchanged.mmproj_path, replaced.mmproj_path);

    let next_model = PathBuf::from("/models/next.gguf");
    let model_changed = mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(Some(next_model.clone())),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("model-only settings missing"))?;
    assert_eq!(model_changed.model_path, Some(next_model));
    assert_eq!(model_changed.mmproj_path, None);
    assert_eq!(
        model_changed.upstream_settings.get("mmprojPath"),
        Some(&json!("")),
        "a model-only patch must never retain the previous model's projector"
    );

    let cleared = mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(None),
        mmproj_path: Some(None),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("cleared settings missing"))?;
    assert_eq!(cleared.model_path, None);
    assert_eq!(cleared.mmproj_path, None);
    assert_eq!(
        cleared.upstream_settings.get("mmprojPath"),
        Some(&json!(""))
    );
    let persisted = mom_llama_runtime::settings_get()?
        .result
        .ok_or_else(|| anyhow!("persisted settings missing"))?;
    assert_eq!(persisted.model_path, None);
    assert_eq!(persisted.mmproj_path, None);
    assert_eq!(
        persisted.upstream_settings.get("mmprojPath"),
        Some(&json!(""))
    );
    Ok(())
}

#[test]
fn legacy_disable_reasoning_parsing_cannot_leak_direct_chat_fixture_output() -> Result<()> {
    let _session = TestSession::new("retired-reasoning-parser-setting")?;
    let mut stored_settings = mom_llama_runtime::settings_get()?
        .result
        .ok_or_else(|| anyhow!("settings missing"))?;
    // Simulate an older encrypted settings document. This key is no longer a
    // writable control, but old documents remain readable without migration.
    stored_settings
        .upstream_settings
        .insert("disableReasoningParsing".into(), json!(true));
    mom_llama_runtime::config::save_settings(&stored_settings)?;
    assert_eq!(
        mom_llama_runtime::settings_get()?
            .result
            .ok_or_else(|| anyhow!("settings missing"))?
            .upstream_settings["disableReasoningParsing"],
        json!(true)
    );

    let conversation = mom_llama_runtime::conversation_new(Some("Parser fixture".to_string()))?
        .result
        .ok_or_else(|| anyhow!("conversation missing"))?;
    let result = mom_llama_runtime::chat_send_in_scope(
        &mom_llama_runtime::OperationScope::detached(),
        ChatSendInput {
            conversation_id: conversation.id.clone(),
            message: "[[fixture:reasoning-marker-regression]]".into(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    assert_eq!(result.readiness, "fake_fixture_exercised");
    assert!(!result.receipt.real_engine_invoked);
    assert!(result.receipt.fake_fixture);
    let output = result
        .result
        .ok_or_else(|| anyhow!("fixture output missing"))?;
    assert_eq!(output.assistant_text, "Fixture assistant response.");
    assert_eq!(
        output.reasoning_content.as_deref(),
        Some("fixture private reasoning")
    );

    let stored = mom_llama_runtime::conversation_select(&conversation.id)?
        .result
        .ok_or_else(|| anyhow!("conversation missing after fixture"))?;
    let assistant = stored
        .messages
        .iter()
        .find(|message| message.id == output.assistant_message_id)
        .ok_or_else(|| anyhow!("assistant fixture message missing"))?;
    assert_eq!(assistant.content, "Fixture assistant response.");
    assert_eq!(
        assistant.reasoning_content.as_deref(),
        Some("fixture private reasoning")
    );
    assert!(!assistant.content.contains("<think>"));
    assert!(!assistant.content.contains("</think>"));
    Ok(())
}

#[test]
fn cache_mode_is_coherent_and_off_blocks_manual_cache_access() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("cache-policy-surface")?;
    let defaults = mom_llama_runtime::settings_get()?
        .result
        .ok_or_else(|| anyhow!("default settings missing"))?;
    assert_eq!(defaults.kv_cache_policy, KvCachePolicy::KvCacheCandidate);
    assert_eq!(
        defaults.upstream_settings.get("preEncodeConversation"),
        Some(&json!(true))
    );

    let off = mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::None),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("off settings missing"))?;
    assert_eq!(off.kv_cache_policy, KvCachePolicy::None);
    assert_eq!(
        off.upstream_settings.get("preEncodeConversation"),
        Some(&json!(false))
    );
    assert_eq!(
        mom_llama_runtime::kv_cache_status()?
            .result
            .ok_or_else(|| anyhow!("cache status missing"))?
            .status,
        mom_llama_runtime::kv_cache::KvCacheState::Disabled
    );
    for blocked in [
        mom_llama_runtime::kv_cache_save(&scope, None)?,
        mom_llama_runtime::kv_cache_restore(&scope, None)?,
    ] {
        assert_eq!(blocked.status, "blocked");
        assert_eq!(
            blocked.blocker.map(|blocker| blocker.code),
            Some("kv_cache_policy_disabled".to_string())
        );
    }

    let automatic = mom_llama_runtime::settings_update(SettingsUpdate {
        upstream_settings: Some(BTreeMap::from([(
            "preEncodeConversation".to_string(),
            json!(true),
        )])),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("automatic settings missing"))?;
    assert_eq!(automatic.kv_cache_policy, KvCachePolicy::KvCacheCandidate);

    let prefixes = mom_llama_runtime::settings_update(SettingsUpdate {
        upstream_settings: Some(BTreeMap::from([(
            "preEncodeConversation".to_string(),
            json!(false),
        )])),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("prefix settings missing"))?;
    assert_eq!(prefixes.kv_cache_policy, KvCachePolicy::PromptPrefix);
    Ok(())
}

#[test]
fn skill_cache_policy_controls_stable_prefix_ownership() -> Result<()> {
    let _session = TestSession::new("skill-cache-policy")?;
    let skill = mom_llama_runtime::skill_store::skill_create(
        "Careful reader".to_string(),
        "Read closely".to_string(),
        "Separate observations from interpretations.".to_string(),
        "Apply before answering.".to_string(),
        KvCachePolicy::None,
    )?
    .result
    .ok_or_else(|| anyhow!("skill missing"))?;
    let uncached =
        mom_llama_runtime::skill_store::applied_skill_prompt(std::slice::from_ref(&skill.id))?;
    assert!(uncached.cache_owner_id.is_none());

    let updated = mom_llama_runtime::skill_store::skill_update(
        &skill.id,
        skill.name,
        skill.description,
        "Separate observations, interpretations, and uncertainty.".to_string(),
        skill.usage_hint,
        KvCachePolicy::PromptPrefix,
    )?
    .result
    .ok_or_else(|| anyhow!("updated skill missing"))?;
    let cached = mom_llama_runtime::skill_store::applied_skill_prompt(&[updated.id])?;
    assert!(cached.cache_owner_id.is_some());
    assert!(cached.prompt.contains("uncertainty"));
    Ok(())
}

#[test]
fn updating_a_conversation_does_not_reorder_the_sidebar() -> Result<()> {
    let _session = TestSession::new("stable-conversation-order")?;
    let first = mom_llama_runtime::conversation_new(Some("First".to_string()))?
        .result
        .ok_or_else(|| anyhow!("first conversation missing"))?;
    let second = mom_llama_runtime::conversation_new(Some("Second".to_string()))?
        .result
        .ok_or_else(|| anyhow!("second conversation missing"))?;
    let (db, mut updated) =
        mom_llama_runtime::conversation_store::get_or_create_conversation(&first.id)?;
    updated.updated_at = "9999999999999".to_string();
    mom_llama_runtime::conversation_store::upsert_conversation(db, updated)?;

    let conversations = mom_llama_runtime::conversation_list()?
        .result
        .ok_or_else(|| anyhow!("conversation list missing"))?;
    let chat_ids = conversations
        .iter()
        .filter(|conversation| conversation.kind == mom_llama_runtime::ConversationKind::Chat)
        .map(|conversation| conversation.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(chat_ids, vec![second.id.as_str(), first.id.as_str()]);
    Ok(())
}

#[test]
fn conversation_system_message_is_scoped_persistent_and_clearable() -> Result<()> {
    let _session = TestSession::new("conversation-system-message")?;
    let first = mom_llama_runtime::conversation_new(Some("First".to_string()))?
        .result
        .ok_or_else(|| anyhow!("first conversation missing"))?;
    let second = mom_llama_runtime::conversation_new(Some("Second".to_string()))?
        .result
        .ok_or_else(|| anyhow!("second conversation missing"))?;

    let updated = mom_llama_runtime::conversation_system_message_update(
        &first.id,
        Some("  Answer like a careful gardener.  ".to_string()),
    )?
    .result
    .ok_or_else(|| anyhow!("updated conversation missing"))?;
    assert_eq!(
        updated.execution_profile.system_message.as_deref(),
        Some("Answer like a careful gardener.")
    );
    assert_eq!(
        mom_llama_runtime::conversation_select(&first.id)?
            .result
            .and_then(|conversation| conversation.execution_profile.system_message),
        Some("Answer like a careful gardener.".to_string())
    );
    assert_eq!(
        mom_llama_runtime::conversation_select(&second.id)?
            .result
            .and_then(|conversation| conversation.execution_profile.system_message),
        None,
        "one chat's instructions must not leak into another chat"
    );

    let cleared = mom_llama_runtime::conversation_system_message_update(&first.id, None)?
        .result
        .ok_or_else(|| anyhow!("cleared conversation missing"))?;
    assert_eq!(cleared.execution_profile.system_message, None);
    Ok(())
}

#[test]
fn freezing_and_sending_from_a_persona_never_mutates_its_source_or_template() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("persona-freeze-isolation")?;
    let source = mom_llama_runtime::conversation_new(Some("Source interview".to_string()))?
        .result
        .ok_or_else(|| anyhow!("source conversation missing"))?;
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "A stable source fact".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source_before = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .ok_or_else(|| anyhow!("source snapshot missing"))?;
    let leaf = source_before
        .active_leaf_message_id
        .clone()
        .ok_or_else(|| anyhow!("source leaf missing"))?;
    let persona = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: leaf,
        name: "Stable witness".to_string(),
        mention_handle: "stable-witness".to_string(),
        history_mode: PersonaHistoryMode::Full,
    })?
    .result
    .ok_or_else(|| anyhow!("persona missing"))?;
    assert_eq!(persona.execution_profile.version, 1);
    assert_eq!(
        mom_llama_runtime::conversation_select(&source.id)?.result,
        Some(source_before.clone()),
        "freezing must not rewrite the source chat"
    );

    let template_before = mom_llama_runtime::persona_get(&persona.id)?
        .result
        .ok_or_else(|| anyhow!("persona template missing"))?;
    let dispatched = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: persona.id.clone(),
            message: "Continue in an ordinary chat".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let ChatDispatchOutput::Direct {
        conversation_id, ..
    } = dispatched
        .result
        .ok_or_else(|| anyhow!("dispatch missing"))?
    else {
        return Err(anyhow!("persona send must instantiate a direct chat"));
    };
    assert_ne!(conversation_id, persona.id);
    assert_eq!(
        mom_llama_runtime::persona_get(&persona.id)?.result,
        Some(template_before),
        "ordinary traffic must never accumulate in the persona template"
    );
    assert_eq!(
        mom_llama_runtime::conversation_select(&source.id)?.result,
        Some(source_before),
        "persona traffic must never write back into the source chat"
    );
    Ok(())
}

#[test]
fn mention_snapshots_are_version_pinned_ordered_and_source_isolated() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("mention-snapshot-isolation")?;
    let source = mom_llama_runtime::conversation_new(Some("Persona source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("source missing"))?;
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "Remember exactly this".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .ok_or_else(|| anyhow!("source reload missing"))?;
    let leaf = source
        .active_leaf_message_id
        .clone()
        .expect("source leaf missing");
    let first = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: leaf.clone(),
        name: "First lens".to_string(),
        mention_handle: "first-lens".to_string(),
        history_mode: PersonaHistoryMode::Full,
    })?
    .result
    .expect("first Persona missing");
    let second = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: leaf,
        name: "Second lens".to_string(),
        mention_handle: "second-lens".to_string(),
        history_mode: PersonaHistoryMode::SystemOnly,
    })?
    .result
    .expect("second Persona missing");
    let group = mom_llama_runtime::persona_group_create(
        "Ordered pair".to_string(),
        "ordered-pair".to_string(),
        vec![first.id.clone(), second.id.clone()],
    )?
    .result
    .expect("ordered Persona group missing");
    let first_before = mom_llama_runtime::persona_get(&first.id)?
        .result
        .expect("first Persona snapshot missing");
    let second_before = mom_llama_runtime::persona_get(&second.id)?
        .result
        .expect("second Persona snapshot missing");
    let host = mom_llama_runtime::conversation_new(Some("Host".to_string()))?
        .result
        .expect("mention host missing");
    let result = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: host.id.clone(),
            message: format!("@{} compare this", group.mention_handle),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    assert_eq!(result.readiness, "fake_fixture_exercised");
    assert!(!result.receipt.real_engine_invoked);
    assert!(result.receipt.fake_fixture);
    let ChatDispatchOutput::Mention { invocation, .. } =
        result.result.expect("mention dispatch output missing")
    else {
        return Err(anyhow!("expected mention dispatch"));
    };
    assert_eq!(
        invocation
            .targets
            .iter()
            .map(|target| target.target_id.as_str())
            .collect::<Vec<_>>(),
        vec![first.id.as_str(), second.id.as_str()]
    );
    assert!(invocation.targets.iter().all(|target| target.version == 1));
    assert_eq!(
        mom_llama_runtime::persona_get(&first.id)?.result,
        Some(first_before.clone())
    );
    assert_eq!(
        mom_llama_runtime::persona_get(&second.id)?.result,
        Some(second_before)
    );
    let host = mom_llama_runtime::conversation_select(&host.id)?
        .result
        .expect("mention host reload missing");
    let attributed = host
        .messages
        .iter()
        .filter_map(|message| message.attribution.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(attributed.len(), 2);
    assert_eq!(attributed[0].handle, "first-lens");
    assert_eq!(attributed[0].target_order, 0);
    assert_eq!(attributed[1].handle, "second-lens");
    assert_eq!(attributed[1].target_order, 1);
    assert_eq!(attributed[0].invocation_id, attributed[1].invocation_id);
    assert_eq!(attributed[0].version, 1);
    let fixture_synthesis = mom_llama_runtime::mention_synthesize(&scope, &invocation.id)?;
    assert_eq!(fixture_synthesis.status, "blocked");
    assert_eq!(
        fixture_synthesis
            .blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("mention_synthesis_sources_incomplete")
    );
    assert!(!fixture_synthesis.receipt.real_engine_invoked);
    let editable = first_before
        .active_leaf_message_id
        .clone()
        .ok_or_else(|| anyhow!("persona edit target missing"))?;
    mom_llama_runtime::message_edit(&first.id, &editable, "A revised frozen branch".to_string())?;
    let revised = mom_llama_runtime::persona_get(&first.id)?
        .result
        .expect("revised Persona missing");
    assert_eq!(revised.execution_profile.version, 2);
    let versions = mom_llama_runtime::persona_versions(&first.id)?;
    assert_eq!(versions.len(), 2);
    assert_ne!(
        versions[0].conversation_sha256,
        versions[1].conversation_sha256
    );
    assert_eq!(
        invocation.targets[0].version, 1,
        "an invocation remains pinned to the persona version captured at dispatch"
    );
    Ok(())
}

#[test]
fn live_chat_mentions_capture_the_committed_leaf_without_writeback() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("live-chat-mention")?;
    let source = mom_llama_runtime::conversation_new(Some("Research notes".to_string()))?
        .result
        .expect("live-chat source missing");
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "Committed source context".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source_before = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .expect("live-chat source reload missing");
    let handle = source_before.execution_profile.mention_handle.clone();
    let leaf = source_before.active_leaf_message_id.clone();
    let host = mom_llama_runtime::conversation_new(Some("Host".to_string()))?
        .result
        .expect("live-chat mention host missing");
    let result = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: host.id.clone(),
            message: format!("@{handle} answer from your notes"),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let ChatDispatchOutput::Mention { invocation, .. } =
        result.result.expect("live-chat mention output missing")
    else {
        return Err(anyhow!("expected a live-chat mention"));
    };
    assert_eq!(invocation.targets.len(), 1);
    assert_eq!(invocation.targets[0].source_leaf_message_id, leaf);
    assert_eq!(
        invocation.targets[0].source_messages,
        mom_llama_runtime::conversation_store::active_path_messages(&source_before)?
    );
    assert_eq!(
        mom_llama_runtime::conversation_select(&source.id)?.result,
        Some(source_before.clone())
    );
    let host = mom_llama_runtime::conversation_select(&host.id)?
        .result
        .expect("live-chat host reload missing");
    let attribution = host
        .messages
        .iter()
        .find_map(|message| message.attribution.as_ref())
        .expect("live-chat attribution missing");
    assert_eq!(
        attribution.kind,
        mom_llama_runtime::MessageSpeakerKind::LiveChat
    );
    assert_eq!(attribution.source_id, source.id);

    let follow_up = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: host.id.clone(),
            message: "Continue as the host chat without inviting anyone.".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let ChatDispatchOutput::Direct {
        conversation_id, ..
    } = follow_up.result.expect("direct follow-up output missing")
    else {
        return Err(anyhow!(
            "a follow-up without an @handle must use the host chat"
        ));
    };
    assert_eq!(conversation_id, host.id);
    let host_after = mom_llama_runtime::conversation_select(&host.id)?
        .result
        .expect("host follow-up reload missing");
    let direct_answer = host_after
        .messages
        .last()
        .expect("direct host answer missing");
    assert_eq!(
        direct_answer.role,
        mom_llama_runtime::MessageRole::Assistant
    );
    assert!(direct_answer.attribution.is_none());
    assert!(!direct_answer.content.starts_with("Response from @"));
    assert_eq!(
        mom_llama_runtime::conversation_select(&source.id)?.result,
        Some(source_before),
        "an ordinary host follow-up must never be sent or written back to the mentioned source"
    );
    Ok(())
}

#[test]
fn duplicate_persona_handles_and_oversized_groups_fail_closed() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("persona-handle-safety")?;
    let source = mom_llama_runtime::conversation_new(Some("Source".to_string()))?
        .result
        .expect("handle-safety source missing");
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "seed".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .expect("handle-safety source reload missing");
    let leaf = source
        .active_leaf_message_id
        .clone()
        .expect("handle-safety source leaf missing");
    let first = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: leaf.clone(),
        name: "One".to_string(),
        mention_handle: "unique-lens".to_string(),
        history_mode: PersonaHistoryMode::Empty,
    })?;
    assert!(first.result.is_some());
    let duplicate = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: leaf.clone(),
        name: "Two".to_string(),
        mention_handle: "UNIQUE-LENS".to_string(),
        history_mode: PersonaHistoryMode::Empty,
    })?;
    assert_eq!(duplicate.status, "blocked");
    assert_eq!(
        duplicate
            .blocker
            .expect("duplicate handle blocker missing")
            .code,
        "mention_handle_taken"
    );
    let mut persona_ids = Vec::new();
    for index in 0..5 {
        let persona = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
            conversation_id: source.id.clone(),
            message_id: leaf.clone(),
            name: format!("Lens {index}"),
            mention_handle: format!("lens-{index}"),
            history_mode: PersonaHistoryMode::Empty,
        })?
        .result
        .expect("bounded group Persona missing");
        persona_ids.push(persona.id);
    }
    let oversized = mom_llama_runtime::persona_group_create(
        "Too many".to_string(),
        "too-many".to_string(),
        persona_ids,
    )?;
    assert_eq!(oversized.status, "blocked");
    assert_eq!(
        oversized
            .blocker
            .expect("oversized group blocker missing")
            .code,
        "persona_group_size_invalid"
    );
    let host = mom_llama_runtime::conversation_new(Some("Host".to_string()))?
        .result
        .expect("missing-target host missing");
    let before = mom_llama_runtime::conversation_select(&host.id)?
        .result
        .expect("missing-target host snapshot missing");
    let missing = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: host.id.clone(),
            message: "@deleted-persona please answer".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    assert_eq!(missing.status, "blocked");
    assert_eq!(
        missing
            .blocker
            .expect("missing-target blocker missing")
            .code,
        "mention_target_not_found"
    );
    assert_eq!(
        mom_llama_runtime::conversation_select(&host.id)?.result,
        Some(before)
    );
    Ok(())
}

#[test]
fn conversation_import_validates_or_safely_assigns_mention_handles() -> Result<()> {
    let _session = TestSession::new("conversation-import-handle-validation")?;
    let source = mom_llama_runtime::conversation_new(Some("Existing handle owner".to_string()))?
        .result
        .ok_or_else(|| anyhow!("source conversation missing"))?;

    let mut duplicate = source.clone();
    duplicate.id = "imported-duplicate".to_string();
    duplicate.title = "Imported duplicate".to_string();
    duplicate.messages.clear();
    duplicate.active_leaf_message_id = None;
    let blocked = mom_llama_runtime::conversation_import_json(&serde_json::to_string(&duplicate)?)?;
    assert_eq!(blocked.status, "blocked");
    assert_eq!(
        blocked
            .blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("mention_handle_taken")
    );

    let mut invalid = duplicate.clone();
    invalid.id = "imported-invalid".to_string();
    invalid.execution_profile.mention_handle = "bad handle!".to_string();
    let blocked = mom_llama_runtime::conversation_import_json(&serde_json::to_string(&invalid)?)?;
    assert_eq!(blocked.status, "blocked");
    assert_eq!(
        blocked
            .blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("mention_handle_invalid")
    );

    let mut legacy = duplicate;
    legacy.id = "imported-legacy".to_string();
    legacy.title = "Imported Legacy Chat".to_string();
    legacy.execution_profile.mention_handle.clear();
    let imported = mom_llama_runtime::conversation_import_json(&serde_json::to_string(&legacy)?)?
        .result
        .ok_or_else(|| anyhow!("legacy import missing"))?;
    assert_eq!(
        imported.execution_profile.mention_handle,
        "imported-legacy-chat"
    );
    Ok(())
}

#[test]
fn adjacent_conversation_file_is_not_imported_or_rewritten() -> Result<()> {
    let session = TestSession::new("adjacent-conversation-file")?;
    let path = session.path().join("conversations.json");
    fs::write(&path, b"private adjacent conversation")?;
    assert!(
        mom_llama_runtime::conversation_list()?
            .result
            .context("conversation list")?
            .is_empty()
    );
    let created = mom_llama_runtime::conversation_new(Some("Current store".to_string()))?
        .result
        .context("created conversation")?;
    let reopened = mom_llama_runtime::conversation_list()?
        .result
        .context("reopened list")?;
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].id, created.id);
    assert_eq!(fs::read(path)?, b"private adjacent conversation");
    Ok(())
}

#[test]
fn blocked_chat_stream_never_claims_native_engine_invocation() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("blocked-stream-evidence")?;
    let mut events = Vec::new();
    let result = mom_llama_runtime::chat_send_stream_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: "blocked-stream-evidence".to_string(),
            message: "Hello".to_string(),
        },
        ChatSendOptions::default(),
        |event| {
            events.push(event);
            Ok(())
        },
    )?;
    assert_eq!(result.status, "blocked");
    assert!(
        matches!(
            result.readiness.as_str(),
            "blocked_missing_model" | "blocked_native_runtime"
        ),
        "the configured host must remain an explicit native blocker"
    );
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "started");
    assert_eq!(events[1].event, "failed");
    assert_eq!(events[0].request_id, events[1].request_id);
    for event in &events {
        assert!(!event.real_engine_invoked);
        assert!(!event.fake_fixture);
    }
    assert!(!result.receipt.real_engine_invoked);
    Ok(())
}

#[test]
fn skip_reasoning_without_an_active_request_is_typed() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("skip-reasoning-no-active")?;
    let result = mom_llama_runtime::chat_skip_reasoning_in_scope(&scope, "conversation")?;
    assert_eq!(result.status, "blocked");
    assert_eq!(
        result.blocker.as_ref().map(|blocker| blocker.code.as_str()),
        Some("no_active_reasoning_request")
    );
    assert!(!result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    Ok(())
}

#[cfg(unix)]
#[test]
fn tool_loop_without_model_is_blocked_and_never_claims_engine_execution() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("tool-loop-missing-model")?;
    configure_mcp_fixture(&session)?;
    let prepared = mom_llama_runtime::tool_loop_prepare_in_scope(
        &scope,
        "tool-loop-missing-model",
        "Check the configured tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({}),
        2,
    )?;
    let approval_id = prepared
        .result
        .map(|approval| approval.id)
        .ok_or_else(|| anyhow!("tool loop approval missing"))?;
    let result = mom_llama_runtime::tool_loop_run_in_scope(
        &scope,
        "tool-loop-missing-model",
        "Check the configured tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({}),
        2,
        Some(approval_id),
    )?;
    assert_eq!(result.readiness, "blocked_missing_model");
    assert_eq!(
        result.blocker.as_ref().map(|blocker| blocker.code.as_str()),
        Some("model_path_missing")
    );
    assert!(!result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    Ok(())
}

#[cfg(unix)]
#[test]
fn tool_loop_requires_an_exact_expiring_single_use_approval() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("tool-loop-approval")?;
    configure_mcp_fixture(&session)?;
    let without_approval = mom_llama_runtime::tool_loop_run_in_scope(
        &scope,
        "tool-loop-approval",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
        None,
    )?;
    assert_eq!(
        without_approval
            .blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("tool_loop_approval_required")
    );

    let prepared = mom_llama_runtime::tool_loop_prepare_in_scope(
        &scope,
        "tool-loop-approval",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
    )?
    .result
    .ok_or_else(|| anyhow!("tool loop approval missing"))?;
    let mismatch = mom_llama_runtime::tool_loop_run_in_scope(
        &scope,
        "tool-loop-approval",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"different"}),
        2,
        Some(prepared.id.clone()),
    )?;
    assert_eq!(
        mismatch
            .blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("tool_loop_approval_mismatch")
    );
    let approved_attempt = mom_llama_runtime::tool_loop_run_in_scope(
        &scope,
        "tool-loop-approval",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
        Some(prepared.id.clone()),
    )?;
    assert_eq!(approved_attempt.readiness, "blocked_missing_model");
    let reused = mom_llama_runtime::tool_loop_run_in_scope(
        &scope,
        "tool-loop-approval",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
        Some(prepared.id),
    )?;
    assert_eq!(
        reused.blocker.as_ref().map(|blocker| blocker.code.as_str()),
        Some("tool_loop_approval_consumed")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn persistent_tool_permissions_support_ask_allow_deny_and_revoke() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("tool-permissions")?;
    configure_mcp_fixture(&session)?;

    mom_llama_runtime::tool_permission_set(
        "fixture".to_string(),
        "echo".to_string(),
        mom_llama_runtime::ToolPermissionPolicy::Deny,
    )?;
    let denied = mom_llama_runtime::tool_loop_prepare_in_scope(
        &scope,
        "tool-permissions",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
    )?;
    assert_eq!(
        denied.blocker.as_ref().map(|blocker| blocker.code.as_str()),
        Some("tool_permission_denied")
    );

    mom_llama_runtime::tool_permission_set(
        "fixture".to_string(),
        "echo".to_string(),
        mom_llama_runtime::ToolPermissionPolicy::AlwaysAllow,
    )?;
    let always_allowed = mom_llama_runtime::tool_loop_prepare_in_scope(
        &scope,
        "tool-permissions",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
    )?
    .result
    .ok_or_else(|| anyhow!("always-allow approval missing"))?;
    assert!(!always_allowed.requires_confirmation);
    let without_prompt = mom_llama_runtime::tool_loop_run_in_scope(
        &scope,
        "tool-permissions",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
        None,
    )?;
    assert_eq!(without_prompt.readiness, "blocked_missing_model");

    mom_llama_runtime::tool_permission_revoke("fixture", "echo")?;
    let ask_again = mom_llama_runtime::tool_loop_prepare_in_scope(
        &scope,
        "tool-permissions",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
    )?
    .result
    .ok_or_else(|| anyhow!("ask approval missing"))?;
    assert!(ask_again.requires_confirmation);
    assert!(
        mom_llama_runtime::tool_permission_list()?
            .result
            .unwrap_or_default()
            .is_empty()
    );
    Ok(())
}

#[test]
fn fixture_readiness_never_claims_native_inference() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("fixture-readiness")?;
    let result =
        mom_llama_runtime::engine_check(&scope, EngineCheckOptions { fake_fixture: true })?;
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
fn product_runtime_rejects_network_process_and_copied_native_authority() -> Result<()> {
    let runtime_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let product_root = runtime_manifest
        .ancestors()
        .nth(2)
        .ok_or_else(|| anyhow!("runtime crate has no product root"))?;
    let repo_root = runtime_manifest
        .ancestors()
        .nth(4)
        .ok_or_else(|| anyhow!("runtime crate has no monorepo root"))?;
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
    for crate_name in [
        "llama-native-types",
        "llama-native-engine",
        "llama-native-cache",
        "llama-native-host",
        "attachment-native-types",
        "attachment-native-inspect",
        "attachment-native-document",
        "attachment-native-plan",
        "attachment-native-host",
    ] {
        assert!(
            !product_root.join("crates").join(crate_name).exists(),
            "{crate_name} must not be copied into the Mom product boundary"
        );
    }
    let workspace_manifest = fs::read_to_string(repo_root.join("Cargo.toml"))?;
    for local_dependency in [
        "llama-native-cache = { path = \"crates/native/crates/llama-native-cache\" }",
        "llama-native-engine = { path = \"crates/native/crates/llama-native-engine\" }",
        "llama-native-host = { path = \"crates/native/crates/llama-native-host\" }",
        "llama-native-types = { path = \"crates/native/crates/llama-native-types\" }",
        "attachment-native-host = { path = \"crates/services/attachment/crates/attachment-native-host\" }",
        "attachment-native-types = { path = \"crates/services/attachment/crates/attachment-native-types\" }",
        "fte-router = { path = \"products/fte/crates/fte-router\" }",
        "fte-store = { path = \"products/fte/crates/fte-store\" }",
        "fte-types = { path = \"products/fte/crates/fte-types\" }",
    ] {
        assert!(
            workspace_manifest.contains(local_dependency),
            "root workspace must provide imported dependency `{local_dependency}`"
        );
    }
    let runtime_workspace_manifest = fs::read_to_string(runtime_manifest.join("Cargo.toml"))?;
    let app_manifest = fs::read_to_string(
        product_root
            .join("apps")
            .join("mom-llama")
            .join("src-tauri")
            .join("Cargo.toml"),
    )?;
    for retired_composition in ["fte-", "tauri-plugin-free-token-energy"] {
        assert!(
            !runtime_workspace_manifest.contains(retired_composition)
                && !app_manifest.contains(retired_composition),
            "Mom must not compose the retired FTE dependency `{retired_composition}`"
        );
    }
    for retired_source in [
        "github.com/delysis/mom-llama",
        "github.com/delysis/llama-native-kit",
        "github.com/delysis/free-token-energy",
        "github.com/delysis/attachment-native-kit",
    ] {
        assert!(
            !runtime_workspace_manifest.contains(retired_source),
            "Mom runtime manifest retains retired Git source `{retired_source}`"
        );
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
    let sampling = settings.sampling_config()?;
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
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("fixture-chat")?;
    let result = mom_llama_runtime::chat_send_in_scope(
        &scope,
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
fn live_chat_controls_select_latest_owned_request_and_preserve_author_state() -> Result<()> {
    let session = TestSession::new("live-chat-controls")?;
    let scope = mom_llama_runtime::OperationScope::detached();
    let foreign = mom_llama_runtime::OperationScope::detached();
    let conversation_id = "live-chat-controls";
    let options = ChatSendOptions {
        timeout_s: 1.0,
        fake_fixture: true,
    };
    let ordinary = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: conversation_id.to_string(),
            message: "Retain this author exchange.".to_string(),
        },
        options,
    )?;
    assert_eq!(ordinary.readiness, "fake_fixture_exercised");
    assert!(ordinary.receipt.fake_fixture);
    assert!(!ordinary.receipt.real_engine_invoked);
    let output = ordinary.result.as_ref().context("ordinary chat output")?;
    let author = mom_llama_runtime::conversation_select(conversation_id)?
        .result
        .context("ordinary author conversation")?;
    assert_eq!(author.messages.len(), 2);
    assert_eq!(author.messages[0].id, output.user_message_id);
    assert_eq!(author.messages[1].id, output.assistant_message_id);
    assert_eq!(author.messages[1].content, output.assistant_text);
    assert_eq!(scope.active_operation_count(), 0);
    let completed_cancel = mom_llama_runtime::chat_cancel_in_scope(&scope, conversation_id)?;
    assert_eq!(
        completed_cancel
            .blocker
            .context("completed chat blocker")?
            .code,
        "no_active_chat_request"
    );
    let completed_skip = mom_llama_runtime::chat_skip_reasoning_in_scope(&scope, conversation_id)?;
    assert_eq!(
        completed_skip
            .blocker
            .context("completed reasoning blocker")?
            .code,
        "no_active_reasoning_request"
    );
    mom_llama_runtime::persist_command_receipt(&ordinary)?;
    let receipt_bytes = || -> Result<(String, Vec<u8>, Vec<u8>, i64)> {
        Ok(rusqlite::Connection::open(session.path().join("runtime.sqlite3"))?.query_row(
            "SELECT command_id, nonce, ciphertext, created_at FROM receipts WHERE receipt_id = ?1",
            [&ordinary.receipt.task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?)
    };
    let retained_receipt = receipt_bytes()?;
    assert_eq!(retained_receipt.0, "mom_llama.chat_send");
    let mut author_rows = None;
    let mut outer_events = Vec::new();
    let outer = mom_llama_runtime::chat_send_stream_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: conversation_id.to_string(),
            message: "Cancel the older concurrent request.".to_string(),
        },
        options,
        |event| {
            if event.event == "started" {
                let mut inner_events = Vec::new();
                let inner = mom_llama_runtime::chat_send_stream_in_scope(
                    &scope,
                    ChatSendInput {
                        conversation_id: conversation_id.to_string(),
                        message: "Cancel the latest concurrent request first.".to_string(),
                    },
                    options,
                    |inner_event| {
                        if inner_event.event == "started" {
                            assert_ne!(event.request_id, inner_event.request_id);
                            assert_eq!(scope.active_operation_count(), 2);
                            let mut rows = encrypted_document_snapshot(session.path())?;
                            rows.remove("active-requests.v2");
                            author_rows = Some(rows);
                            let foreign_cancel =
                                mom_llama_runtime::chat_cancel_in_scope(&foreign, conversation_id)?;
                            assert_eq!(
                                foreign_cancel
                                    .blocker
                                    .context("foreign cancel blocker")?
                                    .code,
                                "no_active_chat_request"
                            );
                            let foreign_skip = mom_llama_runtime::chat_skip_reasoning_in_scope(
                                &foreign,
                                conversation_id,
                            )?;
                            assert_eq!(
                                foreign_skip.blocker.context("foreign skip blocker")?.code,
                                "no_active_reasoning_request"
                            );
                            let skip = mom_llama_runtime::chat_skip_reasoning_in_scope(
                                &scope,
                                conversation_id,
                            )?;
                            assert_eq!(
                                skip.blocker
                                    .context("fixture has no native reasoning")?
                                    .code,
                                "reasoning_request_not_resident"
                            );
                            let cancel =
                                mom_llama_runtime::chat_cancel_in_scope(&scope, conversation_id)?;
                            assert_eq!(cancel.status, "contracted");
                            assert!(!cancel.receipt.real_engine_invoked);
                            let cancelled = cancel.result.context("latest cancellation")?;
                            assert_eq!(cancelled.request_id, inner_event.request_id);
                            assert_eq!(cancelled.conversation_id, conversation_id);
                            assert_eq!(
                                cancelled.cancel_path,
                                format!("native://request/{}", inner_event.request_id)
                            );
                            let repeat =
                                mom_llama_runtime::chat_cancel_in_scope(&scope, conversation_id)?;
                            assert_eq!(
                                repeat.blocker.context("already cancelled blocker")?.code,
                                "chat_request_not_resident"
                            );
                        }
                        inner_events.push(inner_event);
                        Ok(())
                    },
                )?;
                assert!(inner.has_authoritative_cancellation_evidence());
                assert_eq!(inner_events.len(), 2);
                assert_eq!(inner_events[0].event, "started");
                assert_eq!(inner_events[1].event, "cancelled");
                assert_eq!(inner_events[0].request_id, inner_events[1].request_id);
                assert_eq!(scope.active_operation_count(), 1);
                let cancel = mom_llama_runtime::chat_cancel_in_scope(&scope, conversation_id)?;
                assert_eq!(
                    cancel.result.context("older live cancellation")?.request_id,
                    event.request_id
                );
                let skip =
                    mom_llama_runtime::chat_skip_reasoning_in_scope(&scope, conversation_id)?;
                assert_eq!(
                    skip.blocker.context("no uncancelled request remains")?.code,
                    "no_active_reasoning_request"
                );
                for inner_event in inner_events {
                    assert!(inner_event.fake_fixture);
                    assert!(!inner_event.real_engine_invoked);
                }
            }
            outer_events.push(event);
            Ok(())
        },
    )?;
    assert!(outer.has_authoritative_cancellation_evidence());
    assert!(outer.receipt.fake_fixture);
    assert!(!outer.receipt.real_engine_invoked);
    assert_eq!(outer_events.len(), 2);
    assert_eq!(outer_events[0].event, "started");
    assert_eq!(outer_events[1].event, "cancelled");
    assert_eq!(outer_events[0].request_id, outer_events[1].request_id);
    assert_eq!(scope.active_operation_count(), 0);
    let mut rows = encrypted_document_snapshot(session.path())?;
    rows.remove("active-requests.v2");
    assert_eq!(
        rows,
        author_rows.context("author rows captured before cancellation")?
    );
    assert_eq!(receipt_bytes()?, retained_receipt);
    for owned_scope in [&scope, &foreign] {
        let cancel = mom_llama_runtime::chat_cancel_in_scope(owned_scope, conversation_id)?;
        assert_eq!(
            cancel.blocker.context("completed scope blocker")?.code,
            "no_active_chat_request"
        );
    }
    let mut interrupted_events = Vec::new();
    let interrupted = mom_llama_runtime::chat_send_stream_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: conversation_id.to_string(),
            message: "Reject the started callback.".to_string(),
        },
        options,
        |event| {
            let is_started = event.event == "started";
            interrupted_events.push(event);
            if is_started {
                Err(anyhow!("owned callback rejection"))
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(
        interrupted.expect_err("callback rejection").to_string(),
        "owned callback rejection"
    );
    assert_eq!(interrupted_events.len(), 2);
    assert_eq!(interrupted_events[0].event, "started");
    assert_eq!(interrupted_events[1].event, "failed");
    assert_eq!(
        interrupted_events[0].request_id,
        interrupted_events[1].request_id
    );
    assert!(
        interrupted_events
            .iter()
            .all(|event| !event.real_engine_invoked)
    );
    assert_eq!(scope.active_operation_count(), 0);
    assert_eq!(
        serde_json::to_value(mom_llama_runtime::conversation_select(conversation_id)?.result)?,
        serde_json::to_value(Some(author))?
    );
    assert_eq!(receipt_bytes()?, retained_receipt);
    Ok(())
}

#[test]
fn message_tree_preserves_siblings_and_switches_the_active_leaf() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("message-tree")?;
    let options = ChatSendOptions {
        timeout_s: 1.0,
        fake_fixture: true,
    };
    let first = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: "message-tree".to_string(),
            message: "Give me one answer.".to_string(),
        },
        options,
    )?
    .result
    .ok_or_else(|| anyhow!("first fixture output missing"))?;
    let second = mom_llama_runtime::chat_regenerate_in_scope(&scope, "message-tree", options)?
        .result
        .ok_or_else(|| anyhow!("regenerated fixture output missing"))?;
    assert_eq!(first.user_message_id, second.user_message_id);
    assert_ne!(first.assistant_message_id, second.assistant_message_id);

    let selected = mom_llama_runtime::conversation_select("message-tree")?
        .result
        .ok_or_else(|| anyhow!("projected conversation missing"))?;
    assert_eq!(selected.messages.len(), 2);
    let active_assistant = selected
        .messages
        .last()
        .ok_or_else(|| anyhow!("active assistant missing"))?;
    assert_eq!(active_assistant.id, second.assistant_message_id);
    assert_eq!(active_assistant.branch_index, Some(2));
    assert_eq!(active_assistant.branch_count, Some(2));

    let branches =
        mom_llama_runtime::message_branches("message-tree", &second.assistant_message_id)?
            .result
            .ok_or_else(|| anyhow!("message branches missing"))?;
    assert_eq!(branches.siblings.len(), 2);
    assert_eq!(
        branches
            .siblings
            .iter()
            .filter(|sibling| sibling.selected)
            .count(),
        1
    );

    mom_llama_runtime::message_branch_select("message-tree", &first.assistant_message_id)?;
    let switched = mom_llama_runtime::conversation_select("message-tree")?
        .result
        .ok_or_else(|| anyhow!("switched conversation missing"))?;
    let switched_assistant = switched
        .messages
        .last()
        .ok_or_else(|| anyhow!("switched assistant missing"))?;
    assert_eq!(switched_assistant.id, first.assistant_message_id);
    assert_eq!(switched_assistant.branch_index, Some(1));
    assert_eq!(switched_assistant.branch_count, Some(2));

    let stored = mom_llama_runtime::conversation_list()?
        .result
        .and_then(|conversations| {
            conversations
                .into_iter()
                .find(|conversation| conversation.id == "message-tree")
        })
        .ok_or_else(|| anyhow!("stored message tree missing"))?;
    assert_eq!(stored.messages.len(), 3);
    Ok(())
}

#[test]
fn user_and_assistant_edits_preserve_original_message_branches() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("message-edit-branches")?;
    let output = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: "message-edit-branches".to_string(),
            message: "Original user request".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?
    .result
    .ok_or_else(|| anyhow!("fixture output missing"))?;
    let edited_assistant = mom_llama_runtime::message_edit(
        "message-edit-branches",
        &output.assistant_message_id,
        "Edited assistant answer".to_string(),
    )?
    .result
    .ok_or_else(|| anyhow!("assistant edit missing"))?;
    let assistant_branches =
        mom_llama_runtime::message_branches("message-edit-branches", &edited_assistant.id)?
            .result
            .ok_or_else(|| anyhow!("assistant branches missing"))?;
    assert_eq!(assistant_branches.siblings.len(), 2);

    mom_llama_runtime::message_branch_select(
        "message-edit-branches",
        &output.assistant_message_id,
    )?;
    let edited_user = mom_llama_runtime::message_edit(
        "message-edit-branches",
        &output.user_message_id,
        "Edited user request".to_string(),
    )?
    .result
    .ok_or_else(|| anyhow!("user edit missing"))?;
    let user_branches =
        mom_llama_runtime::message_branches("message-edit-branches", &edited_user.id)?
            .result
            .ok_or_else(|| anyhow!("user branches missing"))?;
    assert_eq!(user_branches.siblings.len(), 2);
    let projected = mom_llama_runtime::conversation_select("message-edit-branches")?
        .result
        .ok_or_else(|| anyhow!("projected edit branch missing"))?;
    assert_eq!(projected.messages.len(), 1);
    assert_eq!(projected.messages[0].id, edited_user.id);
    let stored = mom_llama_runtime::conversation_list()?
        .result
        .and_then(|conversations| {
            conversations
                .into_iter()
                .find(|conversation| conversation.id == "message-edit-branches")
        })
        .ok_or_else(|| anyhow!("stored edit tree missing"))?;
    assert_eq!(stored.messages.len(), 4);
    Ok(())
}

#[test]
fn editing_one_mention_result_preserves_later_peers_on_the_edited_branch() -> Result<()> {
    let _session = TestSession::new("mention-result-edit-branches")?;
    let fixture = attributed_history_fixture("mention-result-edit-branches");
    mom_llama_runtime::conversation_import_json(&serde_json::to_string(&fixture)?)?;

    let edited = mom_llama_runtime::message_edit(
        &fixture.id,
        "2-first-peer",
        "Revised first perspective".to_string(),
    )?
    .result
    .ok_or_else(|| anyhow!("edited mention result missing"))?;
    let edited_branch = mom_llama_runtime::conversation_select(&fixture.id)?
        .result
        .ok_or_else(|| anyhow!("edited mention branch missing"))?;
    assert_eq!(
        edited_branch
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Ask both",
            "Revised first perspective",
            "Second perspective"
        ]
    );
    assert_eq!(edited_branch.messages[1].id, edited.id);
    let cloned_peer = &edited_branch.messages[2];
    assert_ne!(cloned_peer.id, "3-second-peer");
    assert_eq!(cloned_peer.parent_id.as_deref(), Some(edited.id.as_str()));
    assert_eq!(
        cloned_peer
            .attribution
            .as_ref()
            .map(|attribution| (attribution.invocation_id.as_str(), attribution.target_order)),
        Some(("shared-invocation", 1))
    );

    let branches = mom_llama_runtime::message_branches(&fixture.id, &edited.id)?
        .result
        .ok_or_else(|| anyhow!("edited mention branches missing"))?;
    assert_eq!(branches.siblings.len(), 2);
    let original_branch = mom_llama_runtime::message_branch_select(&fixture.id, "2-first-peer")?
        .result
        .ok_or_else(|| anyhow!("original mention branch missing"))?;
    assert_eq!(
        original_branch
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Ask both",
            "First perspective",
            "Second perspective",
            "Follow up after both",
            "Host answer after both"
        ]
    );
    Ok(())
}

#[test]
fn deleting_one_mention_result_splices_later_peers_and_host_history() -> Result<()> {
    let _session = TestSession::new("mention-result-delete-splice")?;
    let fixture = attributed_history_fixture("mention-result-delete-splice");
    mom_llama_runtime::conversation_import_json(&serde_json::to_string(&fixture)?)?;

    let deleted = mom_llama_runtime::message_delete(&fixture.id, "2-first-peer")?;
    assert_eq!(deleted.readiness, "contracted");
    let projected = mom_llama_runtime::conversation_select(&fixture.id)?
        .result
        .ok_or_else(|| anyhow!("mention history missing after delete"))?;
    assert_eq!(
        projected
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Ask both",
            "Second perspective",
            "Follow up after both",
            "Host answer after both"
        ]
    );
    assert_eq!(projected.messages[1].parent_id.as_deref(), Some("1-user"));
    assert_eq!(
        projected.active_leaf_message_id.as_deref(),
        Some("5-host-answer")
    );
    assert!(
        projected
            .messages
            .iter()
            .all(|message| message.id != "2-first-peer")
    );
    Ok(())
}

#[test]
fn system_and_tool_messages_reject_edits_without_mutating_the_conversation() -> Result<()> {
    let _session = TestSession::new("message-edit-role-guards")?;
    let system = Message {
        id: "system-message".to_string(),
        conversation_id: "message-edit-role-guards".to_string(),
        role: MessageRole::System,
        content: "Stable system policy".to_string(),
        created_at: "1".to_string(),
        parent_id: None,
        model: None,
        receipt_id: None,
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: None,
        attachment_ids: Vec::new(),
    };
    let tool = Message {
        id: "tool-message".to_string(),
        conversation_id: "message-edit-role-guards".to_string(),
        role: MessageRole::Tool,
        content: "Stable tool result".to_string(),
        created_at: "2".to_string(),
        parent_id: Some(system.id.clone()),
        ..system.clone()
    };
    let fixture = Conversation {
        id: "message-edit-role-guards".to_string(),
        title: "Role guards".to_string(),
        created_at: "1".to_string(),
        updated_at: "2".to_string(),
        kind: ConversationKind::Chat,
        execution_profile: ConversationExecutionProfile::default(),
        selected_model_path: None,
        source_conversation_id: None,
        source_message_id: None,
        branch_root_message_id: None,
        active_leaf_message_id: Some(tool.id.clone()),
        recipient_ids: Vec::new(),
        current_skill_ids: Vec::new(),
        messages: vec![system.clone(), tool.clone()],
        inference_window: None,
    };
    mom_llama_runtime::conversation_import_json(&serde_json::to_string(&fixture)?)?;
    let before = mom_llama_runtime::conversation_select(&fixture.id)?
        .result
        .ok_or_else(|| anyhow!("role-guard fixture missing before edits"))?;

    for message in [&system, &tool] {
        let blocked = mom_llama_runtime::message_edit(
            &fixture.id,
            &message.id,
            "Attempted rewrite".to_string(),
        )?;
        assert_eq!(blocked.status, "blocked");
        assert_eq!(blocked.readiness, "stub_blocked");
        assert_eq!(
            blocked
                .blocker
                .as_ref()
                .map(|blocker| blocker.code.as_str()),
            Some("message_role_not_editable")
        );
        assert!(blocked.result.is_none());
        assert!(blocked.receipt.changed_paths.is_empty());
    }

    let stored = mom_llama_runtime::conversation_select(&fixture.id)?
        .result
        .ok_or_else(|| anyhow!("role-guard fixture missing"))?;
    assert_eq!(stored, before);
    Ok(())
}

#[test]
fn chat_dispatch_ignores_email_embedded_and_code_at_tokens_but_blocks_explicit_unknowns()
-> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("mention-token-boundaries")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Mention boundaries".to_string()))?
        .result
        .ok_or_else(|| anyhow!("mention-boundary conversation missing"))?;
    let literal_message = "Email george@example.com; keep prefix@embedded, `@inline-code`, and:\n```text\n@fenced-code\n```\n    @indented-code";
    let direct = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: conversation.id.clone(),
            message: literal_message.to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    assert!(matches!(
        direct.result,
        Some(ChatDispatchOutput::Direct { .. })
    ));
    let before_unknown = mom_llama_runtime::conversation_select(&conversation.id)?
        .result
        .ok_or_else(|| anyhow!("mention-boundary transcript missing"))?;

    for message in [
        "@missing-person please answer",
        "Ask @missing-person please",
    ] {
        let blocked = mom_llama_runtime::chat_dispatch_in_scope(
            &scope,
            MentionDispatchInput {
                conversation_id: conversation.id.clone(),
                message: message.to_string(),
            },
            ChatSendOptions {
                fake_fixture: true,
                ..ChatSendOptions::default()
            },
        )?;
        assert_eq!(blocked.status, "blocked");
        assert_eq!(
            blocked
                .blocker
                .as_ref()
                .map(|blocker| blocker.code.as_str()),
            Some("mention_target_not_found")
        );
    }
    assert_eq!(
        mom_llama_runtime::conversation_select(&conversation.id)?.result,
        Some(before_unknown),
        "an unresolved explicit mention must not append a host message"
    );
    Ok(())
}

#[test]
fn conversations_search_skills_and_settings_survive_restart() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("persistence")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Garden planning".to_string()))?
        .result
        .ok_or_else(|| anyhow!("conversation was not created"))?;
    mom_llama_runtime::chat_send_in_scope(
        &scope,
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
    let settings_update = mom_llama_runtime::settings_update(SettingsUpdate {
        temperature: Some(0.25),
        context_tokens: Some(4096),
        max_parallel_sequences: Some(4),
        ..SettingsUpdate::default()
    })?;
    let effective_settings = settings_update.result.ok_or_else(|| {
        anyhow!(
            "voice latency benchmark settings were blocked: {:#?}",
            settings_update.blocker
        )
    })?;
    anyhow::ensure!(
        effective_settings.max_parallel_sequences == 4,
        "voice latency benchmark requested four sequences but resolved {}",
        effective_settings.max_parallel_sequences
    );

    set_data_dir_override_for_tests(None);
    mom_llama_runtime::unload_resident_model(&scope);
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
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("attachment")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Photo".to_string()))?
        .result
        .ok_or_else(|| anyhow!("conversation was not created"))?;
    let image = session.path().join("garden.png");
    let payload = VALID_PNG;
    std::fs::write(&image, payload)?;
    let imported = mom_llama_runtime::attachment_import(&scope, &conversation.id, &image)?;
    let output = imported
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("attachment result missing"))?;
    assert_eq!(
        output.attachment.state,
        mom_llama_runtime::AttachmentState::Staged
    );
    assert!(output.attachment.message_id.is_empty());
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
    let catalog = mom_llama_runtime::attachment_preview(&output.attachment.id)?
        .result
        .ok_or_else(|| anyhow!("attachment preview catalog missing"))?;
    assert_eq!(catalog.root_sha256, output.attachment.sha256);
    let media = mom_llama_runtime::attachment_preview_media(&primary_preview_anchor(&catalog)?)?
        .map_err(|blocker| anyhow!("{}: {}", blocker.code, blocker.message))?;
    assert_eq!(media.bytes.as_slice(), payload);
    mom_llama_runtime::draft_update(
        Some(&conversation.id),
        "Describe the image.".to_string(),
        vec![output.attachment.id.clone()],
    )?;
    let chat = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: conversation.id.clone(),
            message: "Describe the image.".to_string(),
        },
        ChatSendOptions {
            timeout_s: 1.0,
            fake_fixture: true,
        },
    )?;
    assert_eq!(chat.readiness, "blocked_missing_mmproj");
    let draft = mom_llama_runtime::draft_get(Some(&conversation.id))?
        .result
        .ok_or_else(|| anyhow!("attachment draft missing"))?;
    assert_eq!(draft.attachment_ids, vec![output.attachment.id.clone()]);
    let conversation = mom_llama_runtime::conversation_select(&conversation.id)?
        .result
        .ok_or_else(|| anyhow!("conversation missing after blocked send"))?;
    assert!(conversation.messages.is_empty());
    Ok(())
}

#[test]
fn long_paste_becomes_an_encrypted_text_attachment_without_a_plaintext_file() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("pasted-text-attachment")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Pasted notes".to_string()))?
        .result
        .ok_or_else(|| anyhow!("pasted-notes conversation missing"))?;
    let marker = "private-long-paste-9137 ".repeat(160);
    let imported =
        mom_llama_runtime::attachment_import_pasted_text(&scope, &conversation.id, marker.clone())?;
    assert_eq!(imported.readiness, "contracted");
    let output = imported
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("pasted attachment result missing"))?;
    assert_eq!(output.attachment.source_path, "pasted-text");
    assert!(output.attachment.file_name.starts_with("pasted-text-"));
    assert!(output.attachment.stored_path.starts_with("encrypted://"));
    assert!(!session.path().join(&output.attachment.file_name).exists());

    let conversation = mom_llama_runtime::conversation_select(&conversation.id)?
        .result
        .ok_or_else(|| anyhow!("default conversation missing"))?;
    assert!(conversation.messages.is_empty());
    let draft = mom_llama_runtime::draft_get(Some(&conversation.id))?
        .result
        .ok_or_else(|| anyhow!("pasted attachment draft missing"))?;
    assert_eq!(draft.attachment_ids, vec![output.attachment.id.clone()]);
    mom_llama_runtime::draft_update(
        Some(&conversation.id),
        "Summarize this attachment.".to_string(),
        vec![output.attachment.id.clone()],
    )?;
    let sent = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: conversation.id.clone(),
            message: "Summarize this attachment.".to_string(),
        },
        ChatSendOptions {
            timeout_s: 1.0,
            fake_fixture: true,
        },
    )?;
    assert_eq!(sent.readiness, "fake_fixture_exercised");
    let conversation = mom_llama_runtime::conversation_select(&conversation.id)?
        .result
        .ok_or_else(|| anyhow!("default conversation missing after send"))?;
    let attached_user = conversation
        .messages
        .iter()
        .find(|message| message.role == mom_llama_runtime::MessageRole::User)
        .ok_or_else(|| anyhow!("attached user message missing"))?;
    assert_eq!(
        attached_user.attachment_ids,
        vec![output.attachment.id.clone()]
    );
    assert_eq!(attached_user.content, "Summarize this attachment.");
    assert!(
        mom_llama_runtime::draft_get(Some(&conversation.id))?
            .result
            .is_some_and(|draft| draft.attachment_ids.is_empty())
    );
    let committed = mom_llama_runtime::attachment_list(Some(&conversation.id))?
        .result
        .and_then(|records| {
            records
                .into_iter()
                .find(|record| record.id == output.attachment.id)
        })
        .ok_or_else(|| anyhow!("committed attachment record missing"))?;
    assert_eq!(
        committed.state,
        mom_llama_runtime::AttachmentState::Committed
    );
    assert_eq!(committed.message_id, attached_user.id);
    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
    );
    Ok(())
}

#[test]
fn fixture_mention_commits_staged_attachments_and_clears_the_draft_without_real_readiness()
-> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("fixture-mention-attachment-lifecycle")?;
    let source = mom_llama_runtime::conversation_new(Some("Fixture persona source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("fixture persona source missing"))?;
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "Stable fixture source context".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .ok_or_else(|| anyhow!("fixture persona source reload missing"))?;
    let persona = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: source
            .active_leaf_message_id
            .clone()
            .ok_or_else(|| anyhow!("fixture persona source leaf missing"))?,
        name: "Attachment reviewer".to_string(),
        mention_handle: "attachment-reviewer".to_string(),
        history_mode: PersonaHistoryMode::Full,
    })?
    .result
    .ok_or_else(|| anyhow!("fixture attachment persona missing"))?;
    let host = mom_llama_runtime::conversation_new(Some("Attachment host".to_string()))?
        .result
        .ok_or_else(|| anyhow!("fixture attachment host missing"))?;
    let imported = mom_llama_runtime::attachment_import_pasted_text(
        &scope,
        &host.id,
        "private fixture attachment payload 4182".to_string(),
    )?
    .result
    .ok_or_else(|| anyhow!("fixture mention attachment import missing"))?;
    let addressed = format!(
        "@{} review the attached note",
        persona.execution_profile.mention_handle
    );
    mom_llama_runtime::draft_update(
        Some(&host.id),
        addressed.clone(),
        vec![imported.attachment.id.clone()],
    )?;

    let dispatched = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: host.id.clone(),
            message: addressed,
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    assert_eq!(dispatched.readiness, "fake_fixture_exercised");
    assert!(dispatched.receipt.fake_fixture);
    assert!(!dispatched.receipt.real_engine_invoked);
    let ChatDispatchOutput::Mention { invocation, .. } = dispatched
        .result
        .ok_or_else(|| anyhow!("fixture mention dispatch output missing"))?
    else {
        return Err(anyhow!("fixture attachment dispatch was not a mention"));
    };
    assert!(invocation.results.iter().all(|result| {
        result.fake_fixture && !result.real_engine_invoked && result.model_id == "fake_fixture"
    }));

    let stored = mom_llama_runtime::conversation_select(&host.id)?
        .result
        .ok_or_else(|| anyhow!("fixture attachment host reload missing"))?;
    let user_message = stored
        .messages
        .iter()
        .find(|message| message.id == invocation.user_message_id)
        .ok_or_else(|| anyhow!("fixture mention user message missing"))?;
    assert_eq!(
        user_message.attachment_ids,
        vec![imported.attachment.id.clone()]
    );
    let draft = mom_llama_runtime::draft_get(Some(&host.id))?
        .result
        .ok_or_else(|| anyhow!("fixture mention draft result missing"))?;
    assert!(draft.message.is_empty());
    assert!(draft.attachment_ids.is_empty());
    let record = mom_llama_runtime::attachment_list(Some(&host.id))?
        .result
        .and_then(|records| {
            records
                .into_iter()
                .find(|record| record.id == imported.attachment.id)
        })
        .ok_or_else(|| anyhow!("fixture mention committed attachment missing"))?;
    assert_eq!(record.state, mom_llama_runtime::AttachmentState::Committed);
    assert_eq!(record.message_id, user_message.id);
    Ok(())
}

#[cfg(unix)]
#[test]
fn mcp_process_authority_is_explicit_bounded_and_receipted() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let session = TestSession::new("mcp")?;
    configure_mcp_fixture(&session)?;
    let tools = mom_llama_runtime::mcp_list_tools_in_scope(&scope, "fixture")?;
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

#[cfg(unix)]
fn configure_mcp_fixture(session: &TestSession) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let response = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "tools": [{
                "name":"echo",
                "description":"Returns the supplied value",
                "inputSchema":{
                    "type":"object",
                    "properties":{"value":{"type":"string"}}
                }
            }],
        }
    });
    let body = serde_json::to_string(&response)?;
    let initialize = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fixture", "version": "1"}
        }
    }))?;
    let executable = session.path().join("mcp-fixture");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nIFS= read -r initialize\nprintf '%s\\n' '{}'\nIFS= read -r initialized\nIFS= read -r request\nprintf '%s\\n' '{}'\n",
            initialize.replace('\'', "'\\''"),
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
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn mcp_public_methods_negotiate_only_their_own_capability() -> Result<()> {
    let _session = TestSession::new("mcp-independent-capabilities")?;
    let scope = mom_llama_runtime::OperationScope::detached();
    mom_llama_runtime::settings_update(SettingsUpdate {
        upstream_settings: Some(BTreeMap::from([(
            "mcpNativeEnabled".to_string(),
            json!(true),
        )])),
        ..SettingsUpdate::default()
    })?;
    let configure = |name: &str,
                     capability: serde_json::Value,
                     method: &str,
                     result: serde_json::Value|
     -> Result<()> {
        let initialize = json!({"jsonrpc":"2.0","id":1,"result":{
            "protocolVersion":"2024-11-05", "capabilities":capability,
            "serverInfo":{"name":"owned-capability-fixture","version":"1"}
        }});
        let response = json!({"jsonrpc":"2.0","id":2,"result":result});
        // Each owned process accepts exactly the public method under test, after initialization.
        let script = format!(
            "IFS= read -r initialize || exit 2; printf '%s\\n' '{initialize}'; \
             IFS= read -r initialized || exit 3; IFS= read -r request || exit 4; \
             case \"$request\" in *'\"method\":\"{method}\"'*) printf '%s\\n' '{response}';; *) exit 5;; esac"
        );
        mom_llama_runtime::mcp_configure(
            name.to_string(),
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), script],
            true,
        )?;
        Ok(())
    };
    // Ordinary tools-only control first; resources/prompts remain independent, not optional tools.
    configure(
        "tools",
        json!({"tools":{}}),
        "tools/list",
        json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
    )?;
    let tools = mom_llama_runtime::mcp_list_tools_in_scope(&scope, "tools")?;
    assert_eq!(tools.status, "host_integrated");
    assert_eq!(tools.result.context("ordinary tool list")?[0].name, "echo");
    assert_eq!(scope.active_operation_count(), 0);

    configure(
        "resources-list",
        json!({"resources":{}}),
        "resources/list",
        json!({"resources":[{"uri":"fixture://note","name":"note"}]}),
    )?;
    let resources = mom_llama_runtime::mcp_list_resources_in_scope(&scope, "resources-list")?;
    assert_eq!(resources.status, "host_integrated");
    assert_eq!(
        resources.result.context("resource list")?[0].uri,
        "fixture://note"
    );
    configure(
        "resources-read",
        json!({"resources":{}}),
        "resources/read",
        json!({"contents":[{"uri":"fixture://note","text":"owned note"}]}),
    )?;
    let resource =
        mom_llama_runtime::mcp_read_resource_in_scope(&scope, "resources-read", "fixture://note")?;
    assert_eq!(resource.status, "host_integrated");
    assert!(serde_json::to_string(&resource.result)?.contains("owned note"));

    configure(
        "prompts-list",
        json!({"prompts":{}}),
        "prompts/list",
        json!({"prompts":[{"name":"review","arguments":[]}]}),
    )?;
    let prompts = mom_llama_runtime::mcp_list_prompts_in_scope(&scope, "prompts-list")?;
    assert_eq!(prompts.status, "host_integrated");
    assert_eq!(prompts.result.context("prompt list")?[0].name, "review");
    configure(
        "prompts-get",
        json!({"prompts":{}}),
        "prompts/get",
        json!({"messages":[{"role":"user","content":{"type":"text","text":"owned prompt"}}]}),
    )?;
    let prompt =
        mom_llama_runtime::mcp_get_prompt_in_scope(&scope, "prompts-get", "review", json!({}))?;
    assert_eq!(prompt.status, "host_integrated");
    assert!(serde_json::to_string(&prompt.result)?.contains("owned prompt"));

    for (name, capabilities) in [
        ("absent", json!({})),
        ("wrong", json!({"tools":{}})),
        ("malformed", json!({"resources":true})),
    ] {
        configure(
            name,
            capabilities,
            "resources/list",
            json!({"resources":[]}),
        )?;
        let error = mom_llama_runtime::mcp_list_resources_in_scope(&scope, name)
            .expect_err("resource capability is mandatory");
        assert!(error.to_string().contains("resources capability"));
        assert_eq!(scope.active_operation_count(), 0);
    }
    let error = mom_llama_runtime::mcp_list_tools_in_scope(&scope, "resources-list")
        .expect_err("resource-only server must not admit a tool call");
    assert!(error.to_string().contains("tools capability"));
    let error = mom_llama_runtime::mcp_get_prompt_in_scope(&scope, "tools", "review", json!({}))
        .expect_err("tools-only server must not admit a prompt call");
    assert!(error.to_string().contains("prompts capability"));
    assert_eq!(scope.active_operation_count(), 0);
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn tool_started_callback_rejection_finishes_the_exact_durable_row() -> Result<()> {
    let session = TestSession::new("tool-started-rejection")?;
    let scope = mom_llama_runtime::OperationScope::detached();
    configure_mcp_fixture(&session)?;
    let conversation_id = "started-rejection";
    for reject_started in [false, true] {
        let prompt = format!("Owned callback control {reject_started}");
        let arguments = json!({"value":"ready"});
        let approval = mom_llama_runtime::tool_loop_prepare_in_scope(
            &scope,
            conversation_id,
            prompt.clone(),
            "fixture".to_string(),
            "echo".to_string(),
            arguments.clone(),
            1,
        )?
        .result
        .context("prepared approval")?;
        let mut events = Vec::new();
        let result = mom_llama_runtime::tool_loop_run_stream_in_scope(
            &scope,
            mom_llama_runtime::ToolLoopRunInput {
                conversation_id: conversation_id.to_string(),
                prompt,
                server: "fixture".to_string(),
                tool: "echo".to_string(),
                arguments,
                max_turns: 1,
                approval_id: Some(approval.id),
            },
            |event| {
                let reject = reject_started && event.event == "started";
                events.push(event);
                if reject {
                    Err(anyhow!("owned tool started rejection"))
                } else {
                    Ok(())
                }
            },
        );
        if reject_started {
            assert_eq!(
                result.expect_err("started rejection").to_string(),
                "owned tool started rejection"
            );
        } else {
            // No GGUF is admitted: this exercises the ordinary existing typed no-model terminal.
            let result = result?;
            assert_eq!(result.readiness, "blocked_missing_model");
            assert!(!result.receipt.real_engine_invoked);
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "started");
        assert_eq!(scope.active_operation_count(), 0);
        let rows = mom_llama_runtime::tool_loop_status(Some(conversation_id))?
            .result
            .context("durable loops")?;
        let row = rows
            .iter()
            .find(|row| row.request_id == events[0].request_id)
            .context("exact registered row")?;
        assert_eq!(row.state, mom_llama_runtime::ToolLoopState::Failed);
        assert!(row.current_model_request_id.is_none());
    }
    Ok(())
}

#[test]
fn concurrent_public_conversation_creates_preserve_every_committed_author() -> Result<()> {
    let session = TestSession::new("current-conversation-transactions")?;
    let ordinary =
        mom_llama_runtime::conversation_new(Some("Ordinary preserved author".to_string()))?
            .result
            .context("ordinary public create")?;
    let start = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers = (0..8)
        .map(|worker| {
            let directory = session.path().to_path_buf();
            let start = start.clone();
            std::thread::spawn(move || {
                set_data_dir_override_for_tests(Some(directory));
                start.wait();
                let result: Result<Vec<Conversation>> = (0..8)
                    .map(|index| {
                        mom_llama_runtime::conversation_new(Some(format!(
                            "Author {worker}/{index}"
                        )))?
                        .result
                        .context("committed public create")
                    })
                    .collect();
                set_data_dir_override_for_tests(None);
                result
            })
        })
        .collect::<Vec<_>>();
    // Join all owned threads before propagating a failure or asserting preservation.
    let joined = workers
        .into_iter()
        .map(|worker| worker.join())
        .collect::<Vec<_>>();
    let mut committed = vec![ordinary];
    for result in joined {
        committed.extend(result.map_err(|_| anyhow!("owned conversation writer panicked"))??);
    }
    let retained = mom_llama_runtime::conversation_store::load_db()?;
    assert_eq!(retained.conversations.len(), committed.len());
    for conversation in &committed {
        assert!(
            retained.conversations.contains(conversation),
            "lost committed author {}",
            conversation.id
        );
    }
    Ok(())
}

#[test]
fn conversation_file_import_binds_regular_descriptor_and_encoded_byte_limit() -> Result<()> {
    let session = TestSession::new("conversation-file-admission")?;
    let source = attributed_history_fixture("file-import-source");
    let path = session.path().join("owned.json");
    fs::write(&path, serde_json::to_vec(&source)?)?;
    let ordinary = mom_llama_runtime::conversation_import_file(&path)?;
    assert_eq!(ordinary.status, "contracted");
    let imported = ordinary.result.context("ordinary JSON import")?;
    assert_eq!(imported.messages, source.messages);
    let rows = encrypted_document_snapshot(session.path())?;

    let oversized = session.path().join("oversized.json");
    fs::File::create(&oversized)?.set_len(128 * 1024 * 1024 + 1)?;
    let error =
        mom_llama_runtime::conversation_import_file(&oversized).expect_err("oversized descriptor");
    assert!(error.to_string().contains("byte limit"));
    assert_eq!(encrypted_document_snapshot(session.path())?, rows);
    let error = mom_llama_runtime::conversation_import_file(session.path())
        .expect_err("directory descriptor");
    #[cfg(unix)]
    assert!(error.to_string().contains("regular file"));
    #[cfg(not(unix))]
    let _ = error;
    assert_eq!(encrypted_document_snapshot(session.path())?, rows);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let fifo = session.path().join("owned-fifo.json");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )?;
        let error =
            mom_llama_runtime::conversation_import_file(&fifo).expect_err("FIFO descriptor");
        assert!(error.to_string().contains("regular file"));
        assert_eq!(encrypted_document_snapshot(session.path())?, rows);
    }
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_chat_cancellation_emits_one_terminal_after_native_join() -> Result<()> {
    let _session = configured_real_session("single-chat-cancel-terminal")?;
    let owner = initialize_product_runtime()?;
    let scope = owner.operation_scope();
    product_chat_cache_request(
        &scope,
        "ordinary-before-cancel",
        "Answer with a short greeting.",
    )?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(4096),
        ..SettingsUpdate::default()
    })?;
    let mut events = Vec::new();
    let mut cancellation_observed = false;
    let result = mom_llama_runtime::chat_send_stream_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: "cancel-one-terminal".to_string(),
            message: "Write an extensive numbered account of integers starting at one.".to_string(),
        },
        ChatSendOptions::default(),
        |event| {
            if !cancellation_observed
                && matches!(event.event.as_str(), "delta" | "reasoning_delta")
                && mom_llama_runtime::resident_status(&scope)
                    .is_some_and(|status| status.active_sequences > 0)
            {
                let cancel =
                    mom_llama_runtime::chat_cancel_in_scope(&scope, "cancel-one-terminal")?;
                anyhow::ensure!(cancel.blocker.is_none(), "public cancellation was refused");
                cancellation_observed = true;
            }
            events.push(event);
            Ok(())
        },
    )?;
    assert!(
        cancellation_observed,
        "no active native request was observed; this is not a cancellation pass"
    );
    assert!(result.has_authoritative_cancellation_evidence());
    assert!(result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    let terminals = events
        .iter()
        .filter(|event| matches!(event.event.as_str(), "completed" | "cancelled" | "failed"))
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0].event, "cancelled");
    assert_eq!(terminals[0].request_id, events[0].request_id);
    assert_eq!(scope.active_operation_count(), 0);
    assert!(
        mom_llama_runtime::resident_status(&scope)
            .is_none_or(|status| status.active_sequences == 0)
    );
    Ok(())
}

fn configured_real_session(name: &str) -> Result<TestSession> {
    let model_path = std::env::var_os("MOM_LLAMA_MODEL_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("MOM_LLAMA_MODEL_PATH must point to a real local GGUF"))?;
    let session = TestSession::new(name)?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(Some(model_path)),
        native_device: Some(llama_native_types::NativeDevice::Cpu),
        // Reasoning-capable small models can spend the first hundred tokens
        // inside a private reasoning block. A real acceptance must budget
        // enough room to prove that visible assistant text is produced.
        max_tokens: Some(256),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    Ok(session)
}

fn initialize_product_runtime() -> Result<mom_llama_runtime::native_runtime::ProductRuntimeOwner> {
    let settings = mom_llama_runtime::config::resolve_settings()?;
    mom_llama_runtime::native_runtime::ProductRuntimeOwner::initialize(&settings)
}

fn product_chat_cache_request(
    scope: &mom_llama_runtime::OperationScope,
    conversation_id: &str,
    user_message: &str,
) -> Result<ChatSendOutput> {
    let result = mom_llama_runtime::chat_send_in_scope(
        scope,
        ChatSendInput {
            conversation_id: conversation_id.to_owned(),
            message: user_message.to_owned(),
        },
        ChatSendOptions::default(),
    )?;
    assert!(
        result.receipt.real_engine_invoked,
        "{}: {:?}",
        result.readiness, result.blocker
    );
    result.result.ok_or_else(|| {
        anyhow!(
            "real product chat returned {}: {:?}",
            result.readiness,
            result.blocker
        )
    })
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_voice_greeting_reports_its_actual_prefix_reuse() -> Result<()> {
    let _session = configured_real_session("real-voice-template-cache")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        max_tokens: Some(16),
        context_tokens: Some(2048),
        batch_tokens: Some(128),
        kv_cache_policy: Some(KvCachePolicy::KvCacheCandidate),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let conversation = mom_llama_runtime::conversation_new(Some("Voice cache proof".to_owned()))?
        .result
        .ok_or_else(|| anyhow!("voice cache proof conversation missing"))?;
    let _first = product_chat_cache_request(
        &scope,
        &conversation.id,
        "[Phone call] [Greeting] Greet Jan in one short sentence.",
    )?;
    let second = product_chat_cache_request(
        &scope,
        &conversation.id,
        "[Phone call] [Greeting] Greet Jan again in one short sentence.",
    )?;
    assert!(
        second.cache_metrics.resident_prefix_tokens > 0
            || second.cache_metrics.restored_prefix_tokens > 0,
        "the next greeting must reuse actual KV cells: {:?}",
        second.cache_metrics
    );
    Ok(())
}

#[test]
#[ignore = "requires a real local Gemma GGUF and matching multimodal projector"]
fn real_startup_preparation_persists_personas_and_history_without_editing_messages() -> Result<()> {
    let _session = configured_real_session("real-startup-voice-cache-preparation")?;
    let projector = std::env::var_os("MOM_LLAMA_MMPROJ_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("MOM_LLAMA_MMPROJ_PATH must point to the matching projector"))?;
    let configured = mom_llama_runtime::settings_update(SettingsUpdate {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        context_tokens: Some(8192),
        batch_tokens: Some(512),
        max_parallel_sequences: Some(2),
        resident_memory_budget_bytes: Some(21_845 * 1024 * 1024),
        max_tokens: Some(8),
        mmproj_path: Some(Some(projector)),
        kv_cache_policy: Some(KvCachePolicy::KvCacheCandidate),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("startup cache settings were not accepted"))?;
    assert!(configured.context_tokens >= 512);
    assert_eq!(configured.batch_tokens, 512);
    assert_eq!(configured.max_parallel_sequences, 2);
    assert_eq!(
        configured.resident_memory_budget_bytes,
        21_845 * 1024 * 1024
    );
    assert_eq!(configured.sampling_config()?.max_tokens, 8);
    assert_eq!(configured.kv_cache_policy, KvCachePolicy::KvCacheCandidate);

    let personas = mom_llama_runtime::persona_list()?
        .result
        .unwrap_or_default();
    anyhow::ensure!(!personas.is_empty(), "the persona catalog is empty");
    let persona_ids = personas
        .iter()
        .map(|persona| persona.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let owner = initialize_product_runtime()?;
    let scope = owner.operation_scope();

    // Build one materialized persona conversation with deterministic fixture
    // history. The fixture only creates input data; all cache preparation below
    // uses the real configured native model.
    let persona = personas
        .first()
        .ok_or_else(|| anyhow!("the persona catalog is empty"))?;
    mom_llama_runtime::conversation_draft_open(Some(persona.id.clone()))?;
    let conversation = mom_llama_runtime::conversation_draft_call_start()?
        .result
        .ok_or_else(|| anyhow!("persona conversation was not materialized"))?;
    let fixture = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: conversation.id.clone(),
            message: "Synthetic startup-cache history: the caller prefers concise replies.".into(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    anyhow::ensure!(
        fixture.receipt.fake_fixture,
        "history setup did not use the fixture path"
    );
    let saved_before = mom_llama_runtime::conversation_select(&conversation.id)?
        .result
        .ok_or_else(|| anyhow!("materialized conversation could not be reloaded"))?
        .messages;
    anyhow::ensure!(saved_before.len() >= 2, "fixture history was not persisted");

    let mut progress = Vec::new();
    let summary = mom_llama_runtime::prepare_startup_voice_caches(&scope, |item| {
        println!(
            "STARTUP_VOICE_CACHE_PROGRESS completed={} total={} elapsed_ms={} positions={} bytes={}",
            item.completed, item.total, item.elapsed_ms, item.cache_positions, item.state_bytes
        );
        progress.push(item);
    })?;
    assert_eq!(summary.prepared, personas.len() + 1);
    assert_eq!(summary.total, personas.len() + 1);
    assert_eq!(progress.len(), summary.total * 2);
    anyhow::ensure!(
        progress.iter().any(|item| item.completed == 0)
            && progress.iter().any(|item| item.completed == summary.total),
        "startup preparation did not report its boundaries"
    );

    let status = mom_llama_runtime::kv_cache_status()?
        .result
        .ok_or_else(|| anyhow!("cache status was unavailable after preparation"))?;
    let ready_personas = status
        .entries
        .iter()
        .filter(|entry| {
            entry.state == mom_llama_runtime::kv_cache::CacheEntryState::Ready
                && entry.tier == mom_llama_runtime::kv_cache::CacheTier::PersonaPack
        })
        .filter_map(|entry| entry.owner_id.as_deref())
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        persona_ids
            .iter()
            .all(|persona_id| ready_personas.contains(persona_id.as_str())),
        "not every catalog persona has a ready native checkpoint"
    );
    let persona_prefix = status
        .entries
        .iter()
        .find(|entry| {
            entry.state == mom_llama_runtime::kv_cache::CacheEntryState::Ready
                && entry.tier == mom_llama_runtime::kv_cache::CacheTier::PersonaPack
                && entry.owner_id.as_deref() == Some(persona.id.as_str())
        })
        .ok_or_else(|| anyhow!("selected persona prefix checkpoint is missing"))?;
    let session_checkpoint = status
        .entries
        .iter()
        .find(|entry| {
            entry.state == mom_llama_runtime::kv_cache::CacheEntryState::Ready
                && entry.tier == mom_llama_runtime::kv_cache::CacheTier::SessionPersistent
                && entry.owner_id.as_deref() == Some(conversation.id.as_str())
        })
        .ok_or_else(|| anyhow!("materialized conversation checkpoint is missing"))?;
    let latest_persona_checkpoint_ms = status
        .entries
        .iter()
        .filter(|entry| {
            entry.state == mom_llama_runtime::kv_cache::CacheEntryState::Ready
                && entry.tier == mom_llama_runtime::kv_cache::CacheTier::PersonaPack
                && persona_ids.contains(entry.owner_id.as_deref().unwrap_or_default())
        })
        .map(|entry| entry.created_at_ms)
        .max()
        .ok_or_else(|| anyhow!("no ready persona checkpoint timestamps were recorded"))?;
    anyhow::ensure!(
        latest_persona_checkpoint_ms <= session_checkpoint.created_at_ms,
        "persona checkpoints were not prepared before the materialized chat"
    );
    anyhow::ensure!(session_checkpoint.state_bytes > 0);
    anyhow::ensure!(
        status
            .entries
            .iter()
            .filter(|entry| {
                entry.state == mom_llama_runtime::kv_cache::CacheEntryState::Ready
                    && (entry.tier == mom_llama_runtime::kv_cache::CacheTier::PersonaPack
                        || (entry.tier
                            == mom_llama_runtime::kv_cache::CacheTier::SessionPersistent
                            && entry.owner_id.as_deref() == Some(conversation.id.as_str())))
            })
            .all(|entry| entry.fingerprint == session_checkpoint.fingerprint),
        "startup checkpoints do not share the configured native build identity"
    );
    assert_eq!(
        session_checkpoint.fingerprint.context_tokens, configured.context_tokens,
        "startup checkpoint must use the effective production context plan"
    );
    anyhow::ensure!(
        session_checkpoint.token_ids.len() > persona_prefix.token_ids.len(),
        "conversation checkpoint contains only the system/persona prefix"
    );
    anyhow::ensure!(
        session_checkpoint
            .cells
            .len()
            .max(session_checkpoint.token_ids.len())
            > persona_prefix
                .cells
                .len()
                .max(persona_prefix.token_ids.len()),
        "conversation checkpoint did not retain history KV positions"
    );
    let saved_after = mom_llama_runtime::conversation_select(&conversation.id)?
        .result
        .ok_or_else(|| anyhow!("materialized conversation disappeared after preparation"))?
        .messages;
    assert_eq!(saved_after, saved_before, "setup changed saved messages");

    let chat_id = conversation.id.clone();
    let checkpoint_id = session_checkpoint.id.clone();
    let checkpoint_cells = session_checkpoint.cells.len();
    let checkpoint_tokens = session_checkpoint.token_ids.len();
    let persona_count = persona_ids.len();
    drop(scope);
    drop(owner);

    let restarted_owner = initialize_product_runtime()?;
    let restarted_scope = restarted_owner.operation_scope();
    let restore_started = Instant::now();
    let restored =
        mom_llama_runtime::kv_cache_prepare_voice_persona_resident(&restarted_scope, &chat_id)?;
    anyhow::ensure!(
        restored.readiness == "resident_prefix_prepared",
        "worker restart did not restore the prepared conversation: {:?}",
        restored.blocker
    );
    let restored = restored
        .result
        .ok_or_else(|| anyhow!("conversation checkpoint restore returned no result"))?;
    anyhow::ensure!(
        restored.persisted,
        "restart did not restore durable native state"
    );
    let restored_metadata = restored
        .metadata
        .as_ref()
        .ok_or_else(|| anyhow!("restart did not use the persisted checkpoint"))?;
    assert_eq!(restored_metadata.id, checkpoint_id);
    assert_eq!(restored.prefill_us, 0, "restart replayed the saved prompt");
    assert_eq!(
        restored.resident_prefix_tokens,
        checkpoint_tokens.max(checkpoint_cells)
    );

    println!(
        "STARTUP_VOICE_CACHE_ACCEPTANCE {}",
        json!({
            "personas": persona_count,
            "effective_context_tokens": configured.context_tokens,
            "effective_batch_tokens": configured.batch_tokens,
            "effective_max_parallel_sequences": configured.max_parallel_sequences,
            "resident_memory_budget_bytes": configured.resident_memory_budget_bytes,
            "prepared": summary.prepared,
            "reused": summary.reused,
            "targets": summary.total,
            "progress_events": progress.len(),
            "checkpoint_tokens": checkpoint_tokens,
            "checkpoint_cells": checkpoint_cells,
            "checkpoint_bytes": session_checkpoint.state_bytes,
            "restored_tokens": restored.resident_prefix_tokens,
            "restore_prefill_us": restored.prefill_us,
            "prepare_elapsed_ms": summary.elapsed_ms,
            "restore_elapsed_ms": restore_started.elapsed().as_millis(),
        })
    );
    Ok(())
}

#[test]
#[ignore = "requires real Gemma GGUF, matching projector, and 16 kHz voice WAV"]
fn real_prepared_persona_prefix_is_reused_by_first_voice_turn() -> Result<()> {
    let _session = configured_real_session("real-voice-persona-prefix")?;
    let projector = std::env::var_os("MOM_LLAMA_MMPROJ_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("MOM_LLAMA_MMPROJ_PATH must point to the matching projector"))?;
    let wav_path = std::env::var_os("MOM_LLAMA_TEST_VOICE_WAV")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("MOM_LLAMA_TEST_VOICE_WAV must point to a 16 kHz WAV"))?;
    let samples = pcm16_mono_16khz_wav_samples(&wav_path)?;
    anyhow::ensure!(samples.len() >= 8_000, "voice WAV needs 500 ms of PCM");
    let configured_settings = mom_llama_runtime::settings_update(SettingsUpdate {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        max_tokens: Some(8),
        mmproj_path: Some(Some(projector)),
        context_tokens: Some(8192),
        batch_tokens: Some(256),
        max_parallel_sequences: Some(4),
        resident_memory_budget_bytes: Some(21_845 * 1024 * 1024),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?
    .result
    .ok_or_else(|| anyhow!("voice context settings were not accepted"))?;
    assert!(
        (8192..=19_456).contains(&configured_settings.context_tokens),
        "the manual memory cap should resolve a bounded context, got {}",
        configured_settings.context_tokens
    );
    assert_eq!(
        configured_settings.resident_memory_budget_bytes,
        21_845 * 1024 * 1024
    );
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let persona = mom_llama_runtime::persona_list()?
        .result
        .unwrap_or_default()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no built-in persona"))?;
    mom_llama_runtime::conversation_draft_open(Some(persona.id.clone()))?;
    let started = std::time::Instant::now();
    let warmed = mom_llama_runtime::runtime_warm(&scope)?;
    let warmed = warmed
        .result
        .ok_or_else(|| anyhow!("runtime warm failed: {:?}", warmed.blocker))?;
    eprintln!(
        "VOICE_PERSONA_PREFIX_CACHE {{\"prepare_ms\":{},\"prefix_ms\":{},\"status\":\"{}\",\"ready\":{}}}",
        started.elapsed().as_millis(),
        warmed.persona_prefix_warm_us as f64 / 1000.0,
        warmed.persona_prefix_warm_status,
        warmed.persona_prefixes_ready
    );
    assert_eq!(warmed.persona_prefix_warm_status, "ready");
    assert_eq!(warmed.persona_prefixes_ready, 1);
    let warmed_again = mom_llama_runtime::runtime_warm(&scope)?
        .result
        .ok_or_else(|| anyhow!("second runtime warm failed"))?;
    assert_eq!(
        warmed_again.persona_prefix_warm_status, "blocked",
        "startup warming must not supersede a worker after its first admitted prefix"
    );
    assert_eq!(warmed_again.persona_prefixes_ready, 0);
    assert_eq!(warmed_again.persona_prefixes_reused, 0);
    let resident = mom_llama_runtime::kv_cache_prepare_voice_persona_resident(&scope, &persona.id)?
        .result
        .ok_or_else(|| anyhow!("explicit resident persona preparation failed"))?;
    assert!(resident.resident_prefix_tokens > 0);
    assert!(resident.metadata.is_none());
    assert!(!resident.persisted);
    let conversation = mom_llama_runtime::conversation_draft_call_start()?
        .result
        .ok_or_else(|| anyhow!("persona call draft did not materialize"))?;
    let greeting_started = Instant::now();
    let greeting = product_chat_cache_request(
        &scope,
        &conversation.id,
        "[Phone call] [Greeting] You have just picked up a call from Jan. Greet Jan naturally in one short sentence, using wording unlike earlier pickups in this conversation. No headings, lists, or preamble.",
    )?;
    eprintln!(
        "VOICE_PERSONA_GREETING {}",
        serde_json::to_string(&json!({
            "wall_ms": greeting_started.elapsed().as_millis(),
            "first_token_ms": greeting.first_token_ms,
            "prompt_tokens": greeting.prompt_tokens,
            "completion_tokens": greeting.completion_tokens,
            "cache_reused": greeting.cache_reused,
            "cache_metrics": greeting.cache_metrics,
        }))?
    );
    assert!(
        greeting.cache_reused,
        "pickup greeting missed the prepared persona prefix"
    );
    let voice = mom_llama_runtime::voice_audio_begin_in_scope(
        &scope,
        &conversation.id,
        "[Phone call] Reply naturally in at most two short sentences. No headings, lists, preamble, or extended explanation.",
    )?;
    assert!(
        voice.begin_metrics().cached_prefix_found,
        "first voice turn did not reuse the prepared persona cache: {:?}",
        voice.begin_metrics()
    );
    let restored_tokens = voice.begin_metrics().cached_prefix_tokens;
    voice.wait_for_resume()?;
    let capture_started = Instant::now();
    for (index, chunk) in samples[..8_000].chunks(160).enumerate() {
        let deadline = capture_started + Duration::from_millis(index as u64 * 10);
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        voice.push_pcm(chunk)?;
    }
    if let Some(wait) =
        (capture_started + Duration::from_millis(500)).checked_duration_since(Instant::now())
    {
        std::thread::sleep(wait);
    }
    let silence = Instant::now();
    voice.seal_input()?;
    let mut first_token_ms = None;
    let stream_events = std::cell::RefCell::new(Vec::new());
    let finished = mom_llama_runtime::voice_audio_finish_with_precommit_in_scope(
        voice,
        ChatSendOptions {
            timeout_s: 60.0,
            fake_fixture: false,
        },
        Some(|event: mom_llama_runtime::ChatStreamEvent| {
            stream_events.borrow_mut().push(event.event.clone());
            if event.event == "text_completed" {
                assert!(event.delta.as_deref().is_some_and(|text| !text.is_empty()));
            }
            if first_token_ms.is_none()
                && matches!(event.event.as_str(), "delta" | "reasoning_delta")
            {
                first_token_ms = Some(silence.elapsed().as_millis());
            }
            Ok(())
        }),
        || {
            assert_eq!(
                stream_events.borrow().last().map(String::as_str),
                Some("text_completed"),
                "speech must receive final text before waiting for recording confirmation"
            );
            assert!(
                !stream_events
                    .borrow()
                    .iter()
                    .any(|event| event == "completed"),
                "text EOF must not claim the exchange is committed"
            );
            let imported = mom_llama_runtime::attachment_import_voice_message(
                &scope,
                &conversation.id,
                std::fs::read(&wav_path)?,
            )?;
            Ok(Some(
                imported
                    .result
                    .ok_or_else(|| {
                        anyhow!("voice attachment import failed: {:?}", imported.blocker)
                    })?
                    .attachment
                    .id,
            ))
        },
    )?;
    let output = finished
        .result
        .ok_or_else(|| anyhow!("prepared first voice turn failed: {:#?}", finished.blocker))?;
    assert_eq!(
        stream_events.borrow().last().map(String::as_str),
        Some("completed"),
        "the terminal stream event must follow the history commit"
    );
    let audio = output
        .voice_audio_metrics
        .as_ref()
        .ok_or_else(|| anyhow!("prepared first voice turn omitted audio metrics"))?;
    eprintln!(
        "VOICE_PERSONA_FIRST_TURN {}",
        serde_json::to_string(&json!({
            "cached_prefix_tokens": restored_tokens,
            "silence_to_first_token_ms": first_token_ms,
            "prefix_us": audio.prefix_us,
            "restored_prefix_tokens": audio.restored_prefix_tokens,
            "projector_us": audio.projector_us,
            "projector_max_us": audio.projector_max_us,
            "last_projector_finish_us": audio.last_projector_finish_us,
            "decode_start_us": audio.decode_start_us,
        }))?
    );
    anyhow::ensure!(
        first_token_ms.is_some(),
        "prepared turn emitted no text token"
    );
    let following = mom_llama_runtime::voice_audio_begin_in_scope(
        &scope,
        &conversation.id,
        "[Phone call] Continue the conversation naturally.",
    )?;
    assert!(
        !following.begin_metrics().attachment_free_fast_path,
        "the prior user recording must still be present until transcription completes"
    );
    assert!(
        following.begin_metrics().cached_prefix_found,
        "pending historical audio must not bypass the resident text prefix: {:?}",
        following.begin_metrics()
    );
    following.cancel();

    let mut overflow = conversation.clone();
    overflow.id = uuid::Uuid::new_v4().to_string();
    overflow.title = "Voice context rollover proof".to_string();
    let handle = overflow
        .execution_profile
        .mention_handle
        .chars()
        .take(32)
        .collect::<String>();
    overflow.execution_profile.mention_handle = format!("{handle}-rollover");
    overflow.messages.clear();
    overflow.branch_root_message_id = None;
    overflow.active_leaf_message_id = None;
    let mut parent_id = None;
    for index in 0..24 {
        let user_id = format!("rollover-user-{index}");
        let assistant_id = format!("rollover-assistant-{index}");
        overflow.messages.push(Message {
            id: user_id.clone(),
            conversation_id: overflow.id.clone(),
            role: MessageRole::User,
            content: format!(
                "Historical user turn {index}: {}",
                "Earlier detail remains in the original source record. ".repeat(64)
            ),
            created_at: index.to_string(),
            parent_id: parent_id.take(),
            model: None,
            receipt_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: Vec::new(),
        });
        overflow.messages.push(Message {
            id: assistant_id.clone(),
            conversation_id: overflow.id.clone(),
            role: MessageRole::Assistant,
            content: format!(
                "Historical assistant turn {index}: {}",
                "The full answer remains available in source history. ".repeat(64)
            ),
            created_at: index.to_string(),
            parent_id: Some(user_id),
            model: None,
            receipt_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: Vec::new(),
        });
        parent_id = Some(assistant_id);
    }
    overflow.active_leaf_message_id = parent_id;
    let imported = mom_llama_runtime::conversation_import_json(&serde_json::to_string(&overflow)?)?
        .result
        .ok_or_else(|| anyhow!("large voice history import failed"))?;
    let original_messages = imported.messages.clone();
    let persona_system_message = imported.execution_profile.system_message.clone();
    anyhow::ensure!(
        !original_messages.is_empty(),
        "rollover fixture must retain source messages"
    );
    anyhow::ensure!(
        persona_system_message.is_some(),
        "rollover fixture must retain the selected Persona's system prompt"
    );
    assert_eq!(
        persona_system_message, conversation.execution_profile.system_message,
        "importing the rollover fixture must preserve the Persona prompt"
    );
    assert_eq!(
        imported.source_conversation_id, conversation.source_conversation_id,
        "the rollover fixture must remain owned by the same Persona"
    );

    let rollover_voice = mom_llama_runtime::voice_audio_begin_in_scope(
        &scope,
        &imported.id,
        "[Phone call] Reply naturally and briefly to the latest message.",
    )?;
    let rollover_begin = rollover_voice.begin_metrics().clone();
    let reserved_prompt_limit = rollover_begin.resident_context_tokens as usize
        - (rollover_begin.resident_context_tokens as usize / 4).min(4096)
        - 96
        - 32;
    eprintln!(
        "VOICE_CONTEXT_ROLLOVER {}",
        serde_json::to_string(&json!({
            "profile": "32GB-manual-21845MiB",
            "fixture": "24 synthetic source exchanges and synthetic 16kHz tone",
            "source_messages": original_messages.len(),
            "resident_context_tokens": rollover_begin.resident_context_tokens,
            "measured_static_prompt_tokens": rollover_begin.measured_prompt_tokens,
            "reserved_prompt_limit": reserved_prompt_limit,
            "omitted_history_messages": rollover_begin.omitted_history_messages,
            "cached_prefix_found": rollover_begin.cached_prefix_found,
            "cached_prefix_tokens": rollover_begin.cached_prefix_tokens,
        }))?
    );
    assert!(
        rollover_begin.omitted_history_messages > 0,
        "oversized retained history should be excluded from inference"
    );
    assert!(
        rollover_begin.measured_prompt_tokens <= reserved_prompt_limit,
        "measured prompt {} exceeded the voice context limit {reserved_prompt_limit} (context {})",
        rollover_begin.measured_prompt_tokens,
        rollover_begin.resident_context_tokens
    );
    assert!(
        rollover_begin.cached_prefix_found,
        "the rollover conversation should reuse the prepared Persona prefix"
    );
    rollover_voice.wait_for_resume()?;
    let capture_started = Instant::now();
    for (index, chunk) in samples[..8_000].chunks(160).enumerate() {
        let deadline = capture_started + Duration::from_millis(index as u64 * 10);
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        rollover_voice.push_pcm(chunk)?;
    }
    if let Some(wait) =
        (capture_started + Duration::from_millis(500)).checked_duration_since(Instant::now())
    {
        std::thread::sleep(wait);
    }
    rollover_voice.seal_input()?;
    let rollover_finished = mom_llama_runtime::voice_audio_finish_with_precommit_in_scope(
        rollover_voice,
        ChatSendOptions {
            timeout_s: 60.0,
            fake_fixture: false,
        },
        None::<fn(mom_llama_runtime::ChatStreamEvent) -> Result<()>>,
        || {
            let imported_audio = mom_llama_runtime::attachment_import_voice_message(
                &scope,
                &imported.id,
                std::fs::read(&wav_path)?,
            )?;
            Ok(Some(
                imported_audio
                    .result
                    .ok_or_else(|| {
                        anyhow!(
                            "rollover voice attachment import failed: {:?}",
                            imported_audio.blocker
                        )
                    })?
                    .attachment
                    .id,
            ))
        },
    )?;
    let rollover_output = rollover_finished.result.ok_or_else(|| {
        anyhow!(
            "rollover voice turn failed: {:#?}",
            rollover_finished.blocker
        )
    })?;
    let rollover_audio = rollover_output
        .voice_audio_metrics
        .as_ref()
        .ok_or_else(|| anyhow!("rollover turn omitted voice metrics"))?;
    assert!(
        rollover_audio.accepted_samples >= 8_000,
        "the real voice path must accept the full synthetic PCM window"
    );
    let committed = mom_llama_runtime::conversation_select(&imported.id)?
        .result
        .ok_or_else(|| anyhow!("committed rollover conversation disappeared"))?;
    anyhow::ensure!(
        committed.messages.len() >= original_messages.len(),
        "committing voice must retain every original source message"
    );
    for (saved, original) in committed.messages[..original_messages.len()]
        .iter()
        .zip(&original_messages)
    {
        assert_eq!(saved.id, original.id, "source message identity changed");
        assert_eq!(saved.conversation_id, original.conversation_id);
        assert_eq!(saved.role, original.role);
        assert_eq!(
            saved.content, original.content,
            "source message text changed"
        );
        assert_eq!(saved.created_at, original.created_at);
        assert_eq!(saved.parent_id, original.parent_id);
        assert_eq!(saved.model, original.model);
        assert_eq!(saved.receipt_id, original.receipt_id);
        assert_eq!(saved.prompt_tokens, original.prompt_tokens);
        assert_eq!(saved.completion_tokens, original.completion_tokens);
        assert_eq!(saved.reasoning_content, original.reasoning_content);
        assert_eq!(saved.reasoning_incomplete, original.reasoning_incomplete);
        assert_eq!(saved.attribution, original.attribution);
        assert_eq!(saved.attachment_ids, original.attachment_ids);
    }
    let saved_window = committed
        .inference_window
        .clone()
        .ok_or_else(|| anyhow!("rollover boundary was not durably saved"))?;
    assert!(
        original_messages
            .iter()
            .any(|message| message.id == saved_window.covered_through_message_id),
        "the saved boundary must point into the unchanged original source history"
    );
    assert_eq!(
        rollover_audio.begin.omitted_history_messages,
        rollover_begin.omitted_history_messages
    );

    let next_rollover_voice = mom_llama_runtime::voice_audio_begin_in_scope(
        &scope,
        &imported.id,
        "[Phone call] Reply naturally and briefly to the latest message.",
    )?;
    assert_eq!(
        next_rollover_voice.begin_metrics().omitted_history_messages,
        rollover_begin.omitted_history_messages,
        "the following small turn should keep the saved context boundary"
    );
    assert!(
        next_rollover_voice.begin_metrics().cached_prefix_found
            && next_rollover_voice.begin_metrics().cached_prefix_tokens > 0,
        "the following turn should reuse a resident Persona prefix: {:?}",
        next_rollover_voice.begin_metrics()
    );
    eprintln!(
        "VOICE_CONTEXT_NEXT_TURN {}",
        serde_json::to_string(&json!({
            "omitted_history_messages": next_rollover_voice.begin_metrics().omitted_history_messages,
            "cached_prefix_found": next_rollover_voice.begin_metrics().cached_prefix_found,
            "cached_prefix_tokens": next_rollover_voice.begin_metrics().cached_prefix_tokens,
        }))?
    );
    next_rollover_voice.cancel();
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH and MOM_LLAMA_MM_PROJ_PATH for real Gemma audio"]
fn real_committed_session_checkpoint_survives_scope_shutdown() -> Result<()> {
    let _session = configured_real_session("real-shutdown-session-checkpoint")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        context_tokens: Some(2048),
        batch_tokens: Some(128),
        max_parallel_sequences: Some(4),
        max_tokens: Some(8),
        mmproj_path: Some(Some(PathBuf::from(std::env::var(
            "MOM_LLAMA_MM_PROJ_PATH",
        )?))),
        resident_memory_budget_bytes: Some(21_845 * 1024 * 1024),
        kv_cache_policy: Some(KvCachePolicy::KvCacheCandidate),
        ..SettingsUpdate::default()
    })?;
    let owner = initialize_product_runtime()?;
    let scope = owner.operation_scope();
    let persona = mom_llama_runtime::persona_list()?
        .result
        .unwrap_or_default()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no built-in persona"))?;
    mom_llama_runtime::conversation_draft_open(Some(persona.id))?;
    let conversation = mom_llama_runtime::conversation_draft_call_start()?
        .result
        .ok_or_else(|| anyhow!("persona call did not materialize"))?;
    product_chat_cache_request(
        &scope,
        &conversation.id,
        "Remember this phrase: copper heron.",
    )?;
    mom_llama_runtime::kv_cache_checkpoint_committed_voice_session(&scope, &conversation.id)?;
    let before = mom_llama_runtime::kv_cache_status()?
        .result
        .ok_or_else(|| anyhow!("missing cache status"))?;
    let older = before
        .entries
        .iter()
        .find(|entry| {
            entry.tier == mom_llama_runtime::kv_cache::CacheTier::SessionPersistent
                && entry.owner_id.as_deref() == Some(&conversation.id)
        })
        .ok_or_else(|| anyhow!("missing first committed checkpoint"))?;
    // Keep idle persistence out while a newer, divergent prompt becomes
    // resident. Pickup must not overwrite it with the previous disk state.
    mom_llama_runtime::kv_cache_set_voice_call_activity(&scope, "shutdown-witness", true)?;
    mom_llama_runtime::conversation_system_message_update(&conversation.id, Some("You are a concise archivist. Retain the caller's exact invented phrases. Answer briefly.".into()))?;
    let latest =
        product_chat_cache_request(&scope, &conversation.id, "Now remember: violet badger.")?;
    let warm =
        mom_llama_runtime::kv_cache_prepare_voice_persona_resident(&scope, &conversation.id)?
            .result
            .ok_or_else(|| anyhow!("missing resident preparation result"))?;
    assert!(
        warm.metadata.is_none(),
        "new resident request must take precedence over older checkpoint {}",
        older.id
    );
    assert_eq!(warm.prefill_us, 0);
    assert!(warm.resident_prefix_tokens > 0);

    // Launch warming is permitted only on a pristine worker. The selected
    // materialized Chat owns a newer committed request, so this attempt must
    // leave its exact resident revision and committed-prefix count untouched.
    let settings = mom_llama_runtime::config::resolve_settings()?;
    let handle = mom_llama_runtime::native_runtime::resident_model(&scope, &settings)?;
    let before_startup_warm = handle.inspect_resident_conversation(Some(&latest.request_id))?;
    let before_startup_state = handle
        .snapshot_conversation_for_request_if_fits(&latest.request_id, 128 * 1024 * 1024)?
        .ok_or_else(|| anyhow!("latest committed request was not snapshot eligible"))?;
    let startup_warm = mom_llama_runtime::runtime_warm(&scope)?
        .result
        .ok_or_else(|| anyhow!("startup warm returned no result"))?;
    assert!(startup_warm.audio_projector_warmed);
    assert_eq!(startup_warm.persona_prefix_warm_status, "blocked");
    assert_eq!(startup_warm.persona_prefixes_ready, 0);
    let after_startup_warm = handle.inspect_resident_conversation(Some(&latest.request_id))?;
    assert_eq!(after_startup_warm.revision, before_startup_warm.revision);
    assert_eq!(
        after_startup_warm.committed_prefix_tokens,
        before_startup_warm.committed_prefix_tokens
    );
    assert_eq!(
        after_startup_warm.committed_prefix_tokens,
        Some(before_startup_state.token_count)
    );
    // Verify the supported warm API still takes the resident path after
    // opportunistic startup work.
    let warm_after_startup =
        mom_llama_runtime::kv_cache_prepare_voice_persona_resident(&scope, &conversation.id)?
            .result
            .ok_or_else(|| anyhow!("latest resident prefix was not prepared"))?;
    assert_eq!(warm_after_startup.prefill_us, 0);
    assert_eq!(
        warm_after_startup.resident_prefix_tokens,
        warm.resident_prefix_tokens
    );
    let after_startup_state = handle
        .snapshot_conversation_for_request_if_fits(&latest.request_id, 128 * 1024 * 1024)?
        .ok_or_else(|| anyhow!("startup warm lost the latest committed request"))?;
    assert_eq!(
        after_startup_state.cells, before_startup_state.cells,
        "startup warm must not replace the latest committed KV cells"
    );
    assert!(handle.resident_conversation_covers_cells(after_startup_state.cells)?);
    println!(
        "STARTUP_WARM_PRESERVED_ACTIVE_SESSION {}",
        json!({
            "revision": after_startup_warm.revision,
            "committed_prefix_tokens": after_startup_warm.committed_prefix_tokens,
            "warm_status": startup_warm.persona_prefix_warm_status,
        })
    );
    println!(
        "PENDING_SESSION_RAM_HIT {}",
        json!({"resident_prefix_tokens": warm.resident_prefix_tokens, "prefill_us": warm.prefill_us, "older_checkpoint": older.id})
    );
    let drain = mom_llama_runtime::prepare_committed_session_checkpoint_drain(&scope);
    scope.request_cancellation();
    assert!(scope.cancellation_requested());
    let started = Instant::now();
    let saved = drain.flush()?;
    assert_eq!(
        saved, 1,
        "shutdown must persist the resident committed request"
    );
    let after = mom_llama_runtime::kv_cache_status()?
        .result
        .ok_or_else(|| anyhow!("missing cache status"))?;
    let checkpoint = after
        .entries
        .iter()
        .find(|entry| {
            entry.id != older.id
                && entry.tier == mom_llama_runtime::kv_cache::CacheTier::SessionPersistent
                && entry.owner_id.as_deref() == Some(&conversation.id)
        })
        .ok_or_else(|| anyhow!("shutdown omitted the conversation checkpoint"))?;
    assert!(checkpoint.state_bytes > 0);
    let cache_id = checkpoint.id.clone();
    println!(
        "SHUTDOWN_CHECKPOINT_SAVED {}",
        json!({"saved": saved, "bytes": checkpoint.state_bytes, "elapsed_ms": started.elapsed().as_millis()})
    );
    drop(scope);
    drop(owner);
    let restored_owner = initialize_product_runtime()?;
    let restored_scope = restored_owner.operation_scope();
    // Session state is intentionally not exposed by the generic persona-cache
    // restore command. Exercise the same conversation-scoped warm boundary as
    // pickup; it accepts only authenticated native state, never token replay.
    let restored = mom_llama_runtime::kv_cache_prepare_voice_persona_resident(
        &restored_scope,
        &conversation.id,
    )?;
    assert_eq!(
        restored.readiness, "resident_prefix_prepared",
        "{:?}",
        restored.blocker
    );
    let prepared = restored
        .result
        .ok_or_else(|| anyhow!("missing conversation restore result"))?;
    let restored_metadata = prepared
        .metadata
        .as_ref()
        .ok_or_else(|| anyhow!("missing exact session checkpoint"))?;
    assert_eq!(restored_metadata.id, cache_id);
    assert_eq!(
        restored_metadata.owner_id.as_deref(),
        Some(conversation.id.as_str())
    );
    assert_eq!(
        prepared.prefill_us, 0,
        "restart must not reprocess the prompt"
    );
    assert_eq!(
        prepared.resident_prefix_tokens,
        checkpoint.cells.len().max(checkpoint.token_ids.len())
    );
    assert!(prepared.resident_prefix_tokens > 0);
    println!(
        "SHUTDOWN_CHECKPOINT_RESTORED {}",
        json!({
            "readiness": restored.readiness,
            "cache_id": restored_metadata.id,
            "resident_prefix_tokens": prepared.resident_prefix_tokens,
            "prefill_us": prepared.prefill_us,
            "path": "conversation-scoped authenticated native restore"
        })
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_product_native_chat_cache_reuses_clears_and_disables_without_fte() -> Result<()> {
    let session = configured_real_session("real-product-native-chat-cache")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        // Exercise the product backend on Apple Silicon; other platforms stay CPU.
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        max_tokens: Some(1),
        context_tokens: Some(2048),
        batch_tokens: Some(128),
        // Match the application profile so cancelled turns exercise the
        // alternate-sequence resident audio checkpoint used for fast resume.
        max_parallel_sequences: Some(4),
        kv_cache_policy: Some(KvCachePolicy::KvCacheCandidate),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let conversation = mom_llama_runtime::conversation_new(Some("Native cache proof".to_owned()))?
        .result
        .ok_or_else(|| anyhow!("cache proof conversation missing"))?;

    let cold = product_chat_cache_request(&scope, &conversation.id, "Return the word cold.")?;
    assert!(
        !cold.cache_reused,
        "the first direct chat request must be cold"
    );
    assert!(
        encrypted_document_snapshot(session.path())?
            .keys()
            .any(|key| key.starts_with("kv-cache.v3.blob.")),
        "cold generation must persist a real encrypted checkpoint"
    );
    let warm = product_chat_cache_request(&scope, &conversation.id, "Return the word warm.")?;
    eprintln!(
        "VOICE_TEXT_CACHE_POLICY_WARM {}",
        serde_json::to_string(&json!({
            "cold_reused": cold.cache_reused,
            "cold_metrics": cold.cache_metrics,
            "warm_reused": warm.cache_reused,
            "warm_metrics": warm.cache_metrics,
            "warm_cache_id": warm.cache_id,
        }))?
    );
    assert!(
        warm.cache_reused,
        "the second direct chat request must reuse its prefix"
    );

    let cleared = mom_llama_runtime::kv_cache_clear(&scope)?;
    assert_eq!(cleared.readiness, "contracted");
    assert!(cleared.blocker.is_none());
    let after_clear =
        product_chat_cache_request(&scope, &conversation.id, "Return the words after clear.")?;
    assert_eq!(
        after_clear.cache_id, None,
        "explicit clear must revoke the product-managed cache entry"
    );
    if after_clear.cache_reused {
        assert!(
            after_clear.cache_metrics.resident_prefix_tokens > 0,
            "a post-clear hit must come only from exact cells still resident in the worker"
        );
    }
    let warm_after_clear =
        product_chat_cache_request(&scope, &conversation.id, "Return the words after warmup.")?;
    assert!(warm_after_clear.cache_reused);

    mom_llama_runtime::kv_cache_clear(&scope)?;
    drop(native_owner);
    mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::None),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let off_first =
        product_chat_cache_request(&scope, &conversation.id, "Caching is off, first request.")?;
    let off_second =
        product_chat_cache_request(&scope, &conversation.id, "Caching is off, second request.")?;
    assert_eq!(off_first.cache_id, None);
    assert_eq!(off_second.cache_id, None);
    // Off removes the product-managed prefix tier. The native worker can still
    // use token-exact cells from the immediately preceding request, and the
    // public cache flag must report that physical reuse honestly.
    assert_eq!(
        off_second.cache_reused,
        off_second.cache_metrics.resident_prefix_tokens > 0
            || off_second.cache_metrics.restored_prefix_tokens > 0
            || off_second.cache_metrics.batch_shared_prefix_tokens > 0
    );
    assert!(
        !encrypted_document_snapshot(session.path())?
            .contains_key("native-host-prefix-cache.mom-llama"),
        "the Off policy must not write the product Native prefix document"
    );
    Ok(())
}

fn one_second_tone_wav() -> Vec<u8> {
    const SAMPLE_RATE: u32 = 16_000;
    const SAMPLE_COUNT: u32 = SAMPLE_RATE;
    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    let data_bytes = SAMPLE_COUNT * u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in 0..SAMPLE_COUNT {
        let phase = sample as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32;
        let value = (phase.sin() * 8_000.0) as i16;
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav
}

fn pcm16_mono_16khz_wav_samples(path: &Path) -> Result<Vec<f32>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read benchmark WAV {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "benchmark audio must be a RIFF/WAVE file"
    );
    let mut cursor = 12usize;
    let mut format = None;
    let mut data = None;
    while cursor.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_len = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into()?) as usize;
        let chunk_start = cursor + 8;
        let chunk_end = chunk_start
            .checked_add(chunk_len)
            .ok_or_else(|| anyhow!("benchmark WAV chunk length overflow"))?;
        anyhow::ensure!(chunk_end <= bytes.len(), "truncated benchmark WAV chunk");
        match chunk_id {
            b"fmt " if chunk_len >= 16 => {
                format = Some((
                    u16::from_le_bytes(bytes[chunk_start..chunk_start + 2].try_into()?),
                    u16::from_le_bytes(bytes[chunk_start + 2..chunk_start + 4].try_into()?),
                    u32::from_le_bytes(bytes[chunk_start + 4..chunk_start + 8].try_into()?),
                    u16::from_le_bytes(bytes[chunk_start + 14..chunk_start + 16].try_into()?),
                ));
            }
            b"data" => data = Some(&bytes[chunk_start..chunk_end]),
            _ => {}
        }
        cursor = chunk_end + (chunk_len & 1);
    }
    let (encoding, channels, sample_rate, bits) =
        format.ok_or_else(|| anyhow!("benchmark WAV has no fmt chunk"))?;
    anyhow::ensure!(
        (encoding, channels, sample_rate, bits) == (1, 1, 16_000, 16),
        "benchmark WAV must be PCM16 mono 16 kHz; got encoding={encoding}, channels={channels}, sample_rate={sample_rate}, bits={bits}"
    );
    let data = data.ok_or_else(|| anyhow!("benchmark WAV has no data chunk"))?;
    anyhow::ensure!(data.len() % 2 == 0, "benchmark WAV has a partial sample");
    Ok(data
        .chunks_exact(2)
        .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32_768.0)
        .collect())
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn resident_model_profiles_fail_closed_before_memory_overcommit_without_eviction() -> Result<()> {
    use std::os::unix::fs::symlink;

    let session = configured_real_session("real-resident-memory-budget")?;
    let model_path = std::env::var_os("MOM_LLAMA_MODEL_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("real model path disappeared"))?;
    let model_bytes = std::fs::metadata(&model_path)?.len();
    let runtime_reserve = (model_bytes / 2).max(384 * 1024 * 1024);
    let one_model_budget = model_bytes
        .saturating_add(runtime_reserve)
        .saturating_add(1024 * 1024);
    mom_llama_runtime::settings_update(SettingsUpdate {
        resident_memory_budget_bytes: Some(one_model_budget),
        max_parallel_sequences: Some(2),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let settings = mom_llama_runtime::config::resolve_settings()?;
    let first = mom_llama_runtime::resident_model_for_profile(&scope, &settings, &model_path, None)
        .map_err(|blocked| anyhow!(blocked.blocker.message))?;
    let first_model_id = first.status().model_id;

    let second_profile_path = session.path().join("same-weights-distinct-profile.gguf");
    symlink(&model_path, &second_profile_path)?;
    let blocked = mom_llama_runtime::resident_model_for_profile(
        &scope,
        &settings,
        &second_profile_path,
        None,
    )
    .expect_err("the second resident profile must be rejected before overcommit");
    assert_eq!(blocked.readiness, "blocked_memory_budget");
    assert_eq!(
        blocked.blocker.code,
        "resident_model_memory_budget_exceeded"
    );
    assert_eq!(
        mom_llama_runtime::resident_status(&scope,).map(|status| status.model_id),
        Some(first_model_id),
        "a rejected profile must never evict the already resident model"
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn repeated_profile_acquisition_reuses_the_same_resident_worker() -> Result<()> {
    let _session = configured_real_session("real-resident-worker-reuse")?;
    let model_path = std::env::var_os("MOM_LLAMA_MODEL_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("real model path disappeared"))?;
    let settings = mom_llama_runtime::config::resolve_settings()?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();

    let first = mom_llama_runtime::resident_model_for_profile(&scope, &settings, &model_path, None)
        .map_err(|blocked| anyhow!(blocked.blocker.message))?;
    let second =
        mom_llama_runtime::resident_model_for_profile(&scope, &settings, &model_path, None)
            .map_err(|blocked| anyhow!(blocked.blocker.message))?;

    assert!(
        first.is_same_worker(&second),
        "reacquiring an unchanged profile must reuse its resident worker"
    );
    assert_eq!(
        mom_llama_runtime::native_runtime::resident_slots(&scope,).len(),
        1
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local base GGUF"]
fn real_native_base_completion_invokes_no_fixture() -> Result<()> {
    let _session = configured_real_session("real-base-completion")?;
    let model_path = std::env::var_os("MOM_LLAMA_MODEL_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("real model path disappeared"))?;
    let settings = mom_llama_runtime::config::resolve_settings()?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let handle =
        mom_llama_runtime::resident_model_for_profile(&scope, &settings, &model_path, None)
            .map_err(|blocked| anyhow!(blocked.blocker.message))?;
    let output = handle
        .generate(NativeGenerationRequest {
            request_id: format!("real-base-completion-{}", uuid::Uuid::new_v4()),
            model_id: handle.status().model_id,
            input: NativeGenerationInput::Completion {
                prompts: vec![CompletionPrompt::Text {
                    text: "At dawn, the locked observatory began to breathe.".to_string(),
                    special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
                }],
            },
            sampling: SamplingConfig {
                seed: 17,
                temperature: 0.8,
                max_tokens: 16,
                ..SamplingConfig::default()
            },
            media: Vec::new(),
            cached_prefix: None,
        })
        .map_err(|error| anyhow!(error))?
        .wait()
        .map_err(|error| anyhow!(error))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("native base completion returned no output"))?;
    assert_eq!(output.state, GenerationState::Completed);
    assert!(output.real_engine_invoked);
    assert!(!output.fake_fixture);
    assert!(output.metrics.completion_tokens > 0);
    assert!(!output.text.trim().is_empty());
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_chat_invokes_no_fixture_and_persists() -> Result<()> {
    let _session = configured_real_session("real-chat")?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let result = mom_llama_runtime::chat_send_in_scope(
        &scope,
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
#[ignore = "requires a real multimodal GGUF, matching mmproj, and local image"]
fn real_native_multimodal_image_and_audio_use_loaded_projector_and_encrypted_bytes() -> Result<()> {
    let Some(model_path) = std::env::var_os("MOM_LLAMA_MULTIMODAL_MODEL_PATH").map(PathBuf::from)
    else {
        return Ok(());
    };
    let Some(mmproj_path) = std::env::var_os("MOM_LLAMA_MM_PROJ_PATH").map(PathBuf::from) else {
        return Ok(());
    };
    let Some(image_path) = std::env::var_os("MOM_LLAMA_TEST_IMAGE_PATH").map(PathBuf::from) else {
        return Ok(());
    };
    if !model_path.is_file() || !mmproj_path.is_file() || !image_path.is_file() {
        return Ok(());
    }

    let session = TestSession::new("real-multimodal-image")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(Some(model_path)),
        mmproj_path: Some(Some(mmproj_path)),
        native_device: Some(llama_native_types::NativeDevice::Cpu),
        max_tokens: Some(4),
        context_tokens: Some(4096),
        resident_memory_budget_bytes: Some(12 * 1024 * 1024 * 1024),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let image_bytes = std::fs::read(&image_path)?;
    let imported =
        mom_llama_runtime::attachment_import(&scope, "real-multimodal-image", &image_path)?;
    let attachment = imported
        .result
        .as_ref()
        .map(|output| output.attachment.clone())
        .ok_or_else(|| anyhow!("multimodal attachment import missing"))?;
    assert!(attachment.stored_path.starts_with("encrypted://"));

    let result = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: "real-multimodal-image".to_string(),
            message: "Describe the attached image in one short sentence.".to_string(),
        },
        ChatSendOptions {
            timeout_s: 600.0,
            fake_fixture: false,
        },
    )?;
    assert_eq!(
        result.readiness, "real_prompt_smoke_passed",
        "multimodal blocker: {:#?}",
        result.blocker
    );
    assert!(result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    assert!(result.result.as_ref().is_some_and(|output| {
        !output.assistant_text.trim().is_empty() && output.completion_tokens > 0
    }));
    let status = mom_llama_runtime::resident_status(&scope)
        .ok_or_else(|| anyhow!("resident multimodal model status missing"))?;
    assert!(
        status
            .fingerprint
            .as_ref()
            .is_some_and(|fingerprint| { fingerprint.multimodal_projector_sha256.is_some() })
    );
    let preview_catalog = mom_llama_runtime::attachment_preview(&attachment.id)?
        .result
        .ok_or_else(|| anyhow!("multimodal preview catalog missing"))?;
    let preview =
        mom_llama_runtime::attachment_preview_media(&primary_preview_anchor(&preview_catalog)?)?
            .map_err(|blocker| anyhow!("{}: {}", blocker.code, blocker.message))?;
    assert_eq!(preview.bytes.as_slice(), image_bytes.as_slice());
    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(image_bytes.len())
            .any(|window| window == image_bytes)
    );

    let audio_bytes = one_second_tone_wav();
    let audio_path = std::env::temp_dir().join(format!(
        "mom-llama-real-tone-{}.wav",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&audio_path, &audio_bytes)?;
    let audio_import =
        mom_llama_runtime::attachment_import(&scope, "real-multimodal-audio", &audio_path);
    std::fs::remove_file(&audio_path)?;
    let audio_attachment = audio_import?
        .result
        .as_ref()
        .map(|output| output.attachment.clone())
        .ok_or_else(|| anyhow!("multimodal audio attachment import missing"))?;
    assert!(audio_attachment.stored_path.starts_with("encrypted://"));
    let audio_result = mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: "real-multimodal-audio".to_string(),
            message: "Reply with whether the attached audio is a tone or silence.".to_string(),
        },
        ChatSendOptions {
            timeout_s: 600.0,
            fake_fixture: false,
        },
    )?;
    assert_eq!(
        audio_result.readiness, "real_prompt_smoke_passed",
        "multimodal audio blocker: {:#?}",
        audio_result.blocker
    );
    assert!(audio_result.receipt.real_engine_invoked);
    assert!(!audio_result.receipt.fake_fixture);
    assert!(audio_result.result.as_ref().is_some_and(|output| {
        !output.assistant_text.trim().is_empty() && output.completion_tokens > 0
    }));
    let audio_catalog = mom_llama_runtime::attachment_preview(&audio_attachment.id)?
        .result
        .ok_or_else(|| anyhow!("multimodal audio preview catalog missing"))?;
    let audio_preview =
        mom_llama_runtime::attachment_preview_media(&primary_preview_anchor(&audio_catalog)?)?
            .map_err(|blocker| anyhow!("{}: {}", blocker.code, blocker.message))?;
    assert_eq!(audio_preview.bytes.as_slice(), audio_bytes.as_slice());
    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(audio_bytes.len())
            .any(|window| window == audio_bytes)
    );
    Ok(())
}

#[test]
#[ignore = "requires the real audio model, matching projector, and a PCM16 mono 16 kHz WAV"]
fn real_incremental_voice_prefill_reports_silence_to_first_token() -> Result<()> {
    let model_path = std::env::var_os("MOM_LLAMA_MULTIMODAL_MODEL_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("MOM_LLAMA_MULTIMODAL_MODEL_PATH must point to a real GGUF"))?;
    let mmproj_path = std::env::var_os("MOM_LLAMA_MM_PROJ_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("MOM_LLAMA_MM_PROJ_PATH must point to its audio projector"))?;
    let wav_path = std::env::var_os("MOM_LLAMA_TEST_VOICE_WAV")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("MOM_LLAMA_TEST_VOICE_WAV must point to a local WAV"))?;
    let samples = pcm16_mono_16khz_wav_samples(&wav_path)?;
    let samples = &samples[..samples.len().min(4 * 16_000)];
    anyhow::ensure!(
        samples.len() >= 640,
        "benchmark WAV must contain at least one 640-sample projector frame"
    );

    let _session = TestSession::new("real-incremental-voice-latency")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(Some(model_path)),
        mmproj_path: Some(Some(mmproj_path)),
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        max_tokens: Some(32),
        batch_tokens: Some(128),
        // Match the packaged call runtime. The fourth resident lane retains
        // the token-exact text prefix while the first two lanes ping-pong
        // incremental audio checkpoints.
        max_parallel_sequences: Some(4),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let cold_warm = mom_llama_runtime::runtime_warm(&scope)?;
    anyhow::ensure!(
        cold_warm
            .result
            .as_ref()
            .is_some_and(|warm| warm.model_loaded && warm.audio_projector_warmed),
        "cold runtime warm did not load and compile the voice model: result={:#?}, blocker={:#?}",
        cold_warm.result,
        cold_warm.blocker,
    );
    drop(scope);
    drop(native_owner);
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let cached_warm = mom_llama_runtime::runtime_warm(&scope)?;
    let cached_load = cached_warm
        .result
        .as_ref()
        .and_then(|warm| warm.model_load.clone());
    println!(
        "VOICE_ARTIFACT_CACHE_BENCHMARK {}",
        serde_json::to_string(&json!({
            "model_acquire_us": cached_warm.result.as_ref().map(|warm| warm.model_acquire_us),
            "audio_projector_warm_us": cached_warm.result.as_ref().map(|warm| warm.audio_projector_warm_us),
            "audio_projector_warmed": cached_warm.result.as_ref().map(|warm| warm.audio_projector_warmed),
            "model_load": cached_load,
        }))?
    );
    anyhow::ensure!(
        cached_warm
            .result
            .as_ref()
            .is_some_and(|warm| warm.model_loaded && warm.audio_projector_warmed),
        "verified artifact cache did not reload and compile the voice model: result={:#?}, blocker={:#?}",
        cached_warm.result,
        cached_warm.blocker,
    );
    let conversation = mom_llama_runtime::conversation_new(Some("Voice latency benchmark".into()))?
        .result
        .ok_or_else(|| anyhow!("voice latency benchmark conversation missing"))?;

    // Exercise the application lifecycle boundary that live calls use when
    // resumed speech (or hang-up) abandons a speculative incremental turn.
    // The next voice note must be admitted against the same resident model
    // even when cancellation lands after the projector has mutated KV state.
    let abandoned = mom_llama_runtime::voice_audio_begin_in_scope(
        &scope,
        &conversation.id,
        "[Phone call] Reply briefly and naturally to what you hear.",
    )?;
    let abandoned_preparation = abandoned.prepared_turn();
    abandoned.wait_for_resume()?;
    for chunk in samples[..16_000].chunks(160) {
        abandoned.push_pcm(chunk)?;
    }
    let abandon_deadline = Instant::now() + Duration::from_secs(5);
    while abandoned.audio_monitor().stats().projected_blocks == 0 {
        anyhow::ensure!(
            Instant::now() < abandon_deadline,
            "abandoned voice turn never projected its admitted PCM"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let cancellation_started = Instant::now();
    abandoned.cancel();
    drop(abandoned);
    let resumed =
        mom_llama_runtime::voice_audio_restart_prepared_in_scope(&scope, abandoned_preparation)?;
    let resume = resumed.wait_for_resume()?;
    let cancellation_to_resume_us = cancellation_started
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    let model_load = mom_llama_runtime::resident_status(&scope)
        .and_then(|status| status.descriptor)
        .map(|descriptor| descriptor.load_metrics);
    println!(
        "VOICE_RESUME_BENCHMARK {}",
        serde_json::to_string(&json!({
            "reused": resume.reused,
            "sample_offset": resume.sample_offset,
            "resident_max_sequences": resume.resident_max_sequences,
            "checkpoint_supported": resume.checkpoint_supported,
            "checkpoint_available": resume.checkpoint_available,
            "resume_key_matched": resume.resume_key_matched,
            "request_matched": resume.request_matched,
            "binding_us": resume.binding_us,
            "restore_us": resume.restore_us,
            "cancellation_to_resume_us": cancellation_to_resume_us,
            "model_load": model_load,
        }))?
    );
    anyhow::ensure!(
        resume.reused
            && resume.sample_offset > 0
            && resume.sample_offset <= 16_000
            && resume.sample_offset % 640 == 0,
        "cancelled voice checkpoint was not reused exactly: {resume:?}"
    );
    for chunk in samples[resume.sample_offset..16_000].chunks(160) {
        resumed.push_pcm(chunk)?;
    }
    resumed.seal_input()?;
    let resumed_output = mom_llama_runtime::voice_audio_finish_in_scope(
        resumed,
        ChatSendOptions {
            timeout_s: 600.0,
            fake_fixture: false,
        },
        None::<fn(mom_llama_runtime::ChatStreamEvent) -> anyhow::Result<()>>,
    )?;
    anyhow::ensure!(
        resumed_output.result.as_ref().is_some_and(|output| {
            !output.assistant_text.trim().is_empty()
                && output.voice_audio_metrics.as_ref().is_some_and(|audio| {
                    resume.sample_offset
                        + usize::try_from(audio.projected_samples).unwrap_or(usize::MAX)
                        == 16_000
                })
        }),
        "resumed voice turn did not replay its exact tail and complete: result={:#?}, blocker={:#?}",
        resumed_output.result,
        resumed_output.blocker,
    );

    // Short acknowledgements are the hardest overlap case: prompt-prefix
    // evaluation has much less microphone time in which to finish before the
    // silence edge. Exercise both that case and an ordinary four-second turn.
    let short_samples = &samples[..samples.len().min(8_000)];
    for (turn, turn_samples) in [(1, short_samples), (2, samples)] {
        let begin_wall = Instant::now();
        // The packaged Tauri command admits every microphone turn on a fresh
        // supervised blocking worker. Exercise that thread boundary here as
        // well: a direct test-thread begin would miss thread-local or unwind
        // failures at the exact boundary that previously failed in product.
        let begin_scope = scope.clone();
        let begin_conversation = conversation.id.clone();
        // The isolated test store override is intentionally thread-local so
        // parallel tests cannot collide. Product workers all resolve the same
        // application data directory; explicitly carry this test-only value
        // across the worker boundary to preserve that product invariant.
        let begin_data_dir = _session.path().to_path_buf();
        let voice = std::thread::Builder::new()
            .name(format!("voice-prefill-benchmark-{turn}"))
            .spawn(move || {
                set_data_dir_override_for_tests(Some(begin_data_dir));
                let result = mom_llama_runtime::voice_audio_begin_in_scope(
                    &begin_scope,
                    &begin_conversation,
                    "[Phone call] Reply briefly and naturally to what you hear.",
                );
                set_data_dir_override_for_tests(None);
                result
            })
            .map_err(|error| anyhow!("voice begin worker could not start: {error}"))?
            .join()
            .map_err(|_| anyhow!("voice begin worker panicked"))??;
        let begin_wall_ms = begin_wall.elapsed().as_millis();
        let begin_metrics = voice.begin_metrics().clone();
        let monitor = voice.audio_monitor();
        for chunk in turn_samples.chunks(160) {
            voice.push_pcm(chunk)?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let silence_wall = Instant::now();
        voice.seal_input()?;
        let mut first_delta_wall_ms = None;
        let finished = mom_llama_runtime::voice_audio_finish_in_scope(
            voice,
            ChatSendOptions {
                timeout_s: 600.0,
                fake_fixture: false,
            },
            Some(|event: mom_llama_runtime::ChatStreamEvent| {
                if matches!(event.event.as_str(), "delta" | "reasoning_delta")
                    && let Some(delta) = event.delta.as_deref()
                    && !delta.is_empty()
                {
                    println!(
                        "VOICE_CONTENT_DELTA_BENCHMARK {}",
                        serde_json::to_string(&json!({
                            "turn": turn,
                            "silence_elapsed_ms": silence_wall.elapsed().as_millis(),
                            "event": event.event,
                            "delta": delta,
                        }))?
                    );
                }
                if first_delta_wall_ms.is_none()
                    && matches!(event.event.as_str(), "delta" | "reasoning_delta")
                {
                    let elapsed_ms = silence_wall.elapsed().as_millis();
                    first_delta_wall_ms = Some(elapsed_ms);
                    println!(
                        "VOICE_FIRST_TOKEN_BENCHMARK {}",
                        serde_json::to_string(&json!({
                            "turn": turn,
                            "silence_to_first_token_ms": elapsed_ms,
                        }))?
                    );
                }
                Ok(())
            }),
        )?;
        let output = finished.result.ok_or_else(|| {
            anyhow!(
                "voice benchmark turn {turn} blocked: {:#?}",
                finished.blocker
            )
        })?;
        let audio = output
            .voice_audio_metrics
            .as_ref()
            .ok_or_else(|| anyhow!("voice benchmark turn {turn} omitted audio metrics"))?;
        let live = monitor.stats();
        let silence_to_first_token_ms = first_delta_wall_ms.or_else(|| {
            output
                .first_token_ms
                .and_then(|first| first.checked_sub(u128::from(audio.finish_ms.unwrap_or(0))))
        });
        let prefix_receipt = json!({
            "start_us": audio.prefix_start_us,
            "finish_us": audio.prefix_finish_us,
            "duration_us": audio.prefix_us,
            "tokens": audio.prefix_tokens,
            "restored_tokens": audio.restored_prefix_tokens,
        });
        println!(
            "VOICE_LATENCY_BENCHMARK {}",
            serde_json::to_string(&json!({
                "turn": turn,
                "audio_ms": turn_samples.len() as f64 / 16.0,
                "begin_wall_ms": begin_wall_ms,
                "resident_context_tokens": begin_metrics.resident_context_tokens,
                "settings_cache_hit": begin_metrics.settings_cache_hit,
                "model_resident_hit": begin_metrics.model_resident_hit,
                "prepared_reuse": begin_metrics.prepared_reuse,
                "begin_total_us": begin_metrics.total_us,
                "model_acquire_us": begin_metrics.model_acquire_us,
                "generation_admission_us": begin_metrics.generation_admission_us,
                "first_accept_us": audio.first_accept_us,
                "first_read_us": audio.first_read_us,
                "first_projector_start_us": audio.first_projector_start_us,
                "first_projector_finish_us": audio.first_projector_finish_us,
                "last_projector_finish_us": audio.last_projector_finish_us,
                "finish_us": audio.finish_us,
                "decode_start_us": audio.decode_start_us,
                "prefix": prefix_receipt,
                "first_token_ms_from_engine_start": output.first_token_ms,
                "silence_to_first_token_ms": silence_to_first_token_ms,
                "silence_to_finish_ms": silence_wall.elapsed().as_millis(),
                "projector_us": audio.projector_us,
                "projector_max_us": audio.projector_max_us,
                "projected_blocks": audio.projected_blocks,
                "projected_samples": audio.projected_samples,
                "tail_samples": audio.tail_samples,
                "producer_lock_wait_us": audio.producer_lock_wait_us,
                "producer_lock_wait_max_us": audio.producer_lock_wait_max_us,
                "ring_write_us": audio.ring_write_us,
                "ring_write_max_us": audio.ring_write_max_us,
                "consumer_lock_wait_us": audio.consumer_lock_wait_us,
                "consumer_lock_wait_max_us": audio.consumer_lock_wait_max_us,
                "ring_read_us": audio.ring_read_us,
                "ring_read_max_us": audio.ring_read_max_us,
                "reader_notifications": live.reader_notifications,
                "reader_waits": live.reader_waits,
                "reader_wakeups": live.reader_wakeups,
                "completion_tokens": output.completion_tokens,
                "duration_ms": output.duration_ms,
                "assistant_text": output.assistant_text,
            }))?
        );
        anyhow::ensure!(
            audio.projected_blocks > 0,
            "turn {turn} projected no audio blocks"
        );
        anyhow::ensure!(
            silence_to_first_token_ms.is_some(),
            "turn {turn} produced no observable first token"
        );
        anyhow::ensure!(
            audio.restored_prefix_tokens > 0,
            "turn {turn} did not reuse the resident text prefix"
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_REASONING_MODEL_PATH pointing at a reasoning-capable GGUF"]
fn real_native_reasoning_stream_can_be_forced_to_the_answer() -> Result<()> {
    let Some(model_path) = std::env::var_os("MOM_LLAMA_REASONING_MODEL_PATH").map(PathBuf::from)
    else {
        return Ok(());
    };
    if !model_path.is_file() {
        return Ok(());
    }
    let _session = TestSession::new("real-reasoning-control")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: Some(Some(model_path)),
        native_device: Some(llama_native_types::NativeDevice::Cpu),
        max_tokens: Some(192),
        upstream_settings: Some(BTreeMap::from([(
            "excludeReasoningFromContext".to_string(),
            json!(false),
        )])),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let mut reasoning_preview = String::new();
    let mut skip_result = None;
    let result = mom_llama_runtime::chat_send_stream_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: "real-reasoning-control".to_string(),
            message: "Begin your response with the exact token <think>. Inside that block, repeat the word reasoning many times before you would close it. After the block, answer READY.".to_string(),
        },
        ChatSendOptions::default(),
        |event| {
            if event.event == "reasoning_delta" {
                if let Some(delta) = event.delta.as_deref() {
                    reasoning_preview.push_str(delta);
                }
                if skip_result.is_none() && reasoning_preview.trim().chars().count() >= 12 {
                    skip_result = Some(mom_llama_runtime::chat_skip_reasoning_in_scope(
                        &scope,
                        "real-reasoning-control",
                    )?);
                }
            }
            Ok(())
        },
    )?;
    assert!(
        !reasoning_preview.trim().is_empty(),
        "the real model did not emit substantive reasoning"
    );
    let skip = skip_result.ok_or_else(|| anyhow!("skip-reasoning result missing"))?;
    assert_ne!(skip.status, "blocked");
    assert_eq!(result.readiness, "real_prompt_smoke_passed");
    assert!(result.receipt.real_engine_invoked);
    let output = result
        .result
        .ok_or_else(|| anyhow!("reasoning output missing"))?;
    assert!(
        output
            .reasoning_content
            .as_deref()
            .is_some_and(|reasoning| !reasoning.trim().is_empty())
    );
    assert!(!output.reasoning_incomplete);
    assert!(!output.assistant_text.contains("</think>"));
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_persona_mentions_reuse_only_the_exact_versioned_prefix() -> Result<()> {
    let _session = configured_real_session("real-persona-cache")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(48),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let source = mom_llama_runtime::conversation_new(Some("Cache source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("cache source missing"))?;
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "Keep this frozen source context exact.".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .ok_or_else(|| anyhow!("cache source reload missing"))?;
    let persona = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: source
            .active_leaf_message_id
            .clone()
            .ok_or_else(|| anyhow!("cache source leaf missing"))?,
        name: "Cache witness".to_string(),
        mention_handle: "cache-witness".to_string(),
        history_mode: PersonaHistoryMode::Full,
    })?
    .result
    .ok_or_else(|| anyhow!("cache persona missing"))?;
    let host = mom_llama_runtime::conversation_new(Some("Cache host".to_string()))?
        .result
        .ok_or_else(|| anyhow!("cache host missing"))?;

    let dispatch = |scope: &mom_llama_runtime::OperationScope,
                    message: &str|
     -> Result<mom_llama_runtime::MentionInvocation> {
        let result = mom_llama_runtime::chat_dispatch_in_scope(
            scope,
            MentionDispatchInput {
                conversation_id: host.id.clone(),
                message: format!("@cache-witness {message}"),
            },
            ChatSendOptions::default(),
        )?;
        assert_eq!(result.readiness, "real_prompt_smoke_passed");
        assert!(result.receipt.real_engine_invoked);
        let ChatDispatchOutput::Mention { invocation, .. } = result
            .result
            .ok_or_else(|| anyhow!("mention output missing"))?
        else {
            return Err(anyhow!("expected a mention invocation"));
        };
        Ok(invocation)
    };

    let first = dispatch(&scope, "Answer in one short sentence.")?;
    let first_result = first
        .results
        .first()
        .ok_or_else(|| anyhow!("first persona result missing"))?;
    assert!(!first_result.cache_reused);
    let first_cache_id = first_result
        .cache_id
        .clone()
        .ok_or_else(|| anyhow!("first persona cache id missing"))?;

    let second = dispatch(&scope, "Give another short answer.")?;
    let second_result = second
        .results
        .first()
        .ok_or_else(|| anyhow!("second persona result missing"))?;
    assert!(second_result.cache_reused);
    assert_eq!(
        second_result.cache_id.as_deref(),
        Some(first_cache_id.as_str())
    );

    drop(native_owner);
    mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::None),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let disabled = dispatch(&scope, "Answer with prompt caching explicitly disabled.")?;
    let disabled_result = disabled
        .results
        .first()
        .ok_or_else(|| anyhow!("disabled-cache persona result missing"))?;
    assert!(!disabled_result.cache_reused);
    assert!(disabled_result.cache_id.is_none());
    assert_eq!(
        mom_llama_runtime::kv_cache_status()?
            .result
            .ok_or_else(|| anyhow!("disabled-cache status missing"))?
            .status,
        mom_llama_runtime::kv_cache::KvCacheState::Disabled
    );
    drop(native_owner);
    mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;

    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let persona_before = mom_llama_runtime::persona_get(&persona.id)?
        .result
        .ok_or_else(|| anyhow!("persona vanished"))?;
    let edit_target = persona_before
        .active_leaf_message_id
        .clone()
        .ok_or_else(|| anyhow!("persona edit target missing"))?;
    mom_llama_runtime::message_edit(
        &persona.id,
        &edit_target,
        "Revised frozen source context.".to_string(),
    )?;
    let third = dispatch(&scope, "Answer after the explicit persona revision.")?;
    assert_eq!(third.targets[0].version, 2);
    let third_result = third
        .results
        .first()
        .ok_or_else(|| anyhow!("third persona result missing"))?;
    assert!(!third_result.cache_reused);
    assert_ne!(
        third_result.cache_id.as_deref(),
        Some(first_cache_id.as_str())
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_four_persona_group_cancels_one_target_without_touching_sources() -> Result<()> {
    let session = configured_real_session("real-persona-group-cancel")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        max_tokens: Some(64),
        context_tokens: Some(2048),
        batch_tokens: Some(128),
        max_parallel_sequences: Some(4),
        // This scenario verifies independent execution, cancellation, and
        // source ownership; reasoning extraction has its own model scenarios.
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let operation_scope = native_owner.operation_scope();
    let scope = operation_scope.clone();
    let source = mom_llama_runtime::conversation_new(Some("Group source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("group source missing"))?;
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "Immutable source material for four views.".to_string(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source_before = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .ok_or_else(|| anyhow!("group source reload missing"))?;
    let leaf = source_before
        .active_leaf_message_id
        .clone()
        .ok_or_else(|| anyhow!("group source leaf missing"))?;
    let mut persona_ids = Vec::new();
    for index in 0..4 {
        let persona = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
            conversation_id: source.id.clone(),
            message_id: leaf.clone(),
            name: format!("Group lens {}", index + 1),
            mention_handle: format!("group-lens-{}", index + 1),
            history_mode: PersonaHistoryMode::Full,
        })?
        .result
        .ok_or_else(|| anyhow!("group persona missing"))?;
        persona_ids.push(persona.id);
    }
    let cancelled_persona_id = persona_ids[3].clone();
    let group = mom_llama_runtime::persona_group_create(
        "Four lenses".to_string(),
        "four-lenses".to_string(),
        persona_ids.clone(),
    )?
    .result
    .ok_or_else(|| anyhow!("persona group missing"))?;
    let host = mom_llama_runtime::conversation_new(Some("Group host".to_string()))?
        .result
        .ok_or_else(|| anyhow!("group host missing"))?;
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let host_id = host.id.clone();
    let dispatch_scope = operation_scope.clone();
    let dispatch_data_dir = session.path().to_path_buf();
    let handle = std::thread::spawn(move || {
        set_data_dir_override_for_tests(Some(dispatch_data_dir));
        mom_llama_runtime::chat_dispatch_stream_in_scope(
            &dispatch_scope,
            MentionDispatchInput {
                conversation_id: host_id,
                message: format!("@{} offer four concise views", group.mention_handle),
            },
            ChatSendOptions::default(),
            Some(move |event: mom_llama_runtime::ChatDispatchStreamEvent| {
                if let mom_llama_runtime::ChatDispatchStreamEvent::Mention(event) = event
                    && event.event == "delta"
                {
                    let _ = started_tx.try_send(event.invocation_id);
                }
                Ok(())
            }),
        )
    });
    let invocation_id = match started_rx.recv_timeout(Duration::from_mins(2)) {
        Ok(id) => id,
        Err(error) => {
            operation_scope.request_cancellation();
            let result = handle
                .join()
                .map_err(|_| anyhow!("persona group dispatch panicked"))??;
            return Err(anyhow!(
                "persona group did not start streaming: {error}; {}: {:?}",
                result.readiness,
                result.blocker
            ));
        }
    };
    let cancelled = mom_llama_runtime::mention_cancel_in_scope(
        &operation_scope,
        &invocation_id,
        Some(&cancelled_persona_id),
    )?;
    assert_eq!(
        cancelled
            .result
            .as_ref()
            .map(|output| output.cancelled_sequences),
        Some(1)
    );
    let result = handle
        .join()
        .map_err(|_| anyhow!("persona group dispatch panicked"))??;
    let ChatDispatchOutput::Mention { invocation, .. } = result
        .result
        .ok_or_else(|| anyhow!("group output missing"))?
    else {
        return Err(anyhow!("expected group mention output"));
    };
    assert_eq!(invocation.targets.len(), 4);
    assert_eq!(invocation.results.len(), 4);
    assert!(invocation.results.iter().any(|target| {
        target.target_id == cancelled_persona_id
            && target.state == llama_native_types::GenerationState::Cancelled
    }));
    assert!(
        invocation
            .results
            .iter()
            .filter(|target| target.state == llama_native_types::GenerationState::Completed)
            .count()
            >= 2
    );
    let synthesis = mom_llama_runtime::mention_synthesize(&scope, &invocation.id)?;
    assert_eq!(synthesis.readiness, "real_prompt_smoke_passed");
    assert!(synthesis.receipt.real_engine_invoked);
    let synthesis = synthesis
        .result
        .ok_or_else(|| anyhow!("mention synthesis missing"))?;
    assert!(synthesis.source_message_ids.len() >= 2);
    let host_after_synthesis = mom_llama_runtime::conversation_select(&host.id)?
        .result
        .ok_or_else(|| anyhow!("group host vanished after synthesis"))?;
    let synthesis_message = host_after_synthesis
        .messages
        .iter()
        .find(|message| message.id == synthesis.message_id)
        .ok_or_else(|| anyhow!("synthesis message not persisted"))?;
    assert_eq!(
        synthesis_message
            .attribution
            .as_ref()
            .map(|attribution| attribution.kind),
        Some(mom_llama_runtime::MessageSpeakerKind::Synthesis)
    );
    assert!(
        host_after_synthesis
            .messages
            .iter()
            .filter(|message| message.role == mom_llama_runtime::MessageRole::User)
            .all(|message| !message.content.contains("Synthesize the invited responses")),
        "synthesis must not expose an internal synthetic user prompt"
    );
    assert_eq!(
        mom_llama_runtime::conversation_select(&source.id)?.result,
        Some(source_before),
        "group dispatch must never write back into its source chat"
    );
    for persona_id in persona_ids {
        let persona = mom_llama_runtime::persona_get(&persona_id)?
            .result
            .ok_or_else(|| anyhow!("group persona vanished"))?;
        assert_eq!(persona.execution_profile.version, 1);
    }
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_kv_cache_save_restore_proves_equivalence() -> Result<()> {
    let _session = configured_real_session("real-cache")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        context_tokens: Some(8192),
        batch_tokens: Some(512),
        max_parallel_sequences: Some(1),
        resident_memory_budget_bytes: Some(21_845 * 1024 * 1024),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let skill = mom_llama_runtime::skill_store::skill_create(
        "Cache proof".to_string(),
        "Deterministic cache verification".to_string(),
        "Answer concisely and accurately.".to_string(),
        "Use for cache verification.".to_string(),
        KvCachePolicy::PromptPrefix,
    )?
    .result
    .ok_or_else(|| anyhow!("cache skill missing"))?;
    let saved = mom_llama_runtime::kv_cache_save(&scope, Some(skill.id))?;
    assert_eq!(saved.readiness, "prompt_smoke_verified");
    let cache_id = saved
        .result
        .as_ref()
        .map(|entry| entry.id.clone())
        .ok_or_else(|| anyhow!("cache metadata missing"))?;
    let restored = mom_llama_runtime::kv_cache_restore(&scope, Some(cache_id))?;
    assert_eq!(restored.readiness, "prompt_smoke_verified");
    assert!(restored.receipt.real_engine_invoked);
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_long_voice_persona_cache_reloads_from_encrypted_chunks_after_worker_restart() -> Result<()>
{
    let session = configured_real_session("real-large-voice-cache-reload")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        native_device: Some(llama_native_types::NativeDevice::Metal),
        context_tokens: Some(8192),
        batch_tokens: Some(512),
        max_parallel_sequences: Some(2),
        resident_memory_budget_bytes: Some(21_845 * 1024 * 1024),
        max_tokens: Some(8),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    let source = mom_llama_runtime::conversation_new(Some("Long cache source".into()))?
        .result
        .ok_or_else(|| anyhow!("long cache source conversation missing"))?;
    let long_persona = "You are a concise, careful assistant. Preserve names and chronology, answer in one short sentence, ask one gentle question only when a key detail is missing, and do not invent memories. ".repeat(150);
    mom_llama_runtime::conversation_system_message_update(&source.id, Some(long_persona))?;
    mom_llama_runtime::chat_send_in_scope(
        &scope,
        ChatSendInput {
            conversation_id: source.id.clone(),
            message: "Acknowledge this isolated cache test.".into(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    let source = mom_llama_runtime::conversation_select(&source.id)?
        .result
        .ok_or_else(|| anyhow!("long cache source reload missing"))?;
    let leaf = source
        .active_leaf_message_id
        .clone()
        .ok_or_else(|| anyhow!("long cache source leaf missing"))?;
    let persona = mom_llama_runtime::persona_freeze(PersonaFreezeInput {
        conversation_id: source.id.clone(),
        message_id: leaf,
        name: "Long cache witness".into(),
        mention_handle: "long-cache-witness".into(),
        history_mode: PersonaHistoryMode::SystemOnly,
    })?
    .result
    .ok_or_else(|| anyhow!("long cache persona missing"))?;

    let prepare_started = Instant::now();
    let prepared = mom_llama_runtime::kv_cache_save_voice_persona(&scope, &persona.id)?
        .result
        .ok_or_else(|| anyhow!("long voice persona cache preparation failed"))?;
    let metadata = prepared
        .metadata
        .as_ref()
        .expect("persisted preparation metadata");
    assert!(
        metadata.token_ids.len() > 4_000,
        "persona cache prefix was not long enough to exercise SWA retention: {}",
        metadata.token_ids.len()
    );
    assert!(
        metadata.state_bytes > 1_000_000_000,
        "real native state did not exceed SQLite's single-value range: {} bytes",
        metadata.state_bytes
    );
    let cache_id = metadata.id.clone();
    let blob_base = format!("kv-cache.v3.blob.{cache_id}");
    let connection = rusqlite::Connection::open(session.path().join("runtime.sqlite3"))?;
    let mut statement = connection.prepare(
        "SELECT namespace FROM encrypted_documents WHERE namespace LIKE ?1 ORDER BY namespace",
    )?;
    let chunk_names = statement
        .query_map([format!("{blob_base}%")], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let chunk_count = chunk_names.len();
    assert!(
        chunk_count >= 2,
        "large state was not stored in multiple encrypted parts"
    );
    assert!(chunk_names.contains(&format!("{blob_base}.part-1")));
    println!(
        "VOICE_LARGE_CACHE_PERSIST {}",
        serde_json::to_string(&json!({
            "model_sha256": metadata.fingerprint.model_sha256,
            "token_count": metadata.token_ids.len(),
            "state_bytes": metadata.state_bytes,
            "encrypted_parts": chunk_count,
            "prepare_ms": prepare_started.elapsed().as_millis(),
            "persisted": prepared.persisted,
        }))?
    );
    drop(scope);
    drop(native_owner);

    let restored_owner = initialize_product_runtime()?;
    let restored_scope = restored_owner.operation_scope();
    let restore_started = Instant::now();
    let restored = mom_llama_runtime::kv_cache_restore(&restored_scope, Some(cache_id.clone()))?;
    assert_eq!(
        restored.readiness, "prompt_smoke_verified",
        "persistent chunk restore failed: {:#?}",
        restored.blocker
    );
    assert!(restored.receipt.real_engine_invoked);
    assert!(
        restored
            .receipt
            .artifacts_produced
            .iter()
            .any(|artifact| artifact.contains("authenticated")
                && artifact.contains("native KV state")),
        "restart restored by native state import, not token replay: {:?}",
        restored.receipt.artifacts_produced
    );
    assert!(
        !restored
            .receipt
            .artifacts_produced
            .iter()
            .any(|artifact| artifact.contains("token IDs")),
        "large-state restart unexpectedly fell back to token replay: {:?}",
        restored.receipt.artifacts_produced
    );
    assert_eq!(
        restored.result.as_ref().map(|entry| entry.id.as_str()),
        Some(cache_id.as_str())
    );
    println!(
        "VOICE_LARGE_CACHE_RELOAD {}",
        serde_json::to_string(&json!({
            "cache_id": cache_id,
            "model_sha256": metadata.fingerprint.model_sha256,
            "state_bytes": metadata.state_bytes,
            "encrypted_parts": chunk_count,
            "restore_ms": restore_started.elapsed().as_millis(),
            "readiness": restored.readiness,
            "real_engine_invoked": restored.receipt.real_engine_invoked,
            "restore_evidence": restored.receipt.artifacts_produced,
        }))?
    );
    Ok(())
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_tool_loop_invokes_model_and_persists_tool_lineage() -> Result<()> {
    let session = configured_real_session("real-tool-loop")?;
    let native_owner = initialize_product_runtime()?;
    let scope = native_owner.operation_scope();
    configure_mcp_fixture(&session)?;
    let prepared = mom_llama_runtime::tool_loop_prepare_in_scope(
        &scope,
        "real-tool-loop",
        "Use the supplied tool result, then answer in one short sentence.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
    )?;
    let approval_id = prepared
        .result
        .map(|approval| approval.id)
        .ok_or_else(|| anyhow!("tool loop approval missing"))?;
    let mut stream_events = Vec::new();
    let result = mom_llama_runtime::tool_loop_run_stream_in_scope(
        &scope,
        mom_llama_runtime::ToolLoopRunInput {
            conversation_id: "real-tool-loop".to_string(),
            prompt: "Use the supplied tool result, then answer in one short sentence.".to_string(),
            server: "fixture".to_string(),
            tool: "echo".to_string(),
            arguments: json!({"value":"ready"}),
            max_turns: 2,
            approval_id: Some(approval_id),
        },
        |event| {
            stream_events.push(event);
            Ok(())
        },
    )?;
    assert_eq!(result.readiness, "real_prompt_smoke_passed");
    assert!(result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    let output = result
        .result
        .ok_or_else(|| anyhow!("tool loop output missing"))?;
    assert!(!output.steps.is_empty());
    assert!(output.steps.len() <= 2);
    assert!(!output.final_answer.trim().is_empty());
    assert!(!output.model_request_ids.is_empty());
    assert!(output.transcript_message_ids.len() >= 3);
    assert!(
        stream_events
            .iter()
            .any(|event| event.event == "tool_call_started")
    );
    assert!(
        stream_events
            .iter()
            .any(|event| event.event == "tool_result" && event.result.is_some())
    );
    assert!(
        stream_events
            .iter()
            .any(|event| event.event == "model_delta" && event.delta.is_some())
    );
    assert!(
        stream_events
            .iter()
            .any(|event| event.event == "model_started")
    );
    assert!(
        stream_events
            .iter()
            .filter(|event| event.event == "model_started")
            .all(|event| !event.real_engine_invoked)
    );
    let conversation = mom_llama_runtime::conversation_select("real-tool-loop")?
        .result
        .ok_or_else(|| anyhow!("tool loop conversation missing"))?;
    assert!(
        conversation
            .messages
            .iter()
            .any(|message| message.role == mom_llama_runtime::MessageRole::Tool)
    );
    assert!(
        conversation
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .all(|message| message.prompt_tokens.is_some() && message.completion_tokens.is_some())
    );
    Ok(())
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_tool_loop_cancels_an_active_model_request() -> Result<()> {
    let session = configured_real_session("real-tool-loop-cancel")?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(512),
        ..SettingsUpdate::default()
    })?;
    let native_owner = initialize_product_runtime()?;
    let operation_scope = native_owner.operation_scope();
    configure_mcp_fixture(&session)?;
    let conversation_id = "real-tool-loop-cancel";
    let prompt =
        "Use the tool result, then write a very long detailed response with many paragraphs."
            .to_string();
    let arguments = json!({"value":"ready"});
    let prepared = mom_llama_runtime::tool_loop_prepare_in_scope(
        &operation_scope,
        conversation_id,
        prompt.clone(),
        "fixture".to_string(),
        "echo".to_string(),
        arguments.clone(),
        2,
    )?;
    let approval_id = prepared
        .result
        .map(|approval| approval.id)
        .ok_or_else(|| anyhow!("tool loop approval missing"))?;
    let prompt_for_worker = prompt.clone();
    let arguments_for_worker = arguments.clone();
    let worker_scope = operation_scope.clone();
    let worker_data_dir = session.path().to_path_buf();
    let worker = std::thread::spawn(move || {
        set_data_dir_override_for_tests(Some(worker_data_dir));
        let result = mom_llama_runtime::tool_loop_run_in_scope(
            &worker_scope,
            conversation_id,
            prompt_for_worker,
            "fixture".to_string(),
            "echo".to_string(),
            arguments_for_worker,
            2,
            Some(approval_id),
        );
        set_data_dir_override_for_tests(None);
        result
    });

    // No observation, timeout, or cancellation error may abandon the owned worker.
    let observation = (|| -> Result<_> {
        let deadline = Instant::now() + Duration::from_mins(2);
        let active = loop {
            let status = mom_llama_runtime::tool_loop_status(Some(conversation_id))?;
            if let Some(active) = status
                .result
                .unwrap_or_default()
                .into_iter()
                .find(|active| {
                    active.current_model_request_id.is_some()
                        && active.state == mom_llama_runtime::ToolLoopState::Running
                })
            {
                // The durable model ID is admission intent. Require live native work too.
                if mom_llama_runtime::resident_status(&operation_scope)
                    .is_some_and(|status| status.active_sequences > 0)
                {
                    break active;
                }
            }
            if worker.is_finished() {
                return Err(anyhow!(
                    "tool loop finished before exposing an active native request"
                ));
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "tool loop did not expose an active native request before timeout"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let cancelled =
            mom_llama_runtime::tool_loop_cancel_in_scope(&operation_scope, conversation_id)?;
        Ok((active, cancelled))
    })();
    if observation.is_err() {
        operation_scope.request_cancellation();
    }
    let joined = worker
        .join()
        .map_err(|_| anyhow!("tool-loop worker panicked"));
    let (active, cancelled) = observation?;
    let result = joined??;
    assert!(active.current_model_request_id.is_some());
    assert_eq!(cancelled.status, "contracted");
    assert_eq!(
        cancelled
            .result
            .as_ref()
            .and_then(|output| output.current_model_request_id.as_ref()),
        active.current_model_request_id.as_ref()
    );
    assert_eq!(result.readiness, "cancelled");
    assert_eq!(
        result.blocker.as_ref().map(|blocker| blocker.code.as_str()),
        Some("tool_loop_cancelled")
    );
    assert!(result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    let final_status = mom_llama_runtime::tool_loop_status(Some(conversation_id))?;
    assert!(
        final_status
            .result
            .unwrap_or_default()
            .iter()
            .any(|status| {
                status.request_id == active.request_id
                    && status.state == mom_llama_runtime::ToolLoopState::Cancelled
            })
    );
    Ok(())
}

#[test]
fn blocked_dispatch_retains_captured_draft_after_selection_changes() -> Result<()> {
    let scope = mom_llama_runtime::OperationScope::detached();
    let _session = TestSession::new("dispatch-draft-ownership")?;
    let a = mom_llama_runtime::conversation_new(Some("A".into()))?
        .result
        .ok_or_else(|| anyhow!("missing ownership fixture result"))?;
    let b = mom_llama_runtime::conversation_new(Some("B".into()))?
        .result
        .ok_or_else(|| anyhow!("missing ownership fixture result"))?;
    let message = "@missing-person please answer";
    mom_llama_runtime::draft_update(Some(&a.id), message.into(), vec![])?;
    mom_llama_runtime::draft_update(Some(&b.id), "precious B".into(), vec![])?;
    mom_llama_runtime::conversation_select(&b.id)?;
    let result = mom_llama_runtime::chat_dispatch_in_scope(
        &scope,
        MentionDispatchInput {
            conversation_id: a.id.clone(),
            message: message.into(),
        },
        ChatSendOptions {
            fake_fixture: true,
            ..ChatSendOptions::default()
        },
    )?;
    assert_eq!(result.status, "blocked");
    assert_eq!(
        mom_llama_runtime::draft_get(Some(&a.id))?
            .result
            .ok_or_else(|| anyhow!("missing ownership fixture result"))?
            .message,
        message
    );
    assert_eq!(
        mom_llama_runtime::draft_get(Some(&b.id))?
            .result
            .ok_or_else(|| anyhow!("missing ownership fixture result"))?
            .message,
        "precious B"
    );
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn conversation_search_snippets_preserve_original_unicode_positions_and_author_rows() -> Result<()>
{
    let session = TestSession::new("search-unicode-snippets")?;
    let cases = [
        (
            "ascii",
            format!("{}Needle\t after\nOriginal", "a".repeat(50)),
            "needle",
            format!("{}Needle after Original", "a".repeat(40)),
        ),
        (
            "multibyte",
            format!("{}Needle", "é".repeat(50)),
            "needle",
            format!("{}Needle", "é".repeat(40)),
        ),
        (
            "expansion",
            format!("{}Needle", "İ".repeat(50)),
            "needle",
            format!("{}Needle", "İ".repeat(40)),
        ),
        (
            "inside-expansion",
            format!("{}İ Tail", "a".repeat(50)),
            "\u{307}",
            format!("{}İ Tail", "a".repeat(40)),
        ),
        (
            "contextual-sigma",
            format!("{} ΟΣ Tail", "a".repeat(50)),
            "ος",
            format!("{} ΟΣ Tail", "a".repeat(39)),
        ),
        (
            "long",
            format!("Head\t{}", "x".repeat(200)),
            "head",
            format!("Head {}", "x".repeat(115)),
        ),
    ];
    let mut imported_ids = Vec::new();
    for (name, content, _, _) in &cases {
        let mut conversation = attributed_history_fixture(&format!("search-{name}"));
        conversation.title = format!("Search {name}");
        conversation.messages.truncate(1);
        conversation.messages[0].content.clone_from(content);
        conversation.active_leaf_message_id = Some(conversation.messages[0].id.clone());
        let imported =
            mom_llama_runtime::conversation_import_json(&serde_json::to_string(&conversation)?)?;
        assert_eq!(imported.status, "contracted");
        assert_eq!(imported.readiness, "contracted");
        let imported = imported.result.context("search fixture was not imported")?;
        assert_eq!(imported.messages[0].content, *content);
        imported_ids.push(imported.id);
    }
    let author_rows = encrypted_document_snapshot(session.path())?;
    assert!(author_rows.contains_key("conversations.v2"));
    let search = |query: &str| -> Result<Vec<mom_llama_runtime::ConversationSearchHit>> {
        let result = mom_llama_runtime::conversation_search(query)?;
        assert_eq!(result.status, "contracted");
        assert_eq!(result.readiness, "contracted");
        assert!(!result.receipt.real_engine_invoked);
        assert!(!result.receipt.fake_fixture);
        assert!(result.receipt.changed_paths.is_empty());
        assert_eq!(encrypted_document_snapshot(session.path())?, author_rows);
        result.result.context("search output missing")
    };
    // Ordinary matching, normalization, empty and no-match controls run first.
    let ordinary = search(" NEEDLE ")?;
    assert_eq!(ordinary.len(), 3);
    let ascii = ordinary
        .iter()
        .find(|hit| hit.conversation_id == imported_ids[0])
        .context("ASCII search hit missing")?;
    assert_eq!(ascii.snippet, cases[0].3);
    assert_eq!(ascii.message_count, 1);
    let empty = search("  ")?;
    assert_eq!(empty.len(), cases.len());
    let long = empty
        .iter()
        .find(|hit| hit.conversation_id == imported_ids[5])
        .context("empty-query long hit missing")?;
    assert_eq!(long.snippet, cases[5].3);
    assert_eq!(long.snippet.chars().count(), 120);
    assert!(search("absent-search-term")?.is_empty());
    let collapsed_miss = search("Needle\t after")?;
    assert_eq!(collapsed_miss.len(), 1);
    assert_eq!(
        collapsed_miss[0].snippet,
        format!("{}Needle after Original", "a".repeat(50))
    );
    let sigma = search(cases[4].2)?;
    assert_eq!(sigma.len(), 1);
    assert_eq!(sigma[0].snippet, cases[4].3);
    for index in [1, 2, 3] {
        let hits = search(cases[index].2)?;
        let hit = hits
            .iter()
            .find(|hit| hit.conversation_id == imported_ids[index])
            .context("Unicode search hit missing")?;
        assert_eq!(
            hit.snippet, cases[index].3,
            "{} search lost its source position",
            cases[index].0
        );
        assert!(hit.snippet.chars().count() <= 120);
    }
    assert_eq!(encrypted_document_snapshot(session.path())?, author_rows);
    Ok(())
}
