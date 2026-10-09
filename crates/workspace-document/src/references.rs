//! One lexical grammar for context references and explicitly addressed consults.
//!
//! A context reference is a value, not a call. The consult composer retains its
//! stronger start/whitespace boundary for invoking participants: `(@expert)` is
//! readable document context but never an implicit invitation. Retained links
//! carry context identity and cannot invoke the participant named by a label.

// The imported grammar's existing fixture tests use unwrap; production parsing
// retains the workspace lint. Moving tests must not remove their assertions.
#[cfg_attr(test, allow(clippy::unwrap_used))]
#[path = "reference_grammar.rs"]
mod grammar;

pub use grammar::{
    DocumentReference, MAX_NEURAL_COMMAND_BYTES, MAX_NEURAL_DEPTH, MAX_NEURAL_DOCUMENT_BYTES,
    MAX_NEURAL_NODES, NeuralCommand, NeuralExpression, NeuralSyntaxError, document_references,
    parse_neural_command,
};

/// Bounded, ordered, case-insensitively deduplicated participant addresses.
/// Return an error on oversized or overfull input; never turn a failed parse into
/// an empty address set that could accidentally select the default assistant.
pub fn participant_handles(source: &str) -> Result<Vec<String>, NeuralSyntaxError> {
    let mut handles = Vec::new();
    for reference in participant_references(source)? {
        let handle = reference.name.to_ascii_lowercase();
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    Ok(handles)
}

/// Remove only the exact address spans from an already admitted consult.
/// Do not trim, collapse whitespace, remove punctuation, or search/replace a
/// handle spelling globally. Code, quotes, emails and larger names retain bytes.
/// Group addresses are removed as addresses, not as their expanded member names.
pub fn remove_participant_addresses(source: &str) -> Result<String, NeuralSyntaxError> {
    let references = participant_references(source)?;
    let mut text = String::with_capacity(source.len());
    let mut cursor = 0;
    for reference in references {
        text.push_str(&source[cursor..reference.range.start]);
        cursor = reference.range.end;
    }
    text.push_str(&source[cursor..]);
    Ok(text)
}

fn participant_references(source: &str) -> Result<Vec<DocumentReference>, NeuralSyntaxError> {
    if source.len() > MAX_NEURAL_COMMAND_BYTES {
        return Err(NeuralSyntaxError {
            offset: MAX_NEURAL_COMMAND_BYTES,
            message: "consult input exceeds the byte limit",
        });
    }
    let guarded = invocation_code_ranges(source);
    Ok(document_references(source)?
        .into_iter()
        .filter(|reference| {
            // A retained identity link starts with `[`, not `@`. The label is
            // presentation and cannot be rebound to an executable participant.
            !guarded
                .iter()
                .any(|range| range.contains(&reference.range.start))
                && source.as_bytes().get(reference.range.start) == Some(&b'@')
                && (reference.range.start == 0
                    || source[..reference.range.start]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
        })
        .collect())
}

// Invocation is stronger authority than a passive reference. Retain Mom's
// conservative code-delimiter guard, including unmatched ticks and mid-line
// tilde fences, on top of the shared Markdown grammar. This guard only removes
// invocation eligibility; it neither resolves names nor edits source bytes.
fn invocation_code_ranges(source: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut open: Option<(u8, usize, usize)> = None;
    let mut offset = 0;
    while offset < bytes.len() {
        let marker = bytes[offset];
        if !matches!(marker, b'`' | b'~') {
            offset += 1;
            continue;
        }
        let start = offset;
        while bytes.get(offset) == Some(&marker) {
            offset += 1;
        }
        let width = offset - start;
        match open {
            Some((opening, count, begin)) if marker == opening && width == count => {
                ranges.push(begin..offset);
                open = None;
            }
            None if marker == b'`' || width >= 3 => open = Some((marker, width, start)),
            _ => {}
        }
    }
    if let Some((_, _, begin)) = open {
        ranges.push(begin..source.len());
    }
    ranges
}

/// Keep the existing exact prompt framing, counting the framing itself in the
/// output budget. The original renderer bounded only the unframed inputs.
pub fn render_base_function_prompt(
    function: &str,
    inputs: &[&str],
) -> Result<String, NeuralSyntaxError> {
    let budget_error = || NeuralSyntaxError {
        offset: 0,
        message: "function inputs and framing exceed the limit",
    };
    if inputs.len() > MAX_NEURAL_NODES {
        return Err(budget_error());
    }
    let mut bytes = function
        .len()
        .checked_add("\nOutput:\n".len())
        .ok_or_else(budget_error)?;
    if !inputs.is_empty() {
        bytes = bytes
            .checked_add("\n\nInputs:\n".len())
            .ok_or_else(budget_error)?;
    }
    for (index, input) in inputs.iter().enumerate() {
        let heading = if inputs.len() > 1 {
            format!("\nInput {}:\n", index + 1).len()
        } else {
            0
        };
        bytes = bytes
            .checked_add(heading)
            .and_then(|size| size.checked_add(input.len()))
            .and_then(|size| size.checked_add(1))
            .ok_or_else(budget_error)?;
    }
    if bytes > MAX_NEURAL_DOCUMENT_BYTES {
        return Err(budget_error());
    }
    grammar::render_base_function_prompt(function, inputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consult_boundaries_remain_stricter_than_passive_context() {
        let input = "@leading then\t@after-tab\n@after-newline (@passive) mail@example.com";
        assert_eq!(
            participant_handles(input).expect("bounded input"),
            ["leading", "after-tab", "after-newline"]
        );
        assert!(
            document_references(input)
                .expect("context")
                .iter()
                .any(|reference| reference.name == "passive")
        );
    }

    #[test]
    fn consult_removes_addresses_not_authored_bytes_or_expanded_group_members() {
        let input = "@consult-group  café\r\n  question, @expert!\n\t`@expert`\n> @expert\n";
        assert_eq!(
            remove_participant_addresses(input).expect("exact removal"),
            "  café\r\n  question, !\n\t`@expert`\n> @expert\n"
        );
        assert_eq!(
            participant_handles(input).expect("addresses"),
            ["consult-group", "expert"]
        );
    }

    #[test]
    fn quoted_names_and_complete_paths_do_not_invoke_a_shorter_prefix() {
        assert_eq!(
            participant_handles("@\"whole-person\" @Expert/notes.md @Expert @expert.")
                .expect("complete names"),
            ["whole-person", "expert/notes.md", "expert"]
        );
        assert_eq!(
            remove_participant_addresses("@\"whole-person\"\r\n  q").expect("quoted address"),
            "\r\n  q"
        );
    }

    #[test]
    fn retained_context_identity_never_invokes_its_label() {
        let link = format!("[@expert](loom-material:material-{})", "a".repeat(64));
        assert!(participant_handles(&link).expect("context link").is_empty());
        assert_eq!(
            remove_participant_addresses(&link).expect("no invitation"),
            link
        );
        assert_eq!(
            document_references(&link).expect("retained identity")[0].name,
            format!("material-{}", "a".repeat(64))
        );
    }

    #[test]
    fn oversized_and_overfull_consults_fail_instead_of_selecting_default_chat() {
        assert!(participant_handles(&"x".repeat(MAX_NEURAL_COMMAND_BYTES + 1)).is_err());
        assert!(participant_handles(&"@expert ".repeat(MAX_NEURAL_NODES + 1)).is_err());
    }

    #[test]
    fn invocation_never_expands_through_an_unclosed_or_inline_code_region() {
        for text in [
            "` unmatched @expert",
            "prefix ~~~ @expert ~~~",
            "`` @expert `",
        ] {
            assert!(participant_handles(text).expect("inert code").is_empty());
            assert_eq!(
                remove_participant_addresses(text).expect("inert bytes"),
                text
            );
        }
        assert_eq!(
            participant_handles("prefix ~~~ @hidden ~~~ @visible").expect("closed region"),
            ["visible"]
        );
    }

    #[test]
    fn framed_prompt_budget_counts_every_separator_and_decimal_heading() {
        let overhead = "\nOutput:\n".len();
        let exact = "x".repeat(MAX_NEURAL_DOCUMENT_BYTES - overhead);
        assert_eq!(
            render_base_function_prompt(&exact, &[])
                .expect("exact ceiling")
                .len(),
            MAX_NEURAL_DOCUMENT_BYTES
        );
        assert!(render_base_function_prompt(&(exact + "x"), &[]).is_err());
        let inputs = vec!["café\r\n"; 12];
        let small = render_base_function_prompt("f", &inputs).expect("small framing");
        let function = "f".repeat(MAX_NEURAL_DOCUMENT_BYTES - small.len() + 1);
        assert_eq!(
            render_base_function_prompt(&function, &inputs)
                .expect("numbered framing")
                .len(),
            MAX_NEURAL_DOCUMENT_BYTES
        );
        assert!(render_base_function_prompt(&(function + "x"), &inputs).is_err());
    }
}
