//! Resolve the already-parsed handles used by dispatch and regression tests.
//! Lexing remains in workspace-document; this module performs no I/O or inference.

use super::*;

pub(super) fn checked_resolve_targets_from_registry(
    handles: &[String],
    host_id: &str,
    conversations: &[Conversation],
    groups: &[crate::personas::PersonaGroup],
) -> TargetResolution {
    let mut by_handle = BTreeMap::<String, Vec<&Conversation>>::new();
    let mut by_id = BTreeMap::<&str, Vec<&Conversation>>::new();
    for conversation in conversations {
        by_id
            .entry(&conversation.id)
            .or_default()
            .push(conversation);
        if !conversation.execution_profile.mention_handle.is_empty() {
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
    }
    let mut groups_by_handle = BTreeMap::<String, Vec<&crate::personas::PersonaGroup>>::new();
    for group in groups {
        if !group.mention_handle.is_empty() {
            groups_by_handle
                .entry(group.mention_handle.to_ascii_lowercase())
                .or_default()
                .push(group);
        }
    }
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    let mut ambiguous = Vec::new();
    let mut seen = BTreeSet::new();
    for handle in handles {
        let conversation_matches = by_handle.get(handle).map_or(&[][..], Vec::as_slice);
        let group_matches = groups_by_handle.get(handle).map_or(&[][..], Vec::as_slice);
        if conversation_matches.len() + group_matches.len() > 1 {
            ambiguous.push(handle.clone());
            continue;
        }
        if let Some(group) = group_matches.first() {
            let mut group_targets = Vec::new();
            let mut group_seen = BTreeSet::new();
            let mut valid = !group.persona_ids.is_empty();
            let mut identity_conflict = false;
            for id in &group.persona_ids {
                let matches = by_id.get(id.as_str()).map_or(&[][..], Vec::as_slice);
                if matches.len() > 1 {
                    identity_conflict = true;
                    continue;
                }
                match matches.first() {
                    Some(conversation)
                        if conversation.id != host_id
                            && conversation.kind == ConversationKind::PersonaTemplate
                            && group_seen.insert(conversation.id.clone()) =>
                    {
                        group_targets.push(ResolvedTarget {
                            kind: MentionTargetKind::Persona,
                            conversation: (**conversation).clone(),
                        });
                    }
                    _ => valid = false,
                }
            }
            // No partial group is admitted, including when its first member
            // was valid but a later member was missing, self, or ambiguous.
            if identity_conflict {
                ambiguous.push(handle.clone());
            } else if !valid {
                unresolved.push(handle.clone());
            } else {
                for target in group_targets {
                    if seen.insert(target.conversation.id.clone()) {
                        resolved.push(target);
                    }
                }
            }
        } else if let Some(conversation) = conversation_matches.first() {
            if by_id
                .get(conversation.id.as_str())
                .is_some_and(|matches| matches.len() != 1)
            {
                ambiguous.push(handle.clone());
            } else if conversation.id == host_id {
                unresolved.push(handle.clone());
            } else if seen.insert(conversation.id.clone()) {
                resolved.push(ResolvedTarget {
                    kind: if conversation.kind == ConversationKind::PersonaTemplate {
                        MentionTargetKind::Persona
                    } else {
                        MentionTargetKind::LiveChat
                    },
                    conversation: (**conversation).clone(),
                });
            }
            // A known target reached twice is a successful no-op. In
            // particular, @group @member must not turn @member into unknown.
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

#[cfg(test)]
mod target_resolution_tests {
    use super::*;

    fn conversation(id: &str, handle: &str, kind: ConversationKind) -> Conversation {
        Conversation {
            id: id.into(),
            title: format!("Title {id}"),
            created_at: "1".into(),
            updated_at: "1".into(),
            kind,
            execution_profile: ConversationExecutionProfile {
                mention_handle: handle.into(),
                ..ConversationExecutionProfile::default()
            },
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
        }
    }
    fn persona(id: &str) -> Conversation {
        conversation(id, id, ConversationKind::PersonaTemplate)
    }
    fn group(handle: &str, members: &[&str]) -> crate::personas::PersonaGroup {
        crate::personas::PersonaGroup {
            id: format!("group-{handle}"),
            name: handle.into(),
            mention_handle: handle.into(),
            persona_ids: members.iter().map(|id| (*id).into()).collect(),
            created_at: "1".into(),
            updated_at: "1".into(),
        }
    }
    fn resolve(
        handles: &[&str],
        host: &str,
        conversations: &[Conversation],
        groups: &[crate::personas::PersonaGroup],
    ) -> TargetResolution {
        // Exercise the existing production entry point, not just the new helper.
        resolve_targets_from_registry(
            &handles
                .iter()
                .map(|value| (*value).into())
                .collect::<Vec<_>>(),
            host,
            conversations,
            groups,
        )
    }
    fn ids(result: &TargetResolution) -> Vec<&str> {
        result
            .targets
            .iter()
            .map(|target| target.conversation.id.as_str())
            .collect()
    }
    fn no_error(result: &TargetResolution) {
        assert!(result.unresolved.is_empty(), "{:?}", result.unresolved);
        assert!(result.ambiguous.is_empty(), "{:?}", result.ambiguous);
    }

    #[test]
    fn group_then_direct_member_is_not_an_unresolved_mention() {
        let result = resolve(
            &["team", "a"],
            "host",
            &[persona("a"), persona("b")],
            &[group("team", &["a", "b"])],
        );
        no_error(&result);
        assert_eq!(ids(&result), ["a", "b"]);
    }
    #[test]
    fn direct_then_group_keeps_first_occurrence_order_without_double_inference() {
        let result = resolve(
            &["b", "team", "a"],
            "host",
            &[persona("a"), persona("b")],
            &[group("team", &["a", "b"])],
        );
        no_error(&result);
        assert_eq!(ids(&result), ["b", "a"]);
    }
    #[test]
    fn overlapping_groups_deduplicate_targets_but_keep_explicit_order() {
        let result = resolve(
            &["first", "second"],
            "host",
            &[persona("a"), persona("b"), persona("c")],
            &[group("first", &["b", "a"]), group("second", &["a", "c"])],
        );
        no_error(&result);
        assert_eq!(ids(&result), ["b", "a", "c"]);
    }
    #[test]
    fn a_group_cannot_bypass_the_direct_self_consult_restriction() {
        let result = resolve(
            &["team"],
            "a",
            &[persona("a"), persona("b")],
            &[group("team", &["b", "a"])],
        );
        assert!(result.targets.is_empty());
        assert_eq!(result.unresolved, ["team"]);
        let direct = resolve(&["a"], "a", &[persona("a")], &[]);
        assert!(direct.targets.is_empty());
        assert_eq!(direct.unresolved, ["a"]);
    }
    #[test]
    fn duplicate_occurrence_ids_with_distinct_handles_are_ambiguous() {
        let registry = [
            persona("a"),
            conversation("a", "alias", ConversationKind::PersonaTemplate),
        ];
        for handle in ["a", "alias", "team"] {
            let result = resolve(&[handle], "host", &registry, &[group("team", &["a"])]);
            assert!(result.targets.is_empty());
            assert_eq!(result.ambiguous, [handle]);
        }
    }
    #[test]
    fn conflicting_group_and_conversation_handles_do_not_choose_by_registry_order() {
        for registry in [
            vec![persona("a"), persona("team")],
            vec![persona("team"), persona("a")],
        ] {
            let result = resolve(&["team"], "host", &registry, &[group("team", &["a"])]);
            assert!(result.targets.is_empty());
            assert_eq!(result.ambiguous, ["team"]);
        }
    }
    #[test]
    fn invalid_group_membership_never_invokes_a_valid_subset() {
        let registry = [
            persona("a"),
            conversation("chat", "chat", ConversationKind::Chat),
        ];
        for members in [
            &[][..],
            &["a", "missing"][..],
            &["a", "a"][..],
            &["a", "chat"][..],
        ] {
            let result = resolve(&["team"], "host", &registry, &[group("team", members)]);
            assert!(result.targets.is_empty());
            assert_eq!(result.unresolved, ["team"]);
        }
    }
    #[test]
    fn unknown_handles_stay_explicit_and_never_fall_back_to_another_assistant() {
        let result = resolve(&["a", "unknown"], "host", &[persona("a")], &[]);
        assert_eq!(ids(&result), ["a"]);
        assert_eq!(result.unresolved, ["unknown"]);
        // The existing dispatch admission rejects any unresolved/ambiguous result.
    }
    #[test]
    fn repeated_direct_handles_are_one_target_not_a_second_call() {
        let result = resolve(&["a", "a"], "host", &[persona("a")], &[]);
        no_error(&result);
        assert_eq!(ids(&result), ["a"]);
    }
    #[test]
    fn duplicate_group_handles_are_not_silently_resolved() {
        let result = resolve(
            &["team"],
            "host",
            &[persona("a"), persona("b")],
            &[group("team", &["a"]), group("team", &["b"])],
        );
        assert!(result.targets.is_empty());
        assert_eq!(result.ambiguous, ["team"]);
    }
    #[test]
    fn registry_case_normalization_preserves_existing_parsed_handle_semantics() {
        let result = resolve(
            &["alice"],
            "host",
            &[conversation("a", "Alice", ConversationKind::Chat)],
            &[],
        );
        no_error(&result);
        assert_eq!(ids(&result), ["a"]);
        assert_eq!(result.targets[0].kind, MentionTargetKind::LiveChat);
    }
    #[test]
    fn resolution_does_not_mutate_profiles_histories_or_group_membership() {
        let conversations = vec![persona("a"), persona("b")];
        let groups = vec![group("team", &["a", "b"])];
        let before = (conversations.clone(), groups.clone());
        let result = resolve(&["team", "a"], "host", &conversations, &groups);
        no_error(&result);
        assert_eq!((conversations, groups), before);
    }
}
