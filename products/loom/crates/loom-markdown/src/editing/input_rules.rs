//! Native equivalents of Loom's text-input rules. Paste remains literal.
use super::{
    Fragment, FragmentKind, checked_range, fragments, selection_style, serialize_leaf, set_format,
    slice_fragments,
};
use crate::{BlockStyle, Dialect, EditorDocument, Error, InlineFormat, Selection};
use std::ops::Range;

const MAX_CONTEXT: usize = 8192;

#[derive(Clone, Copy, Debug)]
pub(super) enum BlockRule {
    Heading(u8),
    Quote,
    List(Option<u64>),
}

#[derive(Debug)]
enum RuleKind {
    Block(BlockRule),
    Inline {
        leading: String,
        content: String,
        format: InlineFormat,
    },
}

#[derive(Debug)]
struct Rule {
    /// Existing display bytes replaced by the transformation; newly typed bytes
    /// are part of the recognized pattern, never an intermediate live edit.
    range: Range<usize>,
    kind: RuleKind,
}

impl EditorDocument {
    /// Interpret completed Markdown patterns from typing or a committed IME edit.
    /// Use `replace_visual` for paste, accessibility replacement and literal input.
    pub fn type_visual(&mut self, selection: Selection, text: &str) -> Result<bool, Error> {
        checked_range(self.projection().text(), selection)?;
        let Some(rule) = detect(self, selection, text) else {
            return self.replace_visual(selection, text);
        };
        let inherited = self
            .typing_style
            .clone()
            .unwrap_or_else(|| selection_style(self.projection(), selection));
        let result = match rule.kind {
            RuleKind::Block(format) => self.apply_block_input_rule(selection, rule.range, format),
            RuleKind::Inline {
                leading,
                content,
                format,
            } => self.apply_inline_input_rule(selection, rule.range, &leading, &content, &format),
        };
        match result {
            Ok(changed) => {
                // Subsequent typing inherits the old marks, not the new mark.
                self.typing_style = Some(inherited);
                Ok(changed)
            }
            // Automatic formatting is optional. A refused transformation must
            // still admit ordinary typing, through its own atomic validation.
            Err(
                Error::UnsupportedEdit | Error::SerializationMismatch | Error::AmbiguousBoundary,
            ) => self.replace_visual(selection, text),
            Err(error) => Err(error),
        }
    }

    fn apply_inline_input_rule(
        &mut self,
        before: Selection,
        range: Range<usize>,
        leading: &str,
        content: &str,
        format: &InlineFormat,
    ) -> Result<bool, Error> {
        let (index, block) = self
            .projection()
            .blocks()
            .iter()
            .enumerate()
            .find(|(_, b)| b.display.start <= range.start && range.end <= b.display.end)
            .ok_or(Error::UnsupportedEdit)?;
        let original = fragments(self.markdown(), self.projection(), block)?;
        let mut style = self
            .typing_style
            .clone()
            .unwrap_or_else(|| selection_style(self.projection(), before));
        let mut changed = slice_fragments(&original, 0..range.start - block.display.start);
        if !leading.is_empty() {
            changed.push(Fragment {
                text: leading.into(),
                style: style.clone(),
                kind: FragmentKind::Text,
            });
        }
        set_format(&mut style, format, true);
        changed.push(Fragment {
            text: content.into(),
            style,
            kind: FragmentKind::Text,
        });
        changed.extend(slice_fragments(
            &original,
            range.end - block.display.start..block.display.len(),
        ));
        let mut transaction = self.transaction();
        transaction.replace(
            block.source.clone(),
            serialize_leaf(self.markdown(), block, &changed, &block.style)?,
        )?;
        let mut expected = self.projection().text().to_owned();
        expected.replace_range(range.clone(), &format!("{leading}{content}"));
        self.commit_visual(
            transaction,
            before,
            Selection::caret(range.start + leading.len() + content.len()),
            &[(index, changed)],
            &expected,
        )
    }
}

fn detect(doc: &EditorDocument, selection: Selection, text: &str) -> Option<Rule> {
    if doc.markdown().dialect() == Dialect::PlainText
        || text.is_empty()
        || text.len() > MAX_CONTEXT
        || text.contains(['\r', '\n'])
    {
        return None;
    }
    let last = text.chars().next_back()?;
    if !matches!(last, '*' | '_' | ')') && !last.is_whitespace() {
        return None;
    }
    let range = selection.range();
    let block = doc.projection().blocks().iter().find(|b| {
        b.display.start <= range.start
            && range.end <= b.display.end
            && matches!(b.style, BlockStyle::Body | BlockStyle::Heading(_))
    })?;
    if doc
        .typing_style
        .as_ref()
        .unwrap_or(&selection_style(doc.projection(), selection))
        .code
    {
        return None;
    }
    let display = doc.projection().text();
    let mut start = block
        .display
        .start
        .max(range.start.saturating_sub(MAX_CONTEXT - text.len()));
    while !display.is_char_boundary(start) {
        start += 1;
    }
    let previous = display[block.display.start..start].chars().next_back();
    let mut prefix = display[start..range.start].to_owned();
    let old_len = prefix.len();
    prefix.push_str(text);
    let inline = |open: usize, content: &str, format| Rule {
        range: start + open.min(old_len)..range.end,
        kind: RuleKind::Inline {
            leading: if open > old_len {
                prefix[old_len..open].into()
            } else {
                String::new()
            },
            content: content.into(),
            format,
        },
    };
    if start == block.display.start
        && let Some(format) = block_rule(&prefix)
    {
        return Some(Rule {
            range: start..range.end,
            kind: RuleKind::Block(format),
        });
    }
    for (delimiter, format) in [
        ("**", InlineFormat::Bold),
        ("__", InlineFormat::Bold),
        ("*", InlineFormat::Italic),
        ("_", InlineFormat::Italic),
    ] {
        let Some(body) = prefix.strip_suffix(delimiter) else {
            continue;
        };
        let Some(open) = body.rfind(delimiter) else {
            continue;
        };
        let marker = delimiter.chars().next()?;
        let content = &body[open + delimiter.len()..];
        let preceding = body[..open].chars().next_back().or(previous);
        if !content.is_empty()
            && !content.contains([marker, '\r', '\n'])
            && !preceding.is_some_and(|c| c == marker || c == '\\')
        {
            return Some(inline(open, content, format));
        }
    }
    let (open, content, url) = link_rule(&prefix, previous)?;
    Some(inline(open, content, InlineFormat::Link(url.into())))
}

fn link_rule(prefix: &str, previous: Option<char>) -> Option<(usize, &str, &str)> {
    let body = prefix.strip_suffix(')')?;
    let (label, url) = body.rsplit_once("](")?;
    if url.is_empty()
        || url
            .chars()
            .any(|c| c == ')' || c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let label_start = label.rfind([']', '\r', '\n']).map_or(0, |i| i + 1);
    let open = label[label_start..]
        .char_indices()
        .filter(|(_, c)| *c == '[')
        .map(|(i, _)| label_start + i)
        .find(|&i| {
            !label[..i]
                .chars()
                .next_back()
                .or(previous)
                .is_some_and(|c| c == '!' || c == '\\')
        })?;
    let content = &label[open + 1..];
    if content.is_empty() {
        return None;
    }
    Some((open, content, url))
}

fn block_rule(prefix: &str) -> Option<BlockRule> {
    let last = prefix.chars().next_back()?;
    if !last.is_whitespace() {
        return None;
    }
    let marker = &prefix[..prefix.len() - last.len_utf8()];
    if (1..=3).contains(&marker.len()) && marker.bytes().all(|b| b == b'#') {
        return Some(BlockRule::Heading(u8::try_from(marker.len()).ok()?));
    }
    match marker.trim_start_matches(char::is_whitespace) {
        ">" => return Some(BlockRule::Quote),
        "-" | "+" | "*" => return Some(BlockRule::List(None)),
        _ => {}
    }
    let number = marker.strip_suffix('.')?;
    if !(1..=9).contains(&number.len()) || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(BlockRule::List(Some(number.parse().ok()?)))
}
