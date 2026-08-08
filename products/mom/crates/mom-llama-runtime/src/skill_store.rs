use crate::config::{KvCachePolicy, resolve_settings};
use crate::conversation_store::{Conversation, load_db, save_db};
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use uuid::Uuid;

const SKILLS_FILE: &str = "skills.json";
const SKILLS_NAMESPACE: &str = "skills.v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub prompt_template: String,
    pub usage_hint: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub cache_policy: KvCachePolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SkillDb {
    #[serde(default)]
    pub skills: Vec<Skill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedSkillPrompt {
    pub prompt: String,
    pub cache_owner_id: Option<String>,
    pub cache_label: String,
}

pub fn skill_create(
    name: String,
    description: String,
    prompt_template: String,
    usage_hint: String,
    cache_policy: KvCachePolicy,
) -> Result<CommandResult<Skill>> {
    if name.trim().is_empty() || prompt_template.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.skill_create",
            "stub_blocked",
            Blocker::new(
                "skill_fields_missing",
                "A Skill needs a name and prompt.",
                vec!["Enter a short name and a reusable prompt.".to_string()],
            ),
        ));
    }
    let now = now_ms().to_string();
    let skill = Skill {
        id: Uuid::new_v4().to_string(),
        name: name.trim().to_string(),
        description: description.trim().to_string(),
        prompt_template: prompt_template.trim().to_string(),
        usage_hint: usage_hint.trim().to_string(),
        tags: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
        cache_policy,
    };
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<SkillDb>(SKILLS_NAMESPACE, &settings.data_dir.join(SKILLS_FILE))?;
    store.mutate(SKILLS_NAMESPACE, SkillDb::default, |db: &mut SkillDb| {
        db.skills.insert(0, skill.clone());
        Ok(())
    })?;
    let path = store.path().to_path_buf();
    Ok(CommandResult::passed(
        "mom_llama.skill_create",
        "contracted",
        skill,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn skill_update(
    skill_id: &str,
    name: String,
    description: String,
    prompt_template: String,
    usage_hint: String,
    cache_policy: KvCachePolicy,
) -> Result<CommandResult<Skill>> {
    if name.trim().is_empty() || prompt_template.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.skill_update",
            "stub_blocked",
            Blocker::new(
                "skill_fields_missing",
                "A Skill needs a name and prompt.",
                vec!["Enter a short name and a reusable prompt.".to_string()],
            ),
        ));
    }
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<SkillDb>(SKILLS_NAMESPACE, &settings.data_dir.join(SKILLS_FILE))?;
    let updated = store.mutate(SKILLS_NAMESPACE, SkillDb::default, |db: &mut SkillDb| {
        let Some(skill) = db.skills.iter_mut().find(|skill| skill.id == skill_id) else {
            return Ok(None);
        };
        skill.name = name.trim().to_string();
        skill.description = description.trim().to_string();
        skill.prompt_template = prompt_template.trim().to_string();
        skill.usage_hint = usage_hint.trim().to_string();
        skill.cache_policy = cache_policy;
        skill.updated_at = now_ms().to_string();
        Ok(Some(skill.clone()))
    })?;
    let Some(skill) = updated else {
        return Ok(CommandResult::blocked(
            "mom_llama.skill_update",
            "stub_blocked",
            Blocker::new(
                "skill_not_found",
                format!("Skill {skill_id} was not found."),
                vec!["Refresh the Skills list.".to_string()],
            ),
        ));
    };
    Ok(CommandResult::passed(
        "mom_llama.skill_update",
        "contracted",
        skill,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn skill_list() -> Result<CommandResult<Vec<Skill>>> {
    let db = load_skill_db()?;
    Ok(CommandResult::passed(
        "mom_llama.skill_list",
        "contracted",
        db.skills,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn skill_apply(
    conversation_id: &str,
    skill_id_or_name: &str,
) -> Result<CommandResult<Conversation>> {
    let skill_db = load_skill_db()?;
    let Some(skill) = skill_db
        .skills
        .iter()
        .find(|skill| skill.id == skill_id_or_name || skill.name == skill_id_or_name)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.skill_apply",
            "stub_blocked",
            Blocker::new(
                "skill_not_found",
                format!("Skill {skill_id_or_name} was not found."),
                vec!["Run `mom-llama skill list --json`.".to_string()],
            ),
        ));
    };
    let mut conversation_db = load_db()?;
    let Some(conversation) = conversation_db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.skill_apply",
            "stub_blocked",
            Blocker::new(
                "conversation_not_found",
                format!("Conversation {conversation_id} was not found."),
                vec!["Run `mom-llama conversation list --json`.".to_string()],
            ),
        ));
    };
    if !conversation.current_skill_ids.contains(&skill.id) {
        conversation.current_skill_ids.push(skill.id.clone());
    }
    conversation.updated_at = now_ms().to_string();
    let result = conversation.clone();
    let path = save_db(&conversation_db)?;
    Ok(CommandResult::passed(
        "mom_llama.skill_apply",
        "contracted",
        result,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn load_skill_db() -> Result<SkillDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<SkillDb>(SKILLS_NAMESPACE, &settings.data_dir.join(SKILLS_FILE))?;
    Ok(store.get(SKILLS_NAMESPACE)?.unwrap_or_default())
}

pub fn save_skill_db(db: &SkillDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(SKILLS_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

pub fn applied_skill_prompt(skill_ids: &[String]) -> Result<AppliedSkillPrompt> {
    let db = load_skill_db()?;
    let skills = skill_ids
        .iter()
        .filter_map(|id| db.skills.iter().find(|skill| &skill.id == id))
        .collect::<Vec<_>>();
    let prompt = skills
        .iter()
        .map(|skill| skill.prompt_template.trim())
        .filter(|prompt| !prompt.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let cacheable = !skills.is_empty()
        && skills
            .iter()
            .all(|skill| skill.cache_policy.allows_prefix_reuse());
    let cache_owner_id = cacheable.then(|| {
        let mut hash = Sha256::new();
        for skill in &skills {
            hash.update(skill.id.as_bytes());
            hash.update([0]);
            hash.update(skill.updated_at.as_bytes());
            hash.update([0]);
            hash.update(skill.prompt_template.as_bytes());
            hash.update([0]);
        }
        format!("skills:{:x}", hash.finalize())
    });
    let cache_label = if skills.is_empty() {
        "Applied Skills".to_string()
    } else {
        format!(
            "Applied Skills: {}",
            skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Ok(AppliedSkillPrompt {
        prompt,
        cache_owner_id,
        cache_label,
    })
}
