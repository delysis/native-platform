use crate::chat::{build_native_messages, upstream_setting_bool};
use crate::config::{resolve_settings, upstream_setting_string};
use crate::conversation_store::{
    CONVERSATIONS_NAMESPACE, ChatTemplatePolicy, ConversationDb, ConversationKind,
    DRAFTS_NAMESPACE, active_path_messages, load_db, load_drafts,
};
use crate::native_runtime::resident_model_for_profile_if_loaded;
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::skill_store::applied_skill_prompt;
use crate::store::RuntimeStore;
use anyhow::Result;
use llama_native_engine::WaitOutcome;
use llama_native_types::{
    ChatTemplateChoice, GenerationInput, GenerationRequest, GenerationState, ModelFingerprint,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const COMMAND: &str = "mom_llama.composer_autocomplete";
const ACCEPT_COMMAND: &str = "mom_llama.composer_autocomplete_accept";
const MAX_DRAFT_BYTES: usize = 16 * 1024;
const MAX_CONTEXT_MESSAGES: usize = 64;
const MAX_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_NATIVE_INPUT_BYTES: usize = 384 * 1024;
const MAX_SUFFIX_BYTES: usize = 512;
const MAX_COMPLETION_TOKENS: u32 = 32;
const COMPLETION_WALL_LIMIT: Duration = Duration::from_secs(4);

#[derive(Debug)]
struct StaleAutocompleteAnchor;

impl std::fmt::Display for StaleAutocompleteAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the composer autocomplete anchor changed")
    }
}

impl std::error::Error for StaleAutocompleteAnchor {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposerAutocompleteInput {
    pub conversation_id: String,
    pub draft: String,
    pub active_leaf_message_id: Option<String>,
    pub execution_profile_version: u64,
    pub selection_start_utf16: u32,
    pub selection_end_utf16: u32,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposerAutocompleteAnchor {
    pub conversation_id: String,
    pub active_leaf_message_id: Option<String>,
    pub execution_profile_version: u64,
    pub draft: String,
    pub selection_start_utf16: u32,
    pub selection_end_utf16: u32,
    pub attachment_ids: Vec<String>,
    pub model_fingerprint_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposerAutocompleteOutput {
    pub request_id: String,
    pub anchor: ComposerAutocompleteAnchor,
    pub suffix: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposerAutocompleteAcceptOutput {
    pub anchor: ComposerAutocompleteAnchor,
    pub message: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposerAutocompleteCancelOutput {
    pub cancellation_requested_for: usize,
}

pub fn composer_autocomplete_accept(
    anchor: ComposerAutocompleteAnchor,
    suffix: String,
) -> Result<CommandResult<ComposerAutocompleteAcceptOutput>> {
    let input = ComposerAutocompleteInput {
        conversation_id: anchor.conversation_id.clone(),
        draft: anchor.draft.clone(),
        active_leaf_message_id: anchor.active_leaf_message_id.clone(),
        execution_profile_version: anchor.execution_profile_version,
        selection_start_utf16: anchor.selection_start_utf16,
        selection_end_utf16: anchor.selection_end_utf16,
        attachment_ids: anchor.attachment_ids.clone(),
    };
    if let Some(blocker) = draft_anchor_blocker(&input) {
        return Ok(CommandResult::blocked(
            ACCEPT_COMMAND,
            "host_integrated",
            blocker,
        ));
    }
    if bounded_suffix(&suffix, "").as_deref() != Some(suffix.as_str()) {
        return Ok(CommandResult::blocked(
            ACCEPT_COMMAND,
            "host_integrated",
            Blocker::new(
                "composer_autocomplete_invalid_suffix",
                "The transient continuation is no longer a bounded single-line suffix.",
                Vec::new(),
            ),
        ));
    }
    let Some(message_len) = anchor.draft.len().checked_add(suffix.len()) else {
        return Ok(stale_accept_anchor());
    };
    if message_len > MAX_DRAFT_BYTES {
        return Ok(CommandResult::blocked(
            ACCEPT_COMMAND,
            "host_integrated",
            Blocker::new(
                "composer_autocomplete_draft_out_of_bounds",
                format!("The accepted draft would exceed {MAX_DRAFT_BYTES} UTF-8 bytes."),
                Vec::new(),
            ),
        ));
    }

    let migrated_db = load_db()?;
    if migrated_db.selected_conversation_id.as_deref() != Some(anchor.conversation_id.as_str()) {
        return Ok(stale_accept_anchor());
    }
    let Some(conversation) = migrated_db
        .conversations
        .iter()
        .find(|conversation| conversation.id == anchor.conversation_id)
    else {
        return Ok(stale_accept_anchor());
    };
    if conversation.kind != ConversationKind::Chat
        || conversation.active_leaf_message_id != anchor.active_leaf_message_id
        || conversation.execution_profile.version != anchor.execution_profile_version
    {
        return Ok(stale_accept_anchor());
    }

    let mut settings = resolve_settings()?;
    settings.model_path = conversation
        .execution_profile
        .model_path
        .clone()
        .or_else(|| conversation.selected_model_path.clone())
        .or(settings.model_path);
    settings.mmproj_path = conversation
        .execution_profile
        .mmproj_path
        .clone()
        .or(settings.mmproj_path);
    let Some(model_path) = settings.model_path.clone() else {
        return Ok(stale_accept_anchor());
    };
    let handle = match resident_model_for_profile_if_loaded(
        &settings,
        &model_path,
        settings.mmproj_path.as_deref(),
    ) {
        Ok(Some(handle)) => handle,
        Ok(None) => return Ok(stale_accept_anchor()),
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                ACCEPT_COMMAND,
                &blocker.readiness,
                blocker.blocker,
            ));
        }
    };
    let status = handle.status();
    let Some(fingerprint) = status.fingerprint.as_ref() else {
        return Ok(stale_accept_anchor());
    };
    if fingerprint_sha256(fingerprint)? != anchor.model_fingerprint_sha256 {
        return Ok(stale_accept_anchor());
    }

    let migrated_drafts = load_drafts()?;
    let store = RuntimeStore::current()?;
    let accepted_message = format!("{}{suffix}", anchor.draft);
    let accepted = store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        || migrated_db,
        |conversations: &mut ConversationDb, documents| {
            let exact_conversation = conversations.selected_conversation_id.as_deref()
                == Some(anchor.conversation_id.as_str())
                && conversations.conversations.iter().any(|conversation| {
                    conversation.id == anchor.conversation_id
                        && conversation.kind == ConversationKind::Chat
                        && conversation.active_leaf_message_id == anchor.active_leaf_message_id
                        && conversation.execution_profile.version
                            == anchor.execution_profile_version
                });
            if !exact_conversation {
                return Err(anyhow::Error::new(StaleAutocompleteAnchor));
            }
            let mut drafts = documents.get(DRAFTS_NAMESPACE)?.unwrap_or(migrated_drafts);
            let Some(draft) = drafts.drafts.iter_mut().find(|draft| {
                draft.conversation_id.as_deref() == Some(anchor.conversation_id.as_str())
            }) else {
                return Err(anyhow::Error::new(StaleAutocompleteAnchor));
            };
            if draft.message != anchor.draft || draft.attachment_ids != anchor.attachment_ids {
                return Err(anyhow::Error::new(StaleAutocompleteAnchor));
            }
            draft.message = accepted_message.clone();
            draft.updated_at = now_ms().to_string();
            let accepted = draft.clone();
            documents.put_bytes(DRAFTS_NAMESPACE, &serde_json::to_vec(&drafts)?)?;
            Ok(accepted)
        },
    );
    let accepted = match accepted {
        Ok(accepted) => accepted,
        Err(error) if error.is::<StaleAutocompleteAnchor>() => return Ok(stale_accept_anchor()),
        Err(error) => return Err(error),
    };

    Ok(CommandResult::passed(
        ACCEPT_COMMAND,
        "host_integrated",
        ComposerAutocompleteAcceptOutput {
            anchor,
            message: accepted.message,
            updated_at: accepted.updated_at,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn composer_autocomplete_supervised(
    input: ComposerAutocompleteInput,
    request_id: String,
    cancellation_requested: impl Fn() -> bool,
) -> Result<CommandResult<ComposerAutocompleteOutput>> {
    let started = Instant::now();
    let mut settings = resolve_settings()?;
    let db = load_db()?;
    if db.selected_conversation_id.as_deref() != Some(input.conversation_id.as_str()) {
        return Ok(stale_anchor());
    }
    let Some(conversation) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == input.conversation_id)
        .cloned()
    else {
        return Ok(blocked(
            "composer_conversation_not_found",
            "The active conversation is no longer available.",
        ));
    };
    if conversation.kind != ConversationKind::Chat {
        return Ok(blocked(
            "composer_autocomplete_unavailable",
            "Suggestions are available only in an active conversation.",
        ));
    }
    if conversation.active_leaf_message_id != input.active_leaf_message_id
        || conversation.execution_profile.version != input.execution_profile_version
    {
        return Ok(stale_anchor());
    }
    if let Some(blocker) = draft_anchor_blocker(&input) {
        return Ok(CommandResult::blocked(COMMAND, "host_integrated", blocker));
    }
    let drafts = load_drafts()?;
    let Some(draft) = drafts
        .drafts
        .iter()
        .find(|draft| draft.conversation_id.as_deref() == Some(input.conversation_id.as_str()))
    else {
        return Ok(stale_anchor());
    };
    if draft.message != input.draft || draft.attachment_ids != input.attachment_ids {
        return Ok(stale_anchor());
    }
    if cancellation_requested() {
        return Ok(cancelled(false));
    }

    settings.model_path = conversation
        .execution_profile
        .model_path
        .clone()
        .or_else(|| conversation.selected_model_path.clone())
        .or(settings.model_path);
    settings.mmproj_path = conversation
        .execution_profile
        .mmproj_path
        .clone()
        .or(settings.mmproj_path);
    let Some(model_path) = settings.model_path.clone() else {
        return Ok(blocked(
            "composer_autocomplete_model_unavailable",
            "Suggestions require the conversation model to already be loaded.",
        ));
    };
    let handle = match resident_model_for_profile_if_loaded(
        &settings,
        &model_path,
        settings.mmproj_path.as_deref(),
    ) {
        Ok(Some(handle)) => handle,
        Ok(None) => {
            return Ok(blocked(
                "composer_autocomplete_model_unavailable",
                "Suggestions require the conversation model to already be loaded.",
            ));
        }
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                COMMAND,
                &blocker.readiness,
                blocker.blocker,
            ));
        }
    };
    let status = handle.status();
    let Some(fingerprint) = status.fingerprint.as_ref() else {
        return Ok(blocked(
            "composer_autocomplete_model_identity_unavailable",
            "The loaded conversation model did not expose an immutable identity.",
        ));
    };
    let model_fingerprint_sha256 = fingerprint_sha256(fingerprint)?;
    let active_messages = active_path_messages(&conversation);
    if let Some(blocker) = context_bound_blocker(&active_messages) {
        return Ok(CommandResult::blocked(COMMAND, "host_integrated", blocker));
    }
    let skill_prompt = applied_skill_prompt(&conversation.current_skill_ids)?;
    let system_message = conversation
        .execution_profile
        .system_message
        .clone()
        .unwrap_or_else(|| upstream_setting_string(&settings, "systemMessage").unwrap_or_default());
    let prediction_prompt = prediction_prompt(&input.draft);
    let messages = build_native_messages(
        &system_message,
        &skill_prompt.prompt,
        &active_messages,
        &prediction_prompt,
        upstream_setting_bool(&settings, "excludeReasoningFromContext"),
        &std::collections::HashMap::new(),
        "",
    );
    if let Some(blocker) =
        native_input_bound_blocker(&messages, &conversation.execution_profile.chat_template)
    {
        return Ok(CommandResult::blocked(COMMAND, "host_integrated", blocker));
    }
    let mut sampling = conversation
        .execution_profile
        .sampling
        .clone()
        .unwrap_or_else(|| settings.sampling_config());
    sampling.max_tokens = MAX_COMPLETION_TOKENS;
    sampling.stop = vec!["\n".to_string(), "\r".to_string()];
    if cancellation_requested() {
        return Ok(cancelled(false));
    }
    let Some(remaining_wall_time) = COMPLETION_WALL_LIMIT.checked_sub(started.elapsed()) else {
        return Ok(blocked(
            "composer_autocomplete_timed_out",
            "The speculative suggestion exceeded its four-second limit before model admission.",
        ));
    };
    let ticket = match handle.generate_speculative(GenerationRequest {
        request_id: request_id.clone(),
        model_id: status.model_id,
        input: GenerationInput::Chat {
            messages,
            template: match &conversation.execution_profile.chat_template {
                ChatTemplatePolicy::ModelDefault => ChatTemplateChoice::ModelDefault,
                ChatTemplatePolicy::FrozenSource(template) => {
                    ChatTemplateChoice::Override(template.clone())
                }
            },
        },
        sampling,
        media: Vec::new(),
        cached_prefix: None,
    }) {
        Ok(ticket) => ticket,
        Err(error) => {
            return Ok(blocked(
                "composer_autocomplete_admission_unavailable",
                &format!("The loaded model did not admit a speculative suggestion: {error}"),
            ));
        }
    };
    if cancellation_requested() {
        ticket.cancel_all();
    }
    let outputs = match ticket.wait_timeout(remaining_wall_time) {
        Ok(WaitOutcome::Ready(outputs)) => outputs,
        Ok(WaitOutcome::TimedOut(ticket)) => {
            ticket.cancel_all();
            drop(ticket);
            return Ok(blocked_with_engine(
                "composer_autocomplete_timed_out",
                "The speculative suggestion exceeded its four-second limit.",
            ));
        }
        Err(error) => {
            return Ok(blocked_with_engine(
                "composer_autocomplete_failed",
                &format!("The speculative suggestion failed: {error}"),
            ));
        }
    };
    if cancellation_requested() {
        return Ok(cancelled(true));
    }
    let Some(output) = outputs.into_iter().next() else {
        return Ok(blocked_with_engine(
            "composer_autocomplete_empty",
            "The speculative suggestion returned no output.",
        ));
    };
    if output.state == GenerationState::Cancelled {
        return Ok(cancelled(true));
    }
    if output.state != GenerationState::Completed {
        return Ok(blocked_with_engine(
            "composer_autocomplete_failed",
            "The speculative suggestion did not reach a completed terminal state.",
        ));
    }
    let Some(suffix) = bounded_suffix(&output.text, &input.draft) else {
        return Ok(blocked_with_engine(
            "composer_autocomplete_empty",
            "The model did not return a usable single-line continuation.",
        ));
    };
    if input
        .draft
        .len()
        .checked_add(suffix.len())
        .is_none_or(|accepted_bytes| accepted_bytes > MAX_DRAFT_BYTES)
    {
        return Ok(blocked_with_engine(
            "composer_autocomplete_draft_out_of_bounds",
            "The speculative continuation would exceed the bounded composer draft.",
        ));
    }
    Ok(CommandResult::passed(
        COMMAND,
        "host_integrated",
        ComposerAutocompleteOutput {
            request_id,
            anchor: ComposerAutocompleteAnchor {
                conversation_id: input.conversation_id,
                active_leaf_message_id: input.active_leaf_message_id,
                execution_profile_version: input.execution_profile_version,
                draft: input.draft,
                selection_start_utf16: input.selection_start_utf16,
                selection_end_utf16: input.selection_end_utf16,
                attachment_ids: input.attachment_ids,
                model_fingerprint_sha256,
            },
            suffix,
            duration_ms: started.elapsed().as_millis(),
        },
        Vec::new(),
        Vec::new(),
        true,
        false,
    ))
}

fn draft_anchor_blocker(input: &ComposerAutocompleteInput) -> Option<Blocker> {
    if input.draft.is_empty() || input.draft.len() > MAX_DRAFT_BYTES {
        return Some(Blocker::new(
            "composer_autocomplete_draft_out_of_bounds",
            format!("The composer draft must contain 1..={MAX_DRAFT_BYTES} UTF-8 bytes."),
            Vec::new(),
        ));
    }
    let Ok(utf16_len) = u32::try_from(input.draft.encode_utf16().count()) else {
        return Some(Blocker::new(
            "composer_autocomplete_draft_out_of_bounds",
            "The composer draft is too large to anchor safely.",
            Vec::new(),
        ));
    };
    if input.selection_start_utf16 != utf16_len || input.selection_end_utf16 != utf16_len {
        return Some(Blocker::new(
            "composer_autocomplete_stale_anchor",
            "The composer selection is no longer collapsed at the exact draft end.",
            Vec::new(),
        ));
    }
    if has_active_mention_token(&input.draft) {
        return Some(Blocker::new(
            "composer_autocomplete_mention_active",
            "Suggestions are paused while an @mention token is active.",
            Vec::new(),
        ));
    }
    if !input.attachment_ids.is_empty() {
        return Some(Blocker::new(
            "composer_autocomplete_attachments_present",
            "Suggestions are paused while attachments are staged.",
            Vec::new(),
        ));
    }
    None
}

fn context_bound_blocker(messages: &[crate::conversation_store::Message]) -> Option<Blocker> {
    if messages.len() > MAX_CONTEXT_MESSAGES {
        return Some(Blocker::new(
            "composer_autocomplete_context_too_large",
            format!(
                "Suggestions are paused when the active path exceeds {MAX_CONTEXT_MESSAGES} messages."
            ),
            Vec::new(),
        ));
    }
    let Some(bytes) = messages.iter().try_fold(0_usize, |total, message| {
        total.checked_add(message.content.len())
    }) else {
        return Some(Blocker::new(
            "composer_autocomplete_context_too_large",
            "The active conversation path is too large to predict safely.",
            Vec::new(),
        ));
    };
    if bytes > MAX_CONTEXT_BYTES {
        return Some(Blocker::new(
            "composer_autocomplete_context_too_large",
            format!(
                "Suggestions are paused when the active path exceeds {MAX_CONTEXT_BYTES} UTF-8 bytes."
            ),
            Vec::new(),
        ));
    }
    None
}

fn native_input_bound_blocker(
    messages: &[llama_native_types::ChatMessage],
    template: &ChatTemplatePolicy,
) -> Option<Blocker> {
    let message_bytes = messages.iter().try_fold(0_usize, |total, message| {
        total.checked_add(message.content.len())
    });
    let template_bytes = match template {
        ChatTemplatePolicy::ModelDefault => 0,
        ChatTemplatePolicy::FrozenSource(template) => template.len(),
    };
    let Some(bytes) = message_bytes.and_then(|bytes| bytes.checked_add(template_bytes)) else {
        return Some(Blocker::new(
            "composer_autocomplete_context_too_large",
            "The exact autocomplete input is too large to predict safely.",
            Vec::new(),
        ));
    };
    (bytes > MAX_NATIVE_INPUT_BYTES).then(|| {
        Blocker::new(
            "composer_autocomplete_context_too_large",
            format!(
                "Suggestions are paused when the exact chat input exceeds {MAX_NATIVE_INPUT_BYTES} UTF-8 bytes."
            ),
            Vec::new(),
        )
    })
}

fn has_active_mention_token(draft: &str) -> bool {
    let token = draft
        .rsplit_once(char::is_whitespace)
        .map_or(draft, |(_, token)| token);
    token.strip_prefix('@').is_some_and(|handle| {
        handle
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    })
}

fn prediction_prompt(draft: &str) -> String {
    format!(
        "Continue the unfinished user message below in the user's voice. Return only text that begins immediately after its final character. Do not answer, explain, quote, label, or use more than one line.\n\n<unfinished-user-message>\n{draft}\n</unfinished-user-message>"
    )
}

fn bounded_suffix(output: &str, draft: &str) -> Option<String> {
    let first_line = output.split(['\r', '\n']).next().unwrap_or_default();
    let candidate = first_line.strip_prefix(draft).unwrap_or(first_line);
    let candidate = candidate.trim_end_matches(char::is_whitespace);
    if candidate.trim().is_empty() || candidate.chars().any(char::is_control) {
        return None;
    }
    let end = candidate
        .char_indices()
        .map(|(index, character)| index + character.len_utf8())
        .take_while(|end| *end <= MAX_SUFFIX_BYTES)
        .last()?;
    Some(candidate[..end].to_string())
}

fn fingerprint_sha256(fingerprint: &ModelFingerprint) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(fingerprint)?)
    ))
}

fn stale_anchor() -> CommandResult<ComposerAutocompleteOutput> {
    blocked(
        "composer_autocomplete_stale_anchor",
        "The conversation or draft changed before the suggestion was admitted.",
    )
}

fn stale_accept_anchor() -> CommandResult<ComposerAutocompleteAcceptOutput> {
    CommandResult::blocked(
        ACCEPT_COMMAND,
        "host_integrated",
        Blocker::new(
            "composer_autocomplete_stale_anchor",
            "The conversation, model, draft, or selection changed before acceptance.",
            Vec::new(),
        ),
    )
}

fn cancelled(engine_invoked: bool) -> CommandResult<ComposerAutocompleteOutput> {
    CommandResult::blocked_with_evidence(
        COMMAND,
        "host_integrated",
        Blocker::new(
            "composer_autocomplete_cancelled",
            "The speculative suggestion was cancelled by newer foreground work.",
            Vec::new(),
        ),
        Vec::new(),
        Vec::new(),
        engine_invoked,
        false,
    )
}

fn blocked(code: &str, message: &str) -> CommandResult<ComposerAutocompleteOutput> {
    CommandResult::blocked(
        COMMAND,
        "host_integrated",
        Blocker::new(code, message, Vec::new()),
    )
}

fn blocked_with_engine(code: &str, message: &str) -> CommandResult<ComposerAutocompleteOutput> {
    CommandResult::blocked_with_evidence(
        COMMAND,
        "host_integrated",
        Blocker::new(code, message, Vec::new()),
        Vec::new(),
        Vec::new(),
        true,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ComposerAutocompleteInput, MAX_SUFFIX_BYTES, bounded_suffix, draft_anchor_blocker,
        has_active_mention_token,
    };

    fn input(draft: &str) -> ComposerAutocompleteInput {
        let end = u32::try_from(draft.encode_utf16().count()).expect("small fixture");
        ComposerAutocompleteInput {
            conversation_id: "conversation".to_string(),
            draft: draft.to_string(),
            active_leaf_message_id: Some("leaf".to_string()),
            execution_profile_version: 7,
            selection_start_utf16: end,
            selection_end_utf16: end,
            attachment_ids: Vec::new(),
        }
    }

    #[test]
    fn anchor_requires_the_exact_utf16_draft_end() {
        let mut anchored = input("hello 🙂");
        assert!(draft_anchor_blocker(&anchored).is_none());
        anchored.selection_start_utf16 -= 1;
        assert!(draft_anchor_blocker(&anchored).is_some());
        anchored.selection_start_utf16 = anchored.selection_end_utf16;
        anchored.selection_end_utf16 -= 1;
        assert!(draft_anchor_blocker(&anchored).is_some());
    }

    #[test]
    fn active_mentions_suppress_prediction_without_matching_email_prose() {
        assert!(has_active_mention_token("ask @mom"));
        assert!(has_active_mention_token("@"));
        assert!(!has_active_mention_token("email me@example.com"));
        assert!(!has_active_mention_token("ask @mom!"));
    }

    #[test]
    fn suffix_is_single_line_echo_stripped_and_utf8_bounded() {
        assert_eq!(
            bounded_suffix(" draft more\nignored", " draft").as_deref(),
            Some(" more")
        );
        assert!(bounded_suffix("\nignored", "draft").is_none());
        let oversized = "🙂".repeat(MAX_SUFFIX_BYTES);
        let suffix = bounded_suffix(&oversized, "draft").expect("bounded suffix");
        assert!(suffix.len() <= MAX_SUFFIX_BYTES);
        assert!(suffix.is_char_boundary(suffix.len()));
    }
}
