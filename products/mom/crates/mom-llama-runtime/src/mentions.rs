fn resolve_targets(handles: &[String], host_id: &str) -> Result<TargetResolution> {
    let (conversations, groups) = conversation_and_group_handles()?;
    Ok(resolve_targets_from_registry(
        handles,
        host_id,
        &conversations,
        &groups,
    ))
}

fn resolve_targets_from_registry(
    handles: &[String],
    host_id: &str,
    conversations: &[Conversation],
    groups: &[crate::personas::PersonaGroup],
) -> TargetResolution {
    let mut by_handle = BTreeMap::<String, Vec<&Conversation>>::new();
    for conversation in conversations {
        by_handle
            .entry(
                conversation
                    .execution_profile
                    .mention_handle
                    .to_ascii_lowercase(),
            )
            .or_default()
            .push(conversation);
    }
    let mut groups_by_handle = BTreeMap::<String, Vec<&crate::personas::PersonaGroup>>::new();
    for group in groups {
        groups_by_handle
            .entry(group.mention_handle.to_ascii_lowercase())
            .or_default()
            .push(group);
    }
    let by_id = conversations
        .iter()
        .map(|conversation| (conversation.id.as_str(), conversation))
        .collect::<BTreeMap<_, _>>();
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    let mut ambiguous = Vec::new();
    let mut seen = BTreeSet::new();
    for handle in handles {
        let conversation_matches = by_handle.get(handle).map(Vec::as_slice).unwrap_or_default();
        let group_matches = groups_by_handle
            .get(handle)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if conversation_matches.len() + group_matches.len() > 1 {
            ambiguous.push(handle.clone());
            continue;
        }
        if let Some(group) = group_matches.first() {
            let mut group_targets = Vec::new();
            let mut group_seen = BTreeSet::new();
            let mut valid = !group.persona_ids.is_empty();
            for id in &group.persona_ids {
                if let Some(conversation) = by_id.get(id.as_str())
                    && conversation.kind == ConversationKind::PersonaTemplate
                    && group_seen.insert(conversation.id.clone())
                {
                    group_targets.push(ResolvedTarget {
                        kind: MentionTargetKind::Persona,
                        conversation: (*conversation).clone(),
                    });
                } else {
                    valid = false;
                }
            }
            if valid {
                for target in group_targets {
                    if seen.insert(target.conversation.id.clone()) {
                        resolved.push(target);
                    }
                }
            } else {
                unresolved.push(handle.clone());
            }
        } else if let Some(conversation) = conversation_matches.first()
            && conversation.id != host_id
            && seen.insert(conversation.id.clone())
        {
            resolved.push(ResolvedTarget {
                kind: if conversation.kind == ConversationKind::PersonaTemplate {
                    MentionTargetKind::Persona
                } else {
                    MentionTargetKind::LiveChat
                },
                conversation: (*conversation).clone(),
            });
        } else {
            unresolved.push(handle.clone());
        }
    }
    TargetResolution {
        targets: resolved,
        unresolved,
        ambiguous,
    }
}

fn snapshot_target(target: &ResolvedTarget) -> Result<MentionTargetSnapshot> {
