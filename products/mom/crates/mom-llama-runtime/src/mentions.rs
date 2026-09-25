pub fn chat_dispatch_stream_in_scope<F>(
    scope: &OperationScope,
    mut input: MentionDispatchInput,
    options: ChatSendOptions,
    mut on_event: Option<F>,
) -> Result<CommandResult<ChatDispatchOutput>>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    // Parse before creating/instantiating any conversation or admitting work.
    let handles = parse_handles(&input.message)?;
    let (_, selected) =
        crate::conversation_store::get_or_create_conversation(&input.conversation_id)?;
    if selected.kind == ConversationKind::PersonaTemplate {
        let instantiated = persona_instantiate(&selected.id, None)?;
        let Some(conversation) = instantiated.result else {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_dispatch",
                &instantiated.readiness,
                instantiated.blocker.unwrap_or_else(|| {
                    Blocker::new(
                        "persona_instantiate_failed",
                        "The persona could not be opened as a chat.",
                        vec!["Try again from Personas in Settings.".to_string()],
                    )
                }),
            ));
        };
        input.conversation_id = conversation.id;
    }
    let resolution = resolve_targets(&handles, &input.conversation_id)?;
    if let Some(blocker) = ambiguous_resolution_blocker(&resolution) {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            blocker,
        ));
    }
    if !resolution.unresolved.is_empty() {
// SOURCE WINDOW GAP 1
    let addressed = append_attachment_context(
        &strip_handles(&input.message, &targets)?,
        &attachment_context.current_text,
    );
    let participant_names = targets
        .iter()
        .map(|target| format!("@{}", target.conversation.execution_profile.mention_handle))
        .collect::<Vec<_>>()
        .join(", ");
    let mut planned = Vec::new();
    for (order, target) in targets.iter().enumerate() {
        let snapshot = &snapshots[order];
// SOURCE WINDOW GAP 2
    let final_message = ChatMessage {
        role: ChatRole::User,
        content: addressed.to_string(),
    };
    let mut messages = Vec::new();
    if let Some(system) = system {
        messages.push(system);
    }
    messages.extend(source.clone());
    messages.push(boundary.clone());
    messages.append(&mut host);
    messages.push(final_message.clone());
// SOURCE WINDOW GAP 3
fn parse_handles(message: &str) -> Result<Vec<String>> {
    Ok(workspace_document::references::participant_handles(message)?)
}

fn strip_handles(message: &str, _targets: &[ResolvedTarget]) -> Result<String> {
    // Resolution already admitted every explicit address, including group
    // aliases. Expanded member handles are not the address spans in the text.
    Ok(workspace_document::references::remove_participant_addresses(message)?)
}

fn append_attachment_context(message: &str, attachment_text: &str) -> String {
    if attachment_text.is_empty() {
        message.to_string()
    } else if message.is_empty() {
        attachment_text.to_string()
    } else {
        format!("{message}\n\n{attachment_text}")
    }
}
// SOURCE WINDOW GAP 4
    #[test]
    fn mention_parser_is_stable_and_deduplicates_handles() {
        assert_eq!(
            parse_handles("Ask @evidence-lens and @whole-person, then @evidence-lens.")
                .expect("bounded addresses"),
            vec!["evidence-lens", "whole-person"]
        );
    }

    #[test]
    fn mention_parser_requires_the_explicit_composer_boundary() {
        assert_eq!(
            parse_handles("@leading then\t@after-tab and\n@after-newline")
                .expect("bounded addresses"),
            vec!["leading", "after-tab", "after-newline"]
        );
        assert!(parse_handles("mail@example.com prefix@embedded (@parenthesized)")
            .expect("inert text").is_empty());
    }

    #[test]
    fn mention_parser_ignores_markdown_code() {
        assert!(
            parse_handles(
                "`@inline` and ``@wide-inline``\n```text\n@fenced\n```\n~~~\n@tilde-fenced\n~~~\n    @indented"
            )
            .expect("inert code")
            .is_empty()
        );
    }

    #[test]
    fn duplicate_case_insensitive_registry_handles_are_ambiguous_not_last_wins() {
        let conversations = vec![
            mention_conversation("first", "same-lens"),
            mention_conversation("second", "SAME-LENS"),
        ];
