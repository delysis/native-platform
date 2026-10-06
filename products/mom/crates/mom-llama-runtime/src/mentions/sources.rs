//! Citation metadata is descriptive. Only a retained committed invocation can
//! authorize opening its exact source occurrence, in one store snapshot.
use super::*;
use crate::attachments::{SelectedAttachmentSource, open_selected_source_in_snapshot};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsultSourcePreview {
    pub source: SelectedAttachmentSource,
    pub canonical_text: String,
}

#[derive(Deserialize)]
struct RetainedInput {
    invocation_id: String,
    host_conversation_id: String,
    user_message_id: String,
    planned_input: RetainedPlan,
}

#[derive(Deserialize)]
struct RetainedPlan {
    schema: String,
    target_id: String,
    snapshot_sha256: String,
    attachment_sources: Vec<SelectedAttachmentSource>,
}

fn retained_sources(
    snapshot: &DocumentSnapshot<'_, '_, '_>,
    invocation_id: &str,
    target_id: &str,
) -> Result<Vec<SelectedAttachmentSource>> {
    let db = snapshot
        .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
        .unwrap_or_default();
    let mut matches = db
        .invocations
        .iter()
        .filter(|invocation| invocation.id == invocation_id);
    let invocation = matches
        .next()
        .ok_or_else(|| anyhow!("consult invocation is missing"))?;
    if matches.next().is_some() {
        return Err(anyhow!("consult invocation identity is ambiguous"));
    }
    let results = invocation
        .results
        .iter()
        .filter(|result| result.target_id == target_id)
        .collect::<Vec<_>>();
    if results.len() != 1
        || results[0].message_id.is_none()
        || results[0].fake_fixture
        || !results[0].real_engine_invoked
    {
        return Err(anyhow!("consult target has no committed native reply"));
    }
    let key = sha256_json(&(invocation_id, target_id))?;
    let retained = snapshot
        .get::<RetainedInput>(&format!("mention-input.v1.{key}"))?
        .ok_or_else(|| anyhow!("consult input receipt is missing"))?;
    if retained.invocation_id != invocation.id
        || retained.host_conversation_id != invocation.host_conversation_id
        || retained.user_message_id != invocation.user_message_id
        || retained.planned_input.schema != "mom_llama.consult_input.v1"
        || retained.planned_input.target_id != target_id
        || invocation
            .targets
            .iter()
            .filter(|target| {
                target.target_id == target_id
                    && target.snapshot_sha256 == retained.planned_input.snapshot_sha256
            })
            .count()
            != 1
    {
        return Err(anyhow!(
            "consult input receipt does not match its committed target"
        ));
    }
    let mut sources = Vec::new();
    for source in retained.planned_input.attachment_sources {
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    Ok(sources)
}

pub fn consult_sources(
    invocation_id: &str,
    target_id: &str,
) -> Result<CommandResult<Vec<SelectedAttachmentSource>>> {
    RuntimeStore::current()?.read_documents(|snapshot| {
        match retained_sources(snapshot, invocation_id, target_id) {
            Ok(sources) => Ok(CommandResult::passed(
                "mom_llama.consult_sources",
                "contracted",
                sources,
                Vec::new(),
                Vec::new(),
                false,
                false,
            )),
            Err(error) => Ok(CommandResult::blocked(
                "mom_llama.consult_sources",
                "contracted",
                Blocker::new("consult_source_unavailable", error.to_string(), Vec::new()),
            )),
        }
    })
}

pub fn consult_source_open(
    invocation_id: &str,
    target_id: &str,
    conversation_id: &str,
    message_id: &str,
    attachment_id: &str,
) -> Result<CommandResult<ConsultSourcePreview>> {
    RuntimeStore::current()?.read_documents(|snapshot| {
        let open = || -> Result<ConsultSourcePreview> {
            let sources = retained_sources(snapshot, invocation_id, target_id)?;
            let mut matches = sources.into_iter().filter(|source| {
                source.conversation_id == conversation_id
                    && source.message_id == message_id
                    && source.representation.attachment_id == attachment_id
            });
            let source = matches.next().ok_or_else(|| {
                anyhow!("the requested occurrence was not selected by this consult")
            })?;
            if matches.next().is_some() {
                return Err(anyhow!("the selected source occurrence is ambiguous"));
            }
            let canonical_text = open_selected_source_in_snapshot(snapshot, &source)?;
            Ok(ConsultSourcePreview {
                source,
                canonical_text,
            })
        };
        match open() {
            Ok(preview) => Ok(CommandResult::passed(
                "mom_llama.consult_source_open",
                "contracted",
                preview,
                Vec::new(),
                Vec::new(),
                false,
                false,
            )),
            Err(error) => Ok(CommandResult::blocked(
                "mom_llama.consult_source_open",
                "contracted",
                Blocker::new(
                    if error.downcast_ref::<StaleConsultSource>().is_some() {
                        "consult_source_stale"
                    } else {
                        "consult_source_unavailable"
                    },
                    error.to_string(),
                    Vec::new(),
                ),
            )),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_sources_require_the_exact_committed_target_and_current_input_schema() -> Result<()>
    {
        let data_dir =
            std::env::temp_dir().join(format!("mom-consult-source-authority-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir)?;
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _remove = Remove(data_dir.clone());
        let store = RuntimeStore::open(&data_dir)?;
        let invocation = MentionInvocation {
            id: "invocation".into(),
            host_conversation_id: "host".into(),
            user_message_id: "user".into(),
            addressed_message: "@mom consult".into(),
            host_context: Vec::new(),
            targets: vec![MentionTargetSnapshot {
                target_id: "persona".into(),
                kind: MentionTargetKind::Persona,
                handle: "mom".into(),
                label: "Mom".into(),
                version: 1,
                source_leaf_message_id: None,
                profile: ConversationExecutionProfile::default(),
                source_messages: Vec::new(),
                snapshot_sha256: "snapshot".into(),
            }],
            // Store-admission fixture only: these flags do not certify a model.
            results: vec![MentionTargetResult {
                target_id: "persona".into(),
                handle: "mom".into(),
                label: "Mom".into(),
                state: GenerationState::Completed,
                text: "retained reply".into(),
                model_id: "test".into(),
                message_id: Some("reply".into()),
                metrics: GenerationMetrics::default(),
                cache_id: None,
                cache_reused: false,
                tool_receipt_ids: Vec::new(),
                real_engine_invoked: true,
                fake_fixture: false,
            }],
            tool_approvals: Vec::new(),
            synthesis_message_id: None,
            synthesis_sha256: None,
            state: MentionInvocationState::Completed,
            created_at: "1".into(),
            updated_at: "1".into(),
        };
        let mut db = MentionInvocationDb {
            invocations: vec![StoredMentionInvocation {
                invocation,
                frozen_tool_continuations: Vec::new(),
            }],
        };
        store.put(INVOCATIONS_NAMESPACE, &db)?;
        let key = sha256_json(&("invocation", "persona"))?;
        let namespace = format!("mention-input.v1.{key}");
        let mut input = json!({ "invocation_id": "invocation", "host_conversation_id": "host", "user_message_id": "user", "planned_input": {
            "schema": "mom_llama.consult_input.v1", "target_id": "persona", "snapshot_sha256": "snapshot", "attachment_sources": []
        }});
        store.put(&namespace, &input)?;
        store.read_documents(|snapshot| {
            assert!(retained_sources(snapshot, "invocation", "persona")?.is_empty());
            Ok(())
        })?;
        input["planned_input"]["snapshot_sha256"] = json!("another-snapshot");
        store.put(&namespace, &input)?;
        store.read_documents(|snapshot| {
            assert!(retained_sources(snapshot, "invocation", "persona").is_err());
            Ok(())
        })?;
        input["planned_input"]["snapshot_sha256"] = json!("snapshot");
        input["planned_input"]["schema"] = json!("old-input-schema");
        store.put(&namespace, &input)?;
        store.read_documents(|snapshot| {
            assert!(retained_sources(snapshot, "invocation", "persona").is_err());
            Ok(())
        })?;
        input["planned_input"]["schema"] = json!("mom_llama.consult_input.v1");
        store.put(&namespace, &input)?;
        db.invocations[0].results[0].fake_fixture = true;
        store.put(INVOCATIONS_NAMESPACE, &db)?;
        store.read_documents(|snapshot| {
            assert!(retained_sources(snapshot, "invocation", "persona").is_err());
            assert!(retained_sources(snapshot, "invocation", "another-target").is_err());
            Ok(())
        })?;
        Ok(())
    }
}
