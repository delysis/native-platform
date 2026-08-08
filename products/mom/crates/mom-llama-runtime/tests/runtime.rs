use anyhow::{Result, anyhow};
use mom_llama_runtime::config::{SettingsUpdate, set_data_dir_override_for_tests};
use mom_llama_runtime::{
    ChatDispatchOutput, ChatSendInput, ChatSendOptions, ConsultPersona, ConsultStartInput,
    ConsultStartOptions, EngineCheckOptions, KvCachePolicy, MentionDispatchInput,
    PersonaFreezeInput, PersonaHistoryMode,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

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
fn cache_mode_is_coherent_and_off_blocks_manual_cache_access() -> Result<()> {
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
        mom_llama_runtime::kv_cache_save(None)?,
        mom_llama_runtime::kv_cache_restore(None)?,
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
fn dream_team_creation_is_encrypted_persistent_and_non_impersonating() -> Result<()> {
    let session = TestSession::new("dream-team-create")?;
    let created = mom_llama_runtime::consult_panel_create(
        "Mom's favorites".to_string(),
        vec![ConsultPersona {
            id: String::new(),
            label: "Compassionate author lens".to_string(),
            description: "Reflects gently and names practical choices.".to_string(),
            perspective_prompt: "Offer a warm reflection grounded in public writing.".to_string(),
            public_figure: Some("Private Example Author 8642".to_string()),
            expertise: Some("Compassion and practical reflection".to_string()),
            model_slot: None,
        }],
    )?;
    assert_eq!(created.readiness, "contracted");
    assert!(created.blocker.is_none());
    let panel = created
        .result
        .ok_or_else(|| anyhow!("Dream Team was not created"))?;
    assert_eq!(panel.personas.len(), 1);
    assert!(!panel.personas[0].id.is_empty());

    let listed = mom_llama_runtime::consult_panel_list()?
        .result
        .ok_or_else(|| anyhow!("Dream Team list missing"))?;
    assert!(listed.iter().any(|candidate| candidate.id == panel.id));
    assert!(
        listed
            .iter()
            .any(|candidate| candidate.id == "builtin-trauma-balanced")
    );
    assert!(
        listed
            .iter()
            .any(|candidate| candidate.id == "builtin-emdr-formulation")
    );

    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(b"Private Example Author 8642".len())
            .any(|window| window == b"Private Example Author 8642")
    );
    Ok(())
}

#[test]
fn legacy_consult_migration_to_personas_and_groups_is_idempotent() -> Result<()> {
    let _session = TestSession::new("consult-persona-migration")?;
    mom_llama_runtime::consult_panel_create(
        "Migration team".to_string(),
        vec![ConsultPersona {
            id: String::new(),
            label: "Migration lens".to_string(),
            description: "A durable migration test lens.".to_string(),
            perspective_prompt: "Keep the migration exact.".to_string(),
            public_figure: None,
            expertise: Some("Migration".to_string()),
            model_slot: None,
        }],
    )?;
    let first_personas = mom_llama_runtime::persona_list()?
        .result
        .expect("first migrated Persona list missing");
    let first_groups = mom_llama_runtime::persona_group_list()?
        .result
        .expect("first migrated group list missing");
    let second_personas = mom_llama_runtime::persona_list()?
        .result
        .expect("second migrated Persona list missing");
    let second_groups = mom_llama_runtime::persona_group_list()?
        .result
        .expect("second migrated group list missing");
    assert_eq!(first_personas, second_personas);
    assert_eq!(first_groups, second_groups);
    assert_eq!(
        first_personas
            .iter()
            .filter(|persona| persona.id.starts_with("persona-"))
            .count(),
        15,
        "the exact 14 supplied Personas plus the one custom migrated Persona should exist"
    );
    assert!(
        first_personas
            .iter()
            .any(|persona| persona.title == "Bessel van der Kolk")
    );
    assert!(first_personas.iter().any(|persona| {
        persona.id == "persona-richard_schwartz" && persona.title == "Richard Schwartz"
    }));
    assert!(
        first_groups
            .iter()
            .all(|group| !group.id.starts_with("group-builtin-")),
        "default legacy panels must not appear as user-configured consult groups"
    );
    assert!(
        first_groups
            .iter()
            .any(|group| group.name == "Migration team")
    );
    Ok(())
}

#[test]
fn freezing_and_sending_from_a_persona_never_mutates_its_source_or_template() -> Result<()> {
    let _session = TestSession::new("persona-freeze-isolation")?;
    let source = mom_llama_runtime::conversation_new(Some("Source interview".to_string()))?
        .result
        .ok_or_else(|| anyhow!("source conversation missing"))?;
    mom_llama_runtime::chat_send(
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
    let dispatched = mom_llama_runtime::chat_dispatch(
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
    let _session = TestSession::new("mention-snapshot-isolation")?;
    let source = mom_llama_runtime::conversation_new(Some("Persona source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("source missing"))?;
    mom_llama_runtime::chat_send(
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
    let result = mom_llama_runtime::chat_dispatch(
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
    let fixture_synthesis = mom_llama_runtime::mention_synthesize(&invocation.id)?;
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
    let _session = TestSession::new("live-chat-mention")?;
    let source = mom_llama_runtime::conversation_new(Some("Research notes".to_string()))?
        .result
        .expect("live-chat source missing");
    mom_llama_runtime::chat_send(
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
    let result = mom_llama_runtime::chat_dispatch(
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
        mom_llama_runtime::conversation_store::active_path_messages(&source_before)
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

    let follow_up = mom_llama_runtime::chat_dispatch(
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
    let _session = TestSession::new("persona-handle-safety")?;
    let source = mom_llama_runtime::conversation_new(Some("Source".to_string()))?
        .result
        .expect("handle-safety source missing");
    mom_llama_runtime::chat_send(
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
    let missing = mom_llama_runtime::chat_dispatch(
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
fn blocked_chat_stream_never_claims_native_engine_invocation() -> Result<()> {
    let _session = TestSession::new("blocked-stream-evidence")?;
    let mut events = Vec::new();
    let result = mom_llama_runtime::chat_send_stream(
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
    assert_eq!(result.readiness, "blocked_missing_model");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "started");
    assert!(!events[0].real_engine_invoked);
    assert!(!events[0].fake_fixture);
    assert!(!result.receipt.real_engine_invoked);
    Ok(())
}

#[test]
fn skip_reasoning_without_an_active_request_is_typed() -> Result<()> {
    let _session = TestSession::new("skip-reasoning-no-active")?;
    let result = mom_llama_runtime::chat_skip_reasoning("conversation")?;
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
    let session = TestSession::new("tool-loop-missing-model")?;
    configure_mcp_fixture(&session)?;
    let prepared = mom_llama_runtime::tool_loop_prepare(
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
    let result = mom_llama_runtime::tool_loop_run(
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
    let session = TestSession::new("tool-loop-approval")?;
    configure_mcp_fixture(&session)?;
    let without_approval = mom_llama_runtime::tool_loop_run(
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

    let prepared = mom_llama_runtime::tool_loop_prepare(
        "tool-loop-approval",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
    )?
    .result
    .ok_or_else(|| anyhow!("tool loop approval missing"))?;
    let mismatch = mom_llama_runtime::tool_loop_run(
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
    let approved_attempt = mom_llama_runtime::tool_loop_run(
        "tool-loop-approval",
        "Use the tool.".to_string(),
        "fixture".to_string(),
        "echo".to_string(),
        json!({"value":"ready"}),
        2,
        Some(prepared.id.clone()),
    )?;
    assert_eq!(approved_attempt.readiness, "blocked_missing_model");
    let reused = mom_llama_runtime::tool_loop_run(
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
    let session = TestSession::new("tool-permissions")?;
    configure_mcp_fixture(&session)?;

    mom_llama_runtime::tool_permission_set(
        "fixture".to_string(),
        "echo".to_string(),
        mom_llama_runtime::ToolPermissionPolicy::Deny,
    )?;
    let denied = mom_llama_runtime::tool_loop_prepare(
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
    let always_allowed = mom_llama_runtime::tool_loop_prepare(
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
    let without_prompt = mom_llama_runtime::tool_loop_run(
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
    let ask_again = mom_llama_runtime::tool_loop_prepare(
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
fn product_runtime_rejects_network_process_and_copied_native_authority() -> Result<()> {
    let runtime_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = runtime_manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow!("runtime crate has no repository root"))?;
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
    ] {
        assert!(
            !repo_root.join("crates").join(crate_name).exists(),
            "{crate_name} must remain an immutable external dependency"
        );
    }
    let workspace_manifest = fs::read_to_string(repo_root.join("Cargo.toml"))?;
    assert!(workspace_manifest.contains("rev = \"a185a4be3c6ad6ea1935e01acef8946c7dfdc459\""));
    assert!(!workspace_manifest.contains("[patch."));

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
fn message_tree_preserves_siblings_and_switches_the_active_leaf() -> Result<()> {
    let _session = TestSession::new("message-tree")?;
    let options = ChatSendOptions {
        timeout_s: 1.0,
        fake_fixture: true,
    };
    let first = mom_llama_runtime::chat_send(
        ChatSendInput {
            conversation_id: "message-tree".to_string(),
            message: "Give me one answer.".to_string(),
        },
        options,
    )?
    .result
    .ok_or_else(|| anyhow!("first fixture output missing"))?;
    let second = mom_llama_runtime::chat_regenerate("message-tree", options)?
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
    let _session = TestSession::new("message-edit-branches")?;
    let output = mom_llama_runtime::chat_send(
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
    let metadata_only = mom_llama_runtime::attachment_preview(&output.attachment.id, false)?
        .result
        .ok_or_else(|| anyhow!("attachment metadata preview missing"))?;
    assert_eq!(metadata_only.attachment.sha256, output.attachment.sha256);
    assert!(metadata_only.bytes.is_none());
    let hydrated = mom_llama_runtime::attachment_preview(&output.attachment.id, true)?
        .result
        .ok_or_else(|| anyhow!("attachment payload preview missing"))?;
    assert_eq!(hydrated.bytes.as_deref(), Some(payload.as_slice()));
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
fn long_paste_becomes_an_encrypted_text_attachment_without_a_plaintext_file() -> Result<()> {
    let session = TestSession::new("pasted-text-attachment")?;
    let marker = "private-long-paste-9137 ".repeat(160);
    let imported = mom_llama_runtime::attachment_import_pasted_text("default", marker.clone())?;
    assert_eq!(imported.readiness, "contracted");
    let output = imported
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("pasted attachment result missing"))?;
    assert_eq!(output.attachment.source_path, "pasted-text");
    assert!(output.attachment.file_name.starts_with("pasted-text-"));
    assert!(output.attachment.stored_path.starts_with("encrypted://"));
    assert!(!session.path().join(&output.attachment.file_name).exists());

    let conversation = mom_llama_runtime::conversation_select("default")?
        .result
        .ok_or_else(|| anyhow!("default conversation missing"))?;
    assert!(
        conversation
            .messages
            .iter()
            .any(|message| message.content.contains("private-long-paste-9137"))
    );
    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
    );
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
    let session = TestSession::new("mcp")?;
    configure_mcp_fixture(&session)?;
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
            "content": [{"type":"text","text":"fixture tool result"}]
        }
    });
    let body = serde_json::to_string(&response)?;
    let executable = session.path().join("mcp-fixture");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf 'Content-Length: {}\\r\\n\\r\\n{}'\n",
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
        native_device: Some(llama_native_types::NativeDevice::Cpu),
        max_tokens: Some(96),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    Ok(Some(session))
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

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn resident_model_profiles_fail_closed_before_memory_overcommit_without_eviction() -> Result<()> {
    use std::os::unix::fs::symlink;

    let Some(session) = configured_real_session("real-resident-memory-budget")? else {
        return Ok(());
    };
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
    let settings = mom_llama_runtime::config::resolve_settings()?;
    let first = mom_llama_runtime::resident_model_for_profile(&settings, &model_path, None)
        .map_err(|blocked| anyhow!(blocked.blocker.message))?;
    let first_model_id = first.status().model_id;

    let second_profile_path = session.path().join("same-weights-distinct-profile.gguf");
    symlink(&model_path, &second_profile_path)?;
    let blocked =
        mom_llama_runtime::resident_model_for_profile(&settings, &second_profile_path, None)
            .expect_err("the second resident profile must be rejected before overcommit");
    assert_eq!(blocked.readiness, "blocked_memory_budget");
    assert_eq!(
        blocked.blocker.code,
        "resident_model_memory_budget_exceeded"
    );
    assert_eq!(
        mom_llama_runtime::resident_status().map(|status| status.model_id),
        Some(first_model_id),
        "a rejected profile must never evict the already resident model"
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn repeated_profile_acquisition_reuses_the_same_resident_worker() -> Result<()> {
    let Some(_session) = configured_real_session("real-resident-worker-reuse")? else {
        return Ok(());
    };
    let model_path = std::env::var_os("MOM_LLAMA_MODEL_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("real model path disappeared"))?;
    let settings = mom_llama_runtime::config::resolve_settings()?;

    let first = mom_llama_runtime::resident_model_for_profile(&settings, &model_path, None)
        .map_err(|blocked| anyhow!(blocked.blocker.message))?;
    let second = mom_llama_runtime::resident_model_for_profile(&settings, &model_path, None)
        .map_err(|blocked| anyhow!(blocked.blocker.message))?;

    assert!(
        first.is_same_worker(&second),
        "reacquiring an unchanged profile must reuse its resident worker"
    );
    assert_eq!(mom_llama_runtime::native_runtime::resident_slots().len(), 1);
    Ok(())
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
        model_path: Some(model_path),
        mmproj_path: Some(mmproj_path),
        native_device: Some(llama_native_types::NativeDevice::Cpu),
        max_tokens: Some(4),
        context_tokens: Some(4096),
        resident_memory_budget_bytes: Some(12 * 1024 * 1024 * 1024),
        ..SettingsUpdate::default()
    })?;
    let image_bytes = std::fs::read(&image_path)?;
    let imported = mom_llama_runtime::attachment_import("real-multimodal-image", &image_path)?;
    let attachment = imported
        .result
        .as_ref()
        .map(|output| output.attachment.clone())
        .ok_or_else(|| anyhow!("multimodal attachment import missing"))?;
    assert!(attachment.stored_path.starts_with("encrypted://"));

    let result = mom_llama_runtime::chat_send(
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
    let status = mom_llama_runtime::resident_status()
        .ok_or_else(|| anyhow!("resident multimodal model status missing"))?;
    assert!(
        status
            .fingerprint
            .as_ref()
            .is_some_and(|fingerprint| { fingerprint.multimodal_projector_sha256.is_some() })
    );
    let preview = mom_llama_runtime::attachment_preview(&attachment.id, true)?
        .result
        .ok_or_else(|| anyhow!("decrypted multimodal preview missing"))?;
    assert_eq!(preview.bytes.as_deref(), Some(image_bytes.as_slice()));
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
    let audio_import = mom_llama_runtime::attachment_import("real-multimodal-audio", &audio_path);
    std::fs::remove_file(&audio_path)?;
    let audio_attachment = audio_import?
        .result
        .as_ref()
        .map(|output| output.attachment.clone())
        .ok_or_else(|| anyhow!("multimodal audio attachment import missing"))?;
    assert!(audio_attachment.stored_path.starts_with("encrypted://"));
    let audio_result = mom_llama_runtime::chat_send(
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
    let audio_preview = mom_llama_runtime::attachment_preview(&audio_attachment.id, true)?
        .result
        .ok_or_else(|| anyhow!("decrypted multimodal audio preview missing"))?;
    assert_eq!(audio_preview.bytes.as_deref(), Some(audio_bytes.as_slice()));
    let sqlite = std::fs::read(session.path().join("runtime.sqlite3"))?;
    assert!(
        !sqlite
            .windows(audio_bytes.len())
            .any(|window| window == audio_bytes)
    );
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
        model_path: Some(model_path),
        native_device: Some(llama_native_types::NativeDevice::Cpu),
        max_tokens: Some(192),
        upstream_settings: Some(BTreeMap::from([
            ("disableReasoningParsing".to_string(), json!(false)),
            ("excludeReasoningFromContext".to_string(), json!(false)),
        ])),
        ..SettingsUpdate::default()
    })?;
    let mut reasoning_preview = String::new();
    let mut skip_result = None;
    let result = mom_llama_runtime::chat_send_stream(
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
                    skip_result = Some(mom_llama_runtime::chat_skip_reasoning(
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
fn real_custom_dream_team_routes_selected_attributed_perspective() -> Result<()> {
    let Some(_session) = configured_real_session("real-dream-team")? else {
        return Ok(());
    };
    let panel = mom_llama_runtime::consult_panel_create(
        "Mom's Dream Team".to_string(),
        vec![ConsultPersona {
            id: "favorite-author".to_string(),
            label: "Favorite author lens".to_string(),
            description: "A gentle perspective inspired by public writing.".to_string(),
            perspective_prompt: "Respond warmly in two short sentences and name uncertainty."
                .to_string(),
            public_figure: Some("Example Author".to_string()),
            expertise: Some("Compassionate public writing".to_string()),
            model_slot: None,
        }],
    )?
    .result
    .ok_or_else(|| anyhow!("custom Dream Team missing"))?;
    let result = mom_llama_runtime::consult_start(
        ConsultStartInput {
            conversation_id: "real-dream-team".to_string(),
            prompt: "How can I prepare calmly for a difficult conversation?".to_string(),
            panel_id: Some(panel.id.clone()),
        },
        ConsultStartOptions::default(),
    )?;
    assert_eq!(result.readiness, "real_prompt_smoke_passed");
    assert!(result.receipt.real_engine_invoked);
    assert!(!result.receipt.fake_fixture);
    let run = result
        .result
        .ok_or_else(|| anyhow!("custom Dream Team run missing"))?;
    assert_eq!(run.panel_id, panel.id);
    assert_eq!(run.seats.len(), 1);
    assert_eq!(run.seats[0].seat_id, "favorite-author");
    assert_eq!(run.seats[0].label, "Favorite author lens");
    assert!(!run.seats[0].text.trim().is_empty());
    assert!(run.seats[0].real_engine_invoked);
    assert!(!run.seats[0].fake_fixture);
    assert_eq!(
        run.seats[0].transport,
        Some(llama_native_types::NativeTransport::InProcess)
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_persona_mentions_reuse_only_the_exact_versioned_prefix() -> Result<()> {
    let Some(_session) = configured_real_session("real-persona-cache")? else {
        return Ok(());
    };
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(24),
        ..SettingsUpdate::default()
    })?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(48),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    let source = mom_llama_runtime::conversation_new(Some("Cache source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("cache source missing"))?;
    mom_llama_runtime::chat_send(
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

    let dispatch = |message: &str| -> Result<mom_llama_runtime::MentionInvocation> {
        let result = mom_llama_runtime::chat_dispatch(
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

    let first = dispatch("Answer in one short sentence.")?;
    let first_result = first
        .results
        .first()
        .ok_or_else(|| anyhow!("first persona result missing"))?;
    assert!(!first_result.cache_reused);
    let first_cache_id = first_result
        .cache_id
        .clone()
        .ok_or_else(|| anyhow!("first persona cache id missing"))?;

    let second = dispatch("Give another short answer.")?;
    let second_result = second
        .results
        .first()
        .ok_or_else(|| anyhow!("second persona result missing"))?;
    assert!(second_result.cache_reused);
    assert_eq!(
        second_result.cache_id.as_deref(),
        Some(first_cache_id.as_str())
    );

    mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::None),
        ..SettingsUpdate::default()
    })?;
    let disabled = dispatch("Answer with prompt caching explicitly disabled.")?;
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
    mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;

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
    let third = dispatch("Answer after the explicit persona revision.")?;
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
    let Some(_session) = configured_real_session("real-persona-group-cancel")? else {
        return Ok(());
    };
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(64),
        ..SettingsUpdate::default()
    })?;
    let source = mom_llama_runtime::conversation_new(Some("Group source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("group source missing"))?;
    mom_llama_runtime::chat_send(
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
    let handle = std::thread::spawn(move || {
        mom_llama_runtime::chat_dispatch_stream(
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
    let invocation_id = started_rx
        .recv_timeout(Duration::from_secs(120))
        .map_err(|error| anyhow!("persona group did not start streaming: {error}"))?;
    let cancelled = mom_llama_runtime::mention_cancel(&invocation_id, Some(&cancelled_persona_id))?;
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
    let synthesis = mom_llama_runtime::mention_synthesize(&invocation.id)?;
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

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_tool_loop_invokes_model_and_persists_tool_lineage() -> Result<()> {
    let Some(session) = configured_real_session("real-tool-loop")? else {
        return Ok(());
    };
    configure_mcp_fixture(&session)?;
    let prepared = mom_llama_runtime::tool_loop_prepare(
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
    let result = mom_llama_runtime::tool_loop_run_stream(
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
    let conversation = mom_llama_runtime::conversation_select("real-tool-loop")?
        .result
        .ok_or_else(|| anyhow!("tool loop conversation missing"))?;
    assert!(
        conversation
            .messages
            .iter()
            .any(|message| message.role == mom_llama_runtime::MessageRole::Tool)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_tool_loop_cancels_an_active_model_request() -> Result<()> {
    let Some(session) = configured_real_session("real-tool-loop-cancel")? else {
        return Ok(());
    };
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(512),
        ..SettingsUpdate::default()
    })?;
    configure_mcp_fixture(&session)?;
    let conversation_id = "real-tool-loop-cancel";
    let prompt =
        "Use the tool result, then write a very long detailed response with many paragraphs."
            .to_string();
    let arguments = json!({"value":"ready"});
    let prepared = mom_llama_runtime::tool_loop_prepare(
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
    let worker = std::thread::spawn(move || {
        mom_llama_runtime::tool_loop_run(
            conversation_id,
            prompt_for_worker,
            "fixture".to_string(),
            "echo".to_string(),
            arguments_for_worker,
            2,
            Some(approval_id),
        )
    });

    let deadline = Instant::now() + Duration::from_secs(120);
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
            break active;
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
    assert!(active.current_model_request_id.is_some());

    let cancelled = mom_llama_runtime::tool_loop_cancel(conversation_id)?;
    assert_eq!(cancelled.status, "contracted");
    assert_eq!(
        cancelled
            .result
            .as_ref()
            .and_then(|output| output.current_model_request_id.as_ref()),
        active.current_model_request_id.as_ref()
    );
    let result = worker
        .join()
        .map_err(|_| anyhow!("tool-loop worker panicked"))??;
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
