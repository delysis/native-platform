use anyhow::{Result, anyhow};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use fte_backend_llama::{BACKEND_ID, LlamaNativeBackend};
use fte_router::{Gateway, GatewayDefaults};
use fte_types::{
    CacheMode, CacheOutcome, CachePolicy, ContentBlock, DeadlinePolicy, GatewayRequest,
    GatewayResponse, GenerationInput, InputItem, MessageRole as GatewayMessageRole, ModelSelector,
    RequestId, ResponseFormat, RoutingPolicy, SamplingOptions, StoragePolicy, StreamPolicy,
    TerminalStatus, ToolPolicy,
};
use mom_llama_runtime::config::{SettingsUpdate, set_data_dir_override_for_tests};
use mom_llama_runtime::{
    ChatDispatchOutput, ChatSendInput, ChatSendOptions, ConsultPanel, ConsultPersona,
    ConsultStartInput, ConsultStartOptions, Conversation, ConversationExecutionProfile,
    ConversationKind, EngineCheckOptions, KvCachePolicy, MentionDispatchInput, Message,
    MessageAttribution, MessageRole, MessageSpeakerKind, PersonaFreezeInput, PersonaHistoryMode,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

const VALID_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\
    \x00\x00\x00\x0dIHDR\x00\x00\x00\x02\x00\x00\x00\x04\x08\x02\x00\x00\x00\x2b\x8d\x79\x6e\
    \x00\x00\x00\x09pHYs\x00\x00\x00\x01\x00\x00\x00\x01\x00\x4f\x25\xc4\xd6\
    \x00\x00\x00\x10IDAT\x78\x9c\x63\xfc\xc3\x00\x02\x2c\x0c\x58\x28\x00\x1b\x74\x01\x0a\x5f\x82\xdc\x5d\
    \x00\x00\x00\x00IEND\xae\x42\x60\x82";

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

fn seed_legacy_consult_panels(data_dir: &Path, panels: Vec<Value>) -> Result<()> {
    const NAMESPACE: &str = "consult-panels.v1";
    let mut key_hasher = Sha256::new();
    key_hasher.update(b"mom-llama-test-store-key-v1");
    key_hasher.update(data_dir.to_string_lossy().as_bytes());
    let key: [u8; 32] = key_hasher.finalize().into();
    let nonce = [0xA5_u8; 24];
    let plaintext = serde_json::to_vec(&json!({ "panels": panels }))?;
    let ciphertext = XChaCha20Poly1305::new(Key::from_slice(&key))
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: NAMESPACE.as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to encrypt legacy Consult fixture"))?;
    rusqlite::Connection::open(data_dir.join("runtime.sqlite3"))?.execute(
        "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
         VALUES (?1, ?2, ?3, 0)
         ON CONFLICT(namespace) DO UPDATE SET
           nonce = excluded.nonce,
           ciphertext = excluded.ciphertext,
           updated_at = excluded.updated_at",
        rusqlite::params![NAMESPACE, nonce.as_slice(), ciphertext],
    )?;
    Ok(())
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
fn legacy_panel_creation_is_read_only_even_before_migration() -> Result<()> {
    let session = TestSession::new("dream-team-create")?;
    let rejected = mom_llama_runtime::consult_panel_create(
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
    assert_eq!(rejected.readiness, "stub_blocked");
    assert!(rejected.result.is_none());
    assert_eq!(
        rejected
            .blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("legacy_consult_panel_write_retired")
    );
    assert!(
        !session.path().join("runtime.sqlite3").exists(),
        "retired runtime writes must not initialize product storage"
    );
    Ok(())
}

#[test]
fn legacy_consult_migration_to_personas_and_groups_is_idempotent() -> Result<()> {
    let session = TestSession::new("consult-persona-migration")?;
    mom_llama_runtime::consult_panel_list()?;
    seed_legacy_consult_panels(
        session.path(),
        vec![json!({
            "id": "migration-team",
            "name": "Migration team",
            "personas": [{
                "id": "migration-lens",
                "label": "Migration lens",
                "description": "A durable migration test lens.",
                "perspective_prompt": "Keep the migration exact.",
                "public_figure": null,
                "expertise": "Migration",
                "model_slot": null
            }],
            "created_at": "1",
            "updated_at": "1"
        })],
    )?;
    let first_personas = mom_llama_runtime::persona_list()?
        .result
        .expect("first migrated Persona list missing");
    let first_groups = mom_llama_runtime::persona_group_list()?
        .result
        .expect("first migrated group list missing");
    let documents_after_migration = encrypted_document_snapshot(session.path())?;
    let second_personas = mom_llama_runtime::persona_list()?
        .result
        .expect("second migrated Persona list missing");
    let second_groups = mom_llama_runtime::persona_group_list()?
        .result
        .expect("second migrated group list missing");
    mom_llama_runtime::mention_candidates("", None)?;
    assert_eq!(
        encrypted_document_snapshot(session.path())?,
        documents_after_migration,
        "subsequent Persona, group, and handle reads must not rewrite legacy migration state"
    );
    let retired_write = mom_llama_runtime::consult_panel_create(
        "Too late legacy team".to_string(),
        vec![ConsultPersona {
            id: "post-migration-lens".to_string(),
            label: "Post-migration lens".to_string(),
            description: "This legacy panel must never be stranded.".to_string(),
            perspective_prompt: "Fail closed instead of writing legacy state.".to_string(),
            public_figure: None,
            expertise: Some("Migration integrity".to_string()),
            model_slot: None,
        }],
    )?;
    assert_eq!(retired_write.readiness, "stub_blocked");
    assert!(retired_write.result.is_none());
    assert_eq!(
        retired_write
            .blocker
            .as_ref()
            .map(|blocker| blocker.code.as_str()),
        Some("legacy_consult_panel_write_retired")
    );
    assert_eq!(
        encrypted_document_snapshot(session.path())?,
        documents_after_migration,
        "a retired legacy write must not mutate any encrypted product document"
    );
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
fn raw_legacy_migration_preserves_builtin_and_persona_id_collisions() -> Result<()> {
    let session = TestSession::new("consult-raw-collisions")?;
    let builtins = mom_llama_runtime::consult_panel_list()?
        .result
        .ok_or_else(|| anyhow!("legacy built-in panel list missing"))?;
    let exact_builtin = builtins
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("legacy built-in panel missing"))?;
    let persona = |id: &str, label: &str, prompt: &str| ConsultPersona {
        id: id.to_string(),
        label: label.to_string(),
        description: format!("{label} description"),
        perspective_prompt: prompt.to_string(),
        public_figure: None,
        expertise: Some("Migration integrity".to_string()),
        model_slot: None,
    };
    let panel = |id: &str, name: &str, personas: Vec<ConsultPersona>| ConsultPanel {
        id: id.to_string(),
        name: name.to_string(),
        personas,
        created_at: "1".to_string(),
        updated_at: "1".to_string(),
    };

    let mut builtin_id_collision = panel(
        &exact_builtin.id,
        "Customized built-in ID",
        vec![persona(
            "shared-id",
            "Customized collision lens",
            "Preserve the customized built-in-ID panel.",
        )],
    );
    builtin_id_collision.updated_at = "customized".to_string();
    let prefixed_user_panel = panel(
        "builtin-custom-user",
        "User-owned builtin prefix",
        vec![persona(
            "prefix-lens",
            "Prefix lens",
            "Preserve user data even when its ID uses an old reserved prefix.",
        )],
    );
    let first_shared_id = panel(
        "first-team",
        "First shared-ID team",
        vec![persona(
            "shared-id",
            "First shared-ID lens",
            "Keep the first prompt exact.",
        )],
    );
    let second_shared_id = panel(
        "second-team",
        "Second shared-ID team",
        vec![persona(
            "shared-id",
            "Second shared-ID lens",
            "Keep the second prompt exact.",
        )],
    );
    seed_legacy_consult_panels(
        session.path(),
        vec![
            serde_json::to_value(exact_builtin.clone())?,
            serde_json::to_value(builtin_id_collision)?,
            serde_json::to_value(prefixed_user_panel)?,
            serde_json::to_value(first_shared_id)?,
            serde_json::to_value(second_shared_id)?,
        ],
    )?;

    let personas = mom_llama_runtime::persona_list()?
        .result
        .ok_or_else(|| anyhow!("migrated Persona list missing"))?;
    let groups = mom_llama_runtime::persona_group_list()?
        .result
        .ok_or_else(|| anyhow!("migrated group list missing"))?;
    assert!(
        !groups.iter().any(|group| group.name == exact_builtin.name),
        "an exact application-owned built-in must not become a user group"
    );
    for recovered_name in [
        "Customized built-in ID",
        "User-owned builtin prefix",
        "First shared-ID team",
        "Second shared-ID team",
    ] {
        assert!(
            groups.iter().any(|group| group.name == recovered_name),
            "raw stored panel `{recovered_name}` was not recovered"
        );
    }
    for recovered_name in ["Customized built-in ID", "User-owned builtin prefix"] {
        assert!(
            groups
                .iter()
                .find(|group| group.name == recovered_name)
                .is_some_and(|group| group.id.starts_with("group-legacy-")),
            "reserved built-in IDs must migrate to deterministic user-owned IDs"
        );
    }
    let first_group = groups
        .iter()
        .find(|group| group.name == "First shared-ID team")
        .ok_or_else(|| anyhow!("first shared-ID group missing"))?;
    let second_group = groups
        .iter()
        .find(|group| group.name == "Second shared-ID team")
        .ok_or_else(|| anyhow!("second shared-ID group missing"))?;
    assert_ne!(first_group.persona_ids, second_group.persona_ids);
    for (group, expected_prompt) in [
        (first_group, "Keep the first prompt exact."),
        (second_group, "Keep the second prompt exact."),
    ] {
        let persona = personas
            .iter()
            .find(|persona| group.persona_ids.first() == Some(&persona.id))
            .ok_or_else(|| anyhow!("migrated shared-ID Persona missing"))?;
        assert_eq!(
            persona.execution_profile.system_message.as_deref(),
            Some(expected_prompt)
        );
    }

    let migrated = encrypted_document_snapshot(session.path())?;
    mom_llama_runtime::persona_list()?;
    mom_llama_runtime::persona_group_list()?;
    assert_eq!(encrypted_document_snapshot(session.path())?, migrated);
    Ok(())
}

#[test]
fn malformed_legacy_panel_blocks_without_advancing_migration() -> Result<()> {
    let session = TestSession::new("consult-malformed-collision")?;
    mom_llama_runtime::consult_panel_list()?;
    let duplicate = json!({
        "id": "duplicate-team",
        "name": "Duplicate team",
        "personas": [{
            "id": "duplicate-lens",
            "label": "Duplicate lens",
            "description": "Duplicate fixture",
            "perspective_prompt": "Keep one exact copy.",
            "public_figure": null,
            "expertise": null,
            "model_slot": null
        }, {
            "id": "duplicate-lens",
            "label": "Duplicate lens",
            "description": "Duplicate fixture",
            "perspective_prompt": "Keep one exact copy.",
            "public_figure": null,
            "expertise": null,
            "model_slot": null
        }],
        "created_at": "1",
        "updated_at": "1"
    });
    seed_legacy_consult_panels(session.path(), vec![duplicate.clone()])?;
    let before = encrypted_document_snapshot(session.path())?;
    let error = mom_llama_runtime::persona_list()
        .expect_err("duplicate legacy Persona content must fail closed");
    assert!(error.to_string().contains("duplicate Persona content"));
    assert_eq!(
        encrypted_document_snapshot(session.path())?,
        before,
        "failed recovery must not persist Personas, groups, versions, or a migration marker"
    );

    let mut repaired = duplicate;
    repaired["personas"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("invalid duplicate fixture"))?
        .pop();
    seed_legacy_consult_panels(session.path(), vec![repaired])?;
    let recovered = mom_llama_runtime::persona_list()?
        .result
        .ok_or_else(|| anyhow!("repaired legacy Persona list missing"))?;
    assert!(
        recovered
            .iter()
            .any(|persona| persona.title == "Duplicate lens"),
        "the failed attempt must not advance the migration marker"
    );
    Ok(())
}

#[test]
fn gateway_document_adapter_is_confined_to_fte_response_namespaces() -> Result<()> {
    let session = TestSession::new("gateway-document-namespace")?;
    for namespace in [
        "consult-panels.v1",
        "persona-groups.v1",
        "fte.response.v1:",
        "fte.response.v1:../consult-panels.v1",
    ] {
        assert!(mom_llama_runtime::gateway_document_get(namespace).is_err());
        assert!(mom_llama_runtime::gateway_document_put(namespace, b"blocked").is_err());
        assert!(mom_llama_runtime::gateway_document_delete(namespace).is_err());
    }
    assert!(
        !session.path().join("runtime.sqlite3").exists(),
        "invalid gateway namespaces must be rejected before product storage opens"
    );

    let namespace = "fte.response.v1:request_01.test";
    mom_llama_runtime::gateway_document_put(namespace, b"response")?;
    assert_eq!(
        mom_llama_runtime::gateway_document_get(namespace)?,
        Some(b"response".to_vec())
    );
    assert!(mom_llama_runtime::gateway_document_delete(namespace)?);
    assert_eq!(mom_llama_runtime::gateway_document_get(namespace)?, None);
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
fn persisted_legacy_handle_collisions_migrate_without_losing_conversation_data() -> Result<()> {
    let session = TestSession::new("persisted-legacy-handle-collisions")?;
    let mut older = attributed_history_fixture("legacy-older");
    older.title = "Older saved chat".to_string();
    older.created_at = "1".to_string();
    older.updated_at = "1".to_string();
    older.execution_profile.mention_handle = "Shared-Lens".to_string();
    let older_messages = older.messages.clone();

    let mut newer = attributed_history_fixture("legacy-newer");
    newer.title = "Newer saved chat".to_string();
    newer.created_at = "2".to_string();
    newer.updated_at = "2".to_string();
    newer.execution_profile.mention_handle = "shared-lens".to_string();
    let newer_messages = newer.messages.clone();

    let legacy = mom_llama_runtime::conversation_store::ConversationDb {
        conversations: vec![newer, older],
        selected_conversation_id: Some("legacy-newer".to_string()),
    };
    fs::write(
        session.path().join("conversations.json"),
        serde_json::to_vec_pretty(&legacy)?,
    )?;

    mom_llama_runtime::persona_list()?;
    let conversations = mom_llama_runtime::conversation_list()?
        .result
        .ok_or_else(|| anyhow!("migrated conversations missing"))?;
    let migrated_older = conversations
        .iter()
        .find(|conversation| conversation.id == "legacy-older")
        .ok_or_else(|| anyhow!("older legacy conversation missing"))?;
    let migrated_newer = conversations
        .iter()
        .find(|conversation| conversation.id == "legacy-newer")
        .ok_or_else(|| anyhow!("newer legacy conversation missing"))?;
    assert_eq!(
        migrated_older.execution_profile.mention_handle,
        "shared-lens"
    );
    assert_eq!(
        migrated_newer.execution_profile.mention_handle,
        "shared-lens-2"
    );
    assert_eq!(migrated_older.messages, older_messages);
    assert_eq!(migrated_newer.messages, newer_messages);
    assert_eq!(migrated_older.title, "Older saved chat");
    assert_eq!(migrated_newer.title, "Newer saved chat");
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
        "attachment-native-types",
        "attachment-native-inspect",
        "attachment-native-document",
        "attachment-native-plan",
        "attachment-native-host",
    ] {
        assert!(
            !repo_root.join("crates").join(crate_name).exists(),
            "{crate_name} must remain an immutable external dependency"
        );
    }
    let workspace_manifest = fs::read_to_string(repo_root.join("Cargo.toml"))?;
    assert!(workspace_manifest.contains("rev = \"4dd744209ff85886be9dce7df46cd65eaa19c804\""));
    assert!(workspace_manifest.contains("rev = \"aa8ce2dab3baf46087f1cff68b8619f947647187\""));
    assert!(workspace_manifest.contains("rev = \"472900732ded5bcfb5cc639c49b3a4f77feece27\""));
    assert!(!workspace_manifest.contains("[patch."));
    assert!(!workspace_manifest.contains("attachment-native-host = { path ="));
    assert!(!workspace_manifest.contains("attachment-native-types = { path ="));

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
        current_skill_ids: Vec::new(),
        messages: vec![system.clone(), tool.clone()],
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
    let _session = TestSession::new("mention-token-boundaries")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Mention boundaries".to_string()))?
        .result
        .ok_or_else(|| anyhow!("mention-boundary conversation missing"))?;
    let literal_message = "Email george@example.com; keep prefix@embedded, `@inline-code`, and:\n```text\n@fenced-code\n```\n    @indented-code";
    let direct = mom_llama_runtime::chat_dispatch(
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
        let blocked = mom_llama_runtime::chat_dispatch(
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
    let payload = VALID_PNG;
    std::fs::write(&image, payload)?;
    let imported = mom_llama_runtime::attachment_import(&conversation.id, &image)?;
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
    let metadata_only = mom_llama_runtime::attachment_preview(&output.attachment.id, false)?
        .result
        .ok_or_else(|| anyhow!("attachment metadata preview missing"))?;
    assert_eq!(metadata_only.attachment.sha256, output.attachment.sha256);
    assert!(metadata_only.bytes.is_none());
    let hydrated = mom_llama_runtime::attachment_preview(&output.attachment.id, true)?
        .result
        .ok_or_else(|| anyhow!("attachment payload preview missing"))?;
    assert_eq!(hydrated.bytes.as_deref(), Some(payload));
    let chat = mom_llama_runtime::chat_send(
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
    let session = TestSession::new("pasted-text-attachment")?;
    let conversation = mom_llama_runtime::conversation_new(Some("Pasted notes".to_string()))?
        .result
        .ok_or_else(|| anyhow!("pasted-notes conversation missing"))?;
    let marker = "private-long-paste-9137 ".repeat(160);
    let imported =
        mom_llama_runtime::attachment_import_pasted_text(&conversation.id, marker.clone())?;
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
    let sent = mom_llama_runtime::chat_send(
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
    let _session = TestSession::new("fixture-mention-attachment-lifecycle")?;
    let source = mom_llama_runtime::conversation_new(Some("Fixture persona source".to_string()))?
        .result
        .ok_or_else(|| anyhow!("fixture persona source missing"))?;
    mom_llama_runtime::chat_send(
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

    let dispatched = mom_llama_runtime::chat_dispatch(
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
        // Reasoning-capable small models can spend the first hundred tokens
        // inside a private reasoning block. A real acceptance must budget
        // enough room to prove that visible assistant text is produced.
        max_tokens: Some(256),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    Ok(Some(session))
}

async fn product_gateway_cache_request(
    stable_system: &str,
    user_message: &str,
    cache_mode: CacheMode,
    owner_version: &str,
) -> Result<GatewayResponse> {
    let (host, model) = mom_llama_runtime::gateway_native_host_and_model()?;
    let model_id = model.model_id.clone();
    let backend = Arc::new(LlamaNativeBackend::new(host));
    backend.configure_model(model)?;
    let gateway = Gateway::new(GatewayDefaults {
        catalog_version: "mom-llama-real-cache-proof-v1".to_string(),
    });
    gateway.register_backend(backend)?;
    let request = GatewayRequest {
        request_id: RequestId::new(),
        client_id: "mom-llama-real-cache-proof".to_string(),
        model: ModelSelector::ExactRoute {
            backend_id: BACKEND_ID.to_string(),
            model_id,
        },
        input: GenerationInput::Chat {
            items: vec![
                InputItem::Message {
                    id: None,
                    role: GatewayMessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: stable_system.to_string(),
                    }],
                },
                InputItem::Message {
                    id: None,
                    role: GatewayMessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: user_message.to_string(),
                    }],
                },
            ],
        },
        sampling: SamplingOptions {
            max_output_tokens: Some(8),
            temperature: Some(0.0),
            seed: Some(7),
            ..SamplingOptions::default()
        },
        response_format: ResponseFormat::default(),
        tools: Vec::new(),
        tool_policy: ToolPolicy::default(),
        cache: CachePolicy {
            mode: cache_mode,
            stable_prefix_items: Some(1),
            owner_namespace: Some("mom-llama-integrated-cache-proof".to_string()),
            owner_version: Some(owner_version.to_string()),
            ..CachePolicy::default()
        },
        routing: RoutingPolicy::default(),
        storage: StoragePolicy::default(),
        deadline: DeadlinePolicy {
            total_ms: Some(180_000),
            ..DeadlinePolicy::default()
        },
        stream: StreamPolicy::default(),
        provider_extensions: BTreeMap::new(),
    };
    Ok(gateway.execute(request).await?.final_response().await?)
}

fn assert_gateway_cache_outcome(
    stage: &str,
    response: &GatewayResponse,
    expected: CacheOutcome,
) -> Result<()> {
    assert_eq!(response.status, TerminalStatus::Completed);
    assert!(response.usage.real_local_inference);
    assert!(!response.output.is_empty());
    assert_eq!(
        response
            .usage
            .cache
            .as_ref()
            .ok_or_else(|| anyhow!("gateway cache receipt missing"))?
            .outcome,
        expected,
        "unexpected cache outcome at `{stage}`"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
async fn real_product_gateway_cache_hierarchy_survives_clear_restart_off_and_corruption()
-> Result<()> {
    let Some(session) = configured_real_session("real-product-gateway-cache")? else {
        return Ok(());
    };
    mom_llama_runtime::settings_update(SettingsUpdate {
        max_tokens: Some(8),
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    let stable_system = format!(
        "This is an immutable local persona prefix used only for cache verification. {}",
        "Keep the complete prior statement in context and answer the next user briefly. "
            .repeat(32)
    );

    let cold = product_gateway_cache_request(
        &stable_system,
        "Return the word cold.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome("cold", &cold, CacheOutcome::Miss)?;
    let warm = product_gateway_cache_request(
        &stable_system,
        "Return the word warm.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome("warm", &warm, CacheOutcome::Hit)?;

    let cleared = mom_llama_runtime::kv_cache_clear()?;
    assert_eq!(cleared.readiness, "contracted");
    assert!(cleared.blocker.is_none());
    let after_clear = product_gateway_cache_request(
        &stable_system,
        "Return the words after clear.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome("after_clear", &after_clear, CacheOutcome::Miss)?;

    let settings = mom_llama_runtime::settings_get()?
        .result
        .ok_or_else(|| anyhow!("cache proof settings missing"))?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        resident_memory_budget_bytes: Some(
            settings
                .resident_memory_budget_bytes
                .saturating_add(1024 * 1024),
        ),
        ..SettingsUpdate::default()
    })?;
    let restored = product_gateway_cache_request(
        &stable_system,
        "Return the words after persistent restore.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome("restored", &restored, CacheOutcome::Hit)?;

    mom_llama_runtime::kv_cache_clear()?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::None),
        ..SettingsUpdate::default()
    })?;
    let off_first = product_gateway_cache_request(
        &stable_system,
        "Caching is off, first request.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    let off_second = product_gateway_cache_request(
        &stable_system,
        "Caching is off, second request.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome("off_first", &off_first, CacheOutcome::Miss)?;
    assert_gateway_cache_outcome("off_second", &off_second, CacheOutcome::Miss)?;
    assert!(
        !encrypted_document_snapshot(session.path())?
            .contains_key("native-host-prefix-cache.mom-llama"),
        "the Off policy must not write the shared FTE/native prefix document"
    );

    mom_llama_runtime::settings_update(SettingsUpdate {
        kv_cache_policy: Some(KvCachePolicy::PromptPrefix),
        ..SettingsUpdate::default()
    })?;
    let before_corruption = product_gateway_cache_request(
        &stable_system,
        "Checkpoint a prefix before corruption.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome("before_corruption", &before_corruption, CacheOutcome::Miss)?;
    let connection = rusqlite::Connection::open(session.path().join("runtime.sqlite3"))?;
    assert_eq!(
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00'
             WHERE namespace = 'native-host-prefix-cache.mom-llama'",
            [],
        )?,
        1
    );
    let settings = mom_llama_runtime::settings_get()?
        .result
        .ok_or_else(|| anyhow!("cache proof settings missing after re-enable"))?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        resident_memory_budget_bytes: Some(
            settings
                .resident_memory_budget_bytes
                .saturating_add(1024 * 1024),
        ),
        ..SettingsUpdate::default()
    })?;
    let corruption_fallback = product_gateway_cache_request(
        &stable_system,
        "Generate normally after quarantining corrupt cache bytes.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome(
        "corruption_fallback",
        &corruption_fallback,
        CacheOutcome::Miss,
    )?;
    let documents = encrypted_document_snapshot(session.path())?;
    assert!(
        documents
            .keys()
            .any(|namespace| namespace.starts_with("quarantine.disposable-cache."))
    );
    assert!(documents.contains_key("native-host-prefix-cache.mom-llama"));

    let warm_after_quarantine = product_gateway_cache_request(
        &stable_system,
        "Verify the replacement cache is warm.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome(
        "warm_after_quarantine",
        &warm_after_quarantine,
        CacheOutcome::Hit,
    )?;
    let settings = mom_llama_runtime::settings_get()?
        .result
        .ok_or_else(|| anyhow!("cache proof settings missing before fingerprint change"))?;
    mom_llama_runtime::settings_update(SettingsUpdate {
        // Use a configuration field whose effective value is not capped by a
        // small model's advertised context window. SmolLM2 clamps an oversized
        // context request back to 8K, which correctly preserves the fingerprint
        // and would make this a false invalidation test.
        batch_tokens: Some(settings.batch_tokens.saturating_add(1)),
        ..SettingsUpdate::default()
    })?;
    assert!(
        mom_llama_runtime::unload_resident_model(),
        "the prior fingerprint's resident worker should be unloadable without clearing cache state"
    );
    let fingerprint_miss = product_gateway_cache_request(
        &stable_system,
        "Generate normally after changing the model batch fingerprint.",
        CacheMode::Persistent,
        "v1",
    )
    .await?;
    assert_gateway_cache_outcome("fingerprint_miss", &fingerprint_miss, CacheOutcome::Miss)?;
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
        // This proof needs observable concurrent work and a non-empty
        // synthesis, not long prose from every seat.
        max_tokens: Some(64),
        ..SettingsUpdate::default()
    })?;
    let cancellation_panel = mom_llama_runtime::consult_panel_list()?
        .result
        .and_then(|panels| panels.into_iter().next())
        .ok_or_else(|| anyhow!("the legacy recovery panel is unavailable"))?;
    let cancellation_target = cancellation_panel
        .personas
        .last()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| anyhow!("the legacy recovery panel has no cancellation target"))?;
    let mut cancelled = None;
    let result = mom_llama_runtime::consult_start_stream(
        ConsultStartInput {
            conversation_id: "real-consult".to_string(),
            prompt: "Give a careful short plan for preparing a virtual consultation.".to_string(),
            panel_id: Some(cancellation_panel.id),
        },
        ConsultStartOptions::default(),
        Some(|event: mom_llama_runtime::ConsultStreamEvent| {
            if cancelled.is_none()
                && event.seat_id == cancellation_target
                && (matches!(
                    event.state,
                    Some(
                        llama_native_types::GenerationState::Prefilling
                            | llama_native_types::GenerationState::Generating
                    )
                ) || event.event == "delta")
            {
                let attempt =
                    mom_llama_runtime::consult_cancel(&event.run_id, Some(&cancellation_target))?;
                if attempt
                    .result
                    .as_ref()
                    .is_some_and(|result| result.cancelled_sequences == 1)
                {
                    cancelled = Some(attempt);
                }
            }
            Ok(())
        }),
    )?;
    let cancelled = cancelled.ok_or_else(|| {
        anyhow!("legacy consult seat `{cancellation_target}` never became cancellable")
    })?;
    assert_eq!(
        cancelled
            .result
            .as_ref()
            .map(|result| result.cancelled_sequences),
        Some(1)
    );
    let run = result
        .result
        .ok_or_else(|| anyhow!("consult result missing"))?;
    assert_eq!(run.seats.len(), 4);
    assert!(run.seats.iter().any(|seat| {
        seat.seat_id == cancellation_target
            && seat.state == llama_native_types::GenerationState::Cancelled
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
