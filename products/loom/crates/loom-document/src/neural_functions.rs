//! A small expression language over ordinary documents.
//!
//! References are values; only calls perform inference. Pipelines are syntax
//! sugar for calls, with the preceding value supplied as the first argument.
//! Resolution and execution belong to the caller, never to the parser.

use std::{collections::HashMap, ops::Range};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_NEURAL_COMMAND_BYTES: usize = 65_536;
pub const MAX_NEURAL_DOCUMENT_BYTES: usize = 1_048_576;
pub const MAX_NEURAL_NODES: usize = 256;
pub const MAX_NEURAL_DEPTH: usize = 16;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum NeuralCommand {
    Prompt(String),
    Expression(NeuralExpression),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NeuralExpression {
    Reference {
        name: String,
    },
    Literal {
        text: String,
    },
    Call {
        function: String,
        arguments: Vec<Self>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentReference {
    pub name: String,
    /// UTF-8 byte range, including the `@` and any quoting.
    pub range: Range<usize>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message} at byte {offset}")]
pub struct NeuralSyntaxError {
    pub offset: usize,
    pub message: &'static str,
}

/// Parse a terminal entry. Only a leading `=` opts into expression syntax.
/// Ordinary prompts retain all whitespace and line endings.
pub fn parse_neural_command(source: &str) -> Result<NeuralCommand, NeuralSyntaxError> {
    check_size(source, MAX_NEURAL_COMMAND_BYTES)?;
    let start = source.len() - source.trim_start().len();
    if source.as_bytes().get(start) != Some(&b'=') {
        return Ok(NeuralCommand::Prompt(source.to_owned()));
    }
    let mut parser = Parser {
        source,
        offset: start + 1,
        nodes: 0,
    };
    let expression = parser.expression(0)?;
    parser.whitespace();
    if parser.offset != source.len() {
        return Err(parser.error("expected the end of the expression"));
    }
    if expression_depth(&expression) > MAX_NEURAL_DEPTH {
        return Err(parser.error("expression nesting exceeds the limit"));
    }
    Ok(NeuralCommand::Expression(expression))
}

/// Find explicit mentions in Markdown prose, excluding escapes, email-like
/// words, fenced/indented code, and inline code spans. Repeated mentions retain
/// their source ranges; the resolver decides how to deduplicate identities.
pub fn document_references(source: &str) -> Result<Vec<DocumentReference>, NeuralSyntaxError> {
    check_size(source, MAX_NEURAL_DOCUMENT_BYTES)?;
    let mut references = Vec::new();
    let closing_ticks = last_tick_runs(source);
    let mut fence: Option<(u8, usize)> = None;
    let mut inline_ticks = 0;
    let mut line_start = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let marker = trimmed.as_bytes().first().copied();
        let run = marker.map_or(0, |byte| trimmed.bytes().take_while(|b| *b == byte).count());
        if let Some((byte, count)) = fence {
            if indent <= 3
                && marker == Some(byte)
                && run >= count
                && trimmed[run..].trim().is_empty()
            {
                fence = None;
            }
            line_start += line.len();
            continue;
        }
        if inline_ticks == 0
            && indent <= 3
            && matches!(marker, Some(b'`' | b'~'))
            && run >= 3
            && (marker != Some(b'`') || !trimmed[run..].contains('`'))
        {
            fence = marker.map(|byte| (byte, run));
            line_start += line.len();
            continue;
        }
        if inline_ticks == 0 && (indent >= 4 || line.starts_with('\t')) {
            line_start += line.len();
            continue;
        }
        let mut offset = 0;
        while offset < line.len() {
            let rest = &line[offset..];
            let character = rest.chars().next().expect("nonempty remainder");
            if character == '`' {
                let count = rest.bytes().take_while(|byte| *byte == b'`').count();
                if inline_ticks == count {
                    inline_ticks = 0;
                } else if inline_ticks == 0
                    && closing_ticks
                        .get(&count)
                        .is_some_and(|last| *last > line_start + offset)
                {
                    inline_ticks = count;
                }
                offset += count;
                continue;
            }
            if inline_ticks != 0 {
                offset += character.len_utf8();
                continue;
            }
            if character == '\\' {
                offset += character.len_utf8();
                if let Some(escaped) = line[offset..].chars().next() {
                    offset += escaped.len_utf8();
                }
                continue;
            }
            let previous = line[..offset].chars().next_back();
            if character != '@'
                || previous.is_some_and(|c| {
                    c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '@' | '-')
                })
            {
                offset += character.len_utf8();
                continue;
            }
            let start = line_start + offset;
            if let Some(reference) = reference_at(source, start) {
                if references.len() == MAX_NEURAL_NODES {
                    return Err(NeuralSyntaxError {
                        offset: start,
                        message: "too many document references",
                    });
                }
                offset = reference.range.end - line_start;
                references.push(reference);
            } else {
                offset += character.len_utf8();
            }
        }
        line_start += line.len();
    }
    Ok(references)
}

fn reference_at(source: &str, start: usize) -> Option<DocumentReference> {
    let mut parser = Parser {
        source,
        offset: start + 1,
        nodes: 0,
    };
    let mut name = parser.name().ok()?;
    let mut end = parser.offset;
    // A sentence's final full stop is prose punctuation. Quote a name ending
    // in a full stop to refer to it literally.
    if source.as_bytes().get(start + 1) != Some(&b'"') {
        let bare_len = name.trim_end_matches('.').len();
        end -= name.len() - bare_len;
        name.truncate(bare_len);
    }
    (!name.is_empty()).then_some(DocumentReference {
        name,
        range: start..end,
    })
}

/// Render a document function for a base model, without a chat template.
/// Every document and input byte is retained. The headings are a transparent
/// continuation scaffold, not an instruction-isolation or escaping protocol.
pub fn render_base_function_prompt(
    function: &str,
    inputs: &[&str],
) -> Result<String, NeuralSyntaxError> {
    let size = inputs
        .iter()
        .try_fold(function.len(), |size, input| size.checked_add(input.len()));
    if size.is_none_or(|size| size > MAX_NEURAL_DOCUMENT_BYTES) || inputs.len() > MAX_NEURAL_NODES {
        return Err(NeuralSyntaxError {
            offset: 0,
            message: "function inputs exceed the limit",
        });
    }
    let mut prompt = function.to_owned();
    if !inputs.is_empty() {
        prompt.push_str("\n\nInputs:\n");
        for (index, input) in inputs.iter().enumerate() {
            if inputs.len() > 1 {
                use std::fmt::Write;
                let _ = write!(prompt, "\nInput {}:\n", index + 1);
            }
            prompt.push_str(input);
            prompt.push('\n');
        }
    }
    prompt.push_str("\nOutput:\n");
    Ok(prompt)
}

fn check_size(source: &str, limit: usize) -> Result<(), NeuralSyntaxError> {
    if source.len() > limit {
        Err(NeuralSyntaxError {
            offset: limit,
            message: "input exceeds the byte limit",
        })
    } else {
        Ok(())
    }
}

// A suffix lookup prevents quadratic rescanning on unmatched backticks.
fn last_tick_runs(source: &str) -> HashMap<usize, usize> {
    let mut runs = HashMap::new();
    let bytes = source.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'`' {
            offset += 1;
            continue;
        }
        let start = offset;
        while bytes.get(offset) == Some(&b'`') {
            offset += 1;
        }
        runs.insert(offset - start, start);
    }
    runs
}

fn expression_depth(expression: &NeuralExpression) -> usize {
    match expression {
        NeuralExpression::Call { arguments, .. } => {
            1 + arguments.iter().map(expression_depth).max().unwrap_or(0)
        }
        _ => 1,
    }
}

struct Parser<'a> {
    source: &'a str,
    offset: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn error(&self, message: &'static str) -> NeuralSyntaxError {
        NeuralSyntaxError {
            offset: self.offset,
            message,
        }
    }

    fn whitespace(&mut self) {
        self.offset +=
            self.source[self.offset..].len() - self.source[self.offset..].trim_start().len();
    }

    fn take(&mut self, token: &str) -> bool {
        if self.source[self.offset..].starts_with(token) {
            self.offset += token.len();
            true
        } else {
            false
        }
    }

    fn node(&mut self) -> Result<(), NeuralSyntaxError> {
        self.nodes += 1;
        if self.nodes > MAX_NEURAL_NODES {
            return Err(self.error("too many expression nodes"));
        }
        Ok(())
    }

    fn expression(&mut self, depth: usize) -> Result<NeuralExpression, NeuralSyntaxError> {
        if depth >= MAX_NEURAL_DEPTH {
            return Err(self.error("expression nesting exceeds the limit"));
        }
        let mut expression = self.atom(depth)?;
        self.whitespace();
        while self.take("|>") {
            self.whitespace();
            if !self.take("@") {
                return Err(self.error("expected a document function after |>"));
            }
            let function = self.name()?;
            self.node()?;
            let mut arguments = vec![expression];
            self.whitespace();
            if self.take("(") {
                arguments.extend(self.arguments(depth)?);
            }
            expression = NeuralExpression::Call {
                function,
                arguments,
            };
            if expression_depth(&expression) > MAX_NEURAL_DEPTH {
                return Err(self.error("expression nesting exceeds the limit"));
            }
            self.whitespace();
        }
        Ok(expression)
    }

    fn atom(&mut self, depth: usize) -> Result<NeuralExpression, NeuralSyntaxError> {
        self.whitespace();
        self.node()?;
        if self.take("@") {
            let name = self.name()?;
            self.whitespace();
            if self.take("(") {
                Ok(NeuralExpression::Call {
                    function: name,
                    arguments: self.arguments(depth)?,
                })
            } else {
                Ok(NeuralExpression::Reference { name })
            }
        } else if self.source[self.offset..].starts_with('"') {
            Ok(NeuralExpression::Literal {
                text: self.quoted()?,
            })
        } else {
            Err(self.error("expected @document, @function(...), or quoted text"))
        }
    }

    fn arguments(&mut self, depth: usize) -> Result<Vec<NeuralExpression>, NeuralSyntaxError> {
        let mut arguments = Vec::new();
        self.whitespace();
        if self.take(")") {
            return Ok(arguments);
        }
        loop {
            arguments.push(self.expression(depth + 1)?);
            self.whitespace();
            if self.take(")") {
                return Ok(arguments);
            }
            if !self.take(",") {
                return Err(self.error("expected , or )"));
            }
        }
    }

    fn name(&mut self) -> Result<String, NeuralSyntaxError> {
        if self.source[self.offset..].starts_with('"') {
            let name = self.quoted()?;
            if name.trim().is_empty() {
                return Err(self.error("document name is empty"));
            }
            return Ok(name);
        }
        let start = self.offset;
        for character in self.source[self.offset..].chars() {
            if character.is_alphanumeric()
                || matches!(character, '_' | '-' | '/' | '.' | '#')
                || matches!(character, '\u{0300}'..='\u{036f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}' | '\u{fe20}'..='\u{fe2f}')
            {
                self.offset += character.len_utf8();
            } else {
                break;
            }
        }
        if start == self.offset {
            return Err(self.error("expected a document name"));
        }
        Ok(self.source[start..self.offset].to_owned())
    }

    fn quoted(&mut self) -> Result<String, NeuralSyntaxError> {
        self.take("\"");
        let mut text = String::new();
        while let Some(character) = self.source[self.offset..].chars().next() {
            self.offset += character.len_utf8();
            match character {
                '"' => return Ok(text),
                '\\' => {
                    let escaped = self.source[self.offset..]
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("unfinished escape"))?;
                    self.offset += escaped.len_utf8();
                    text.push(match escaped {
                        '"' | '\\' => escaped,
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        _ => {
                            return Err(
                                self.error("unknown escape; use quote, backslash, n, r, or t")
                            );
                        }
                    });
                }
                '\n' | '\r' => return Err(self.error("use an escaped newline inside quoted text")),
                _ => text.push(character),
            }
        }
        Err(self.error("unclosed quote"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(name: &str) -> NeuralExpression {
        NeuralExpression::Reference {
            name: name.to_owned(),
        }
    }
    fn call(function: &str, arguments: Vec<NeuralExpression>) -> NeuralExpression {
        NeuralExpression::Call {
            function: function.to_owned(),
            arguments,
        }
    }

    #[test]
    fn plain_prompts_and_reference_values_never_become_calls() {
        let text = "  Continue @Draft\r\n";
        assert_eq!(
            parse_neural_command(text).unwrap(),
            NeuralCommand::Prompt(text.into())
        );
        assert_eq!(
            parse_neural_command(" =@Draft ").unwrap(),
            NeuralCommand::Expression(reference("Draft"))
        );
        assert_eq!(
            parse_neural_command("=@Draft()").unwrap(),
            NeuralCommand::Expression(call("Draft", vec![]))
        );
    }

    #[test]
    fn pipelines_elaborate_left_to_right_and_prepend_the_value() {
        let source = "=@Draft |> @Shorten |> @Polish(@Voice)";
        let expected = call(
            "Polish",
            vec![
                call("Shorten", vec![reference("Draft")]),
                reference("Voice"),
            ],
        );
        assert_eq!(
            parse_neural_command(source).unwrap(),
            NeuralCommand::Expression(expected)
        );
        assert_eq!(
            parse_neural_command(source),
            parse_neural_command("=@Polish(@Shorten(@Draft), @Voice)")
        );
    }

    #[test]
    fn quoted_names_paths_unicode_and_literal_arguments_round_trip() {
        let parsed =
            parse_neural_command("=@\"Précis and polish\"(@研究/étude#résumé, \"one\\ntwo\\\"\")")
                .unwrap();
        assert_eq!(
            parsed,
            NeuralCommand::Expression(call(
                "Précis and polish",
                vec![
                    reference("研究/étude#résumé"),
                    NeuralExpression::Literal {
                        text: "one\ntwo\"".into()
                    }
                ]
            ))
        );
        for malformed in [
            "=@",
            "=@F(@A,)",
            "=@F(@A",
            "=@A trailing",
            "=@A |> text",
            "=@\"\"",
            "=@F(\"\\q\")",
        ] {
            assert!(
                parse_neural_command(malformed).is_err(),
                "accepted {malformed}"
            );
        }
    }

    #[test]
    fn markdown_references_skip_code_escapes_and_email_preserving_ranges() {
        let text = "See @Draft, @\"Voice notes\" and @研究/.\n\\@escaped person@example.com `@inline`\n```md\n@fenced\n```\n~~~\n@tilde\n~~~\n    @indented\n\t@tabbed\n(@Again).";
        let references = document_references(text).unwrap();
        assert_eq!(
            references
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Draft", "Voice notes", "研究/", "Again"]
        );
        assert_eq!(
            references
                .iter()
                .map(|r| &text[r.range.clone()])
                .collect::<Vec<_>>(),
            vec!["@Draft", "@\"Voice notes\"", "@研究/", "@Again"]
        );
    }

    #[test]
    fn prose_punctuation_stays_outside_unicode_names() {
        let text = "“@Voice” — @研究。 @cafe\u{301}, @\"🌿 notes\"";
        let names = document_references(text)
            .unwrap()
            .into_iter()
            .map(|reference| reference.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["Voice", "研究", "cafe\u{301}", "🌿 notes"]);
    }

    #[test]
    fn code_delimiters_obey_run_lengths_and_unclosed_ticks_are_literal() {
        let text = "`` @hidden ` still hidden `` @visible\n` unmatched @also-visible\n";
        let references = document_references(text).unwrap();
        assert_eq!(
            references
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["visible", "also-visible"]
        );
        assert!(
            document_references("```\n@hidden\n``\n@still-hidden")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn limits_reject_deep_wide_and_oversized_input() {
        let nested = format!(
            "={}@A{}",
            "@F(".repeat(MAX_NEURAL_DEPTH),
            ")".repeat(MAX_NEURAL_DEPTH)
        );
        assert!(parse_neural_command(&nested).is_err());
        let pipeline = format!("=@A{}", " |> @F".repeat(MAX_NEURAL_DEPTH));
        assert!(parse_neural_command(&pipeline).is_err());
        let wide = format!("=@F({})", vec!["@A"; MAX_NEURAL_NODES].join(","));
        assert!(parse_neural_command(&wide).is_err());
        assert!(parse_neural_command(&"é".repeat(MAX_NEURAL_COMMAND_BYTES)).is_err());
        assert!(document_references(&"@a ".repeat(MAX_NEURAL_NODES + 1)).is_err());
    }

    #[test]
    fn base_prompt_retains_exact_function_and_input_bytes() {
        let function = "  Make it brief.\r\nExample:\n  before -> after  ";
        let input = "\n  café\r\n\t";
        let prompt = render_base_function_prompt(function, &[input]).unwrap();
        assert_eq!(
            prompt,
            format!("{function}\n\nInputs:\n{input}\n\nOutput:\n")
        );
        assert_eq!(
            render_base_function_prompt(function, &[]).unwrap(),
            format!("{function}\nOutput:\n")
        );
        assert!(
            render_base_function_prompt(&"x".repeat(MAX_NEURAL_DOCUMENT_BYTES), &["y"]).is_err()
        );
    }
}
