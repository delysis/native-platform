pub fn chat_dispatch_stream_in_scope<F>(
    scope: &OperationScope,
    mut input: MentionDispatchInput,
    options: ChatSendOptions,
    mut on_event: Option<F>,
) -> Result<CommandResult<ChatDispatchOutput>>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
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
    let handles = parse_handles(&input.message);
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
        &strip_handles(&input.message, &targets),
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
        content: addressed.trim().to_string(),
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
fn parse_handles(message: &str) -> Vec<String> {
    let mut handles = Vec::new();
    for token in mention_tokens(message) {
        let handle = token.handle.to_ascii_lowercase();
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    handles
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MentionToken<'a> {
    start: usize,
    end: usize,
    handle: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeDelimiter {
    marker: char,
    width: usize,
}

fn mention_tokens(message: &str) -> Vec<MentionToken<'_>> {
    let chars = message.char_indices().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut code_delimiter: Option<CodeDelimiter> = None;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index].1;
        if character == '`' || character == '~' {
            let marker = character;
            let mut width = 1;
            while index + width < chars.len() && chars[index + width].1 == marker {
                width += 1;
            }
            match code_delimiter {
                Some(open) if open.marker == marker && open.width == width => {
                    code_delimiter = None;
                }
                None if marker == '`' || width >= 3 => {
                    code_delimiter = Some(CodeDelimiter { marker, width });
                }
                _ => {}
            }
            index += width;
            continue;
        }
        let explicit_boundary = index == 0 || chars[index - 1].1.is_whitespace();
        if code_delimiter.is_none()
            && character == '@'
            && explicit_boundary
            && !is_indented_code_position(message, chars[index].0)
        {
            let start = chars[index].0 + 1;
            let mut end = start;
            index += 1;
            while index < chars.len()
                && (chars[index].1.is_ascii_alphanumeric() || chars[index].1 == '-')
            {
                end = chars[index].0 + chars[index].1.len_utf8();
                index += 1;
            }
            if end > start {
                tokens.push(MentionToken {
                    start: start - 1,
                    end,
                    handle: &message[start..end],
                });
            }
        } else {
            index += 1;
        }
    }
    tokens
}

fn is_indented_code_position(message: &str, byte_index: usize) -> bool {
    let line_start = message[..byte_index]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let prefix = &message[line_start..byte_index];
    prefix.starts_with('\t') || (prefix.len() >= 4 && prefix.bytes().all(|byte| byte == b' '))
}

fn strip_handles(message: &str, targets: &[ResolvedTarget]) -> String {
    let target_handles = targets
        .iter()
        .map(|target| {
            target
                .conversation
                .execution_profile
                .mention_handle
                .to_ascii_lowercase()
        })
        .collect::<BTreeSet<_>>();
    let removals = mention_tokens(message)
        .into_iter()
        .filter(|token| target_handles.contains(&token.handle.to_ascii_lowercase()))
        .map(|token| {
            let mut end = token.end;
            for (offset, character) in message[token.end..].char_indices() {
                if character.is_whitespace() || character.is_ascii_alphanumeric() {
                    break;
                }
                end = token.end + offset + character.len_utf8();
            }
            token.start..end
        })
        .collect::<Vec<_>>();
    let mut stripped = String::with_capacity(message.len());
    let mut cursor = 0;
    for removal in removals {
        stripped.push_str(&message[cursor..removal.start]);
        cursor = removal.end;
    }
    stripped.push_str(&message[cursor..]);
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
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
            parse_handles("Ask @evidence-lens and @whole-person, then @evidence-lens."),
            vec!["evidence-lens", "whole-person"]
        );
    }

    #[test]
    fn mention_parser_requires_the_explicit_composer_boundary() {
        assert_eq!(
            parse_handles("@leading then\t@after-tab and\n@after-newline"),
            vec!["leading", "after-tab", "after-newline"]
        );
        assert!(parse_handles("mail@example.com prefix@embedded (@parenthesized)").is_empty());
    }

    #[test]
    fn mention_parser_ignores_markdown_code() {
        assert!(
            parse_handles(
                "`@inline` and ``@wide-inline``\n```text\n@fenced\n```\n~~~\n@tilde-fenced\n~~~\n    @indented"
            )
            .is_empty()
        );
    }

    #[test]
    fn duplicate_case_insensitive_registry_handles_are_ambiguous_not_last_wins() {
        let conversations = vec![
            mention_conversation("first", "same-lens"),
            mention_conversation("second", "SAME-LENS"),
        ];
