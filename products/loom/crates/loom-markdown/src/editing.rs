//! Semantic inline edits rewrite only affected text blocks. The candidate is
//! parsed again and its visible text/marks checked before the transaction commits.
use crate::transaction::EditorSelection;
use crate::{
    Bias, BlockStyle, EditorDocument, Error, InlineStyle, Markdown, NodeKind, Projection,
    Selection, Transaction, VisualBlock,
};
use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd};
use std::{ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
mod code;
mod indentation;
mod input_rules;
mod serialization;
mod structure;
pub use structure::{DeleteDirection, ListIndent, StructureFormat};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineFormat {
    Bold,
    Italic,
    Link(String),
    Unlink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParagraphFormat {
    Body,
    Heading(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FragmentKind {
    Text,
    Code,
    SoftBreak,
    HardBreak,
    Raw(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fragment {
    text: String,
    style: InlineStyle,
    kind: FragmentKind,
}

impl EditorDocument {
    pub fn format_inline(
        &mut self,
        selection: Selection,
        format: &InlineFormat,
    ) -> Result<bool, Error> {
        if self.markdown().dialect() == crate::Dialect::PlainText {
            return Err(Error::UnsupportedEdit);
        }
        let range = checked_range(self.projection().text(), selection)?;
        if let InlineFormat::Link(url) = format
            && (url.is_empty()
                || url.len() > 8192
                || url.chars().any(|c| c.is_whitespace() || c.is_control()))
        {
            return Err(Error::UnsupportedEdit);
        }
        if range.is_empty() {
            if matches!(format, InlineFormat::Link(_) | InlineFormat::Unlink) {
                return Err(Error::UnsupportedEdit);
            }
            let mut style = self
                .typing_style
                .clone()
                .unwrap_or_else(|| selection_style(self.projection(), selection));
            let active = match format {
                InlineFormat::Bold => style.bold,
                InlineFormat::Italic => style.italic,
                _ => false,
            };
            set_format(&mut style, format, !active);
            self.typing_style = Some(style);
            return Ok(true);
        }
        let active = self
            .projection()
            .spans()
            .iter()
            .filter(|s| s.display.start < range.end && s.display.end > range.start)
            .any(|s| match format {
                InlineFormat::Bold => s.style.bold,
                InlineFormat::Italic => s.style.italic,
                InlineFormat::Link(_) | InlineFormat::Unlink => false,
            });
        let mut transaction = self.transaction();
        let mut expected = Vec::new();
        for (index, block) in self.projection().blocks().iter().enumerate() {
            if block.display.start >= range.end
                || block.display.end <= range.start
                || !editable(block)
            {
                continue;
            }
            let local = range.start.saturating_sub(block.display.start)
                ..(range.end - block.display.start).min(block.display.len());
            let text = &self.projection().text()[block.display.clone()];
            let selected = &text[local.clone()];
            let (from, to) = if matches!(format, InlineFormat::Unlink) {
                (local.start, local.end)
            } else {
                (
                    local.start + selected.len() - selected.trim_start().len(),
                    local.end - (selected.len() - selected.trim_end().len()),
                )
            };
            if from >= to {
                continue;
            }
            let mut fragments = fragments(self.markdown(), self.projection(), block)?;
            fragments = transform(&fragments, from..to, |mut f| {
                set_format(&mut f.style, format, !active);
                f
            });
            transaction.replace(
                block.source.clone(),
                serialize_leaf(self.markdown(), block, &fragments, &block.style)?,
            )?;
            expected.push((index, fragments));
        }
        let text = self.projection().text().to_owned();
        self.commit_visual(transaction, selection, selection, &expected, &text)
    }

    pub fn format_paragraph(
        &mut self,
        selection: Selection,
        format: ParagraphFormat,
    ) -> Result<bool, Error> {
        if self.markdown().dialect() == crate::Dialect::PlainText {
            return Err(Error::UnsupportedEdit);
        }
        let range = checked_range(self.projection().text(), selection)?;
        let style = match format {
            ParagraphFormat::Body => BlockStyle::Body,
            ParagraphFormat::Heading(level @ 1..=6) => BlockStyle::Heading(level),
            ParagraphFormat::Heading(_) => return Err(Error::UnsupportedEdit),
        };
        let mut transaction = self.transaction();
        let mut expected = Vec::new();
        for (index, block) in self.projection().blocks().iter().enumerate() {
            let selected = if range.is_empty() {
                block.display.start <= range.start && range.start <= block.display.end
            } else {
                block.display.start < range.end && block.display.end > range.start
            };
            if !selected || !editable(block) {
                continue;
            }
            let fragments = fragments(self.markdown(), self.projection(), block)?;
            transaction.replace(
                block.source.clone(),
                serialize_leaf(self.markdown(), block, &fragments, &style)?,
            )?;
            expected.push((index, fragments));
        }
        let text = self.projection().text().to_owned();
        self.commit_visual(transaction, selection, selection, &expected, &text)
    }

    /// Replace writer-visible text without exposing Markdown delimiters. Newlines
    /// split paragraphs; a selection across paragraphs joins the retained ends.
    pub fn replace_visual(&mut self, selection: Selection, text: &str) -> Result<bool, Error> {
        self.replace_visible(selection, selection, text)
    }

    /// Delete a native word/grapheme range, retaining the original caret for undo.
    pub fn delete_visual(
        &mut self,
        selection: Selection,
        range: Range<usize>,
    ) -> Result<bool, Error> {
        checked_range(self.projection().text(), selection)?;
        if !selection.range().is_empty() && selection.range() != range
            || selection.range().is_empty()
                && selection.focus != range.start
                && selection.focus != range.end
        {
            return Err(Error::InvalidRange);
        }
        self.replace_visible(
            selection,
            Selection {
                anchor: range.start,
                focus: range.end,
            },
            "",
        )
    }

    fn replace_visible(
        &mut self,
        before: Selection,
        selection: Selection,
        text: &str,
    ) -> Result<bool, Error> {
        let range = checked_range(self.projection().text(), selection)?;
        if text.len() > crate::MAX_SOURCE_BYTES {
            return Err(Error::Limit);
        }
        if range.is_empty() && text.is_empty() {
            return Ok(false);
        }
        if self.markdown().dialect() == crate::Dialect::PlainText {
            let mut transaction = self.transaction();
            transaction.replace(range.clone(), text)?;
            let mut expected = self.source().to_owned();
            expected.replace_range(range.clone(), text);
            return self.commit_visual(
                transaction,
                before,
                Selection::caret(range.start + text.len()),
                &[],
                &expected,
            );
        }
        if self.projection().blocks().is_empty() && range.is_empty() {
            return self.replace_hidden_body(before, text);
        }
        if let Some(index) = self.projection().blocks().iter().position(|b| {
            b.display.start <= range.start
                && range.end <= b.display.end
                && matches!(b.style, BlockStyle::Code(_))
        }) {
            return self.replace_code(before, selection, text, index);
        }
        let leaf = self
            .projection()
            .blocks()
            .iter()
            .enumerate()
            .find(|(_, b)| {
                b.display.start <= range.start && range.end <= b.display.end && editable(b)
            });
        if leaf.is_some_and(|(_, b)| {
            (b.list_depth > 0 || b.quote_depth > 0)
                && (text.contains(['\r', '\n']) || b.display.is_empty())
        }) {
            return self.replace_in_structure(before, selection, text);
        }
        if text.contains(['\r', '\n']) || leaf.is_none() {
            return self.replace_paragraphs(before, selection, text);
        }
        let (index, block) = leaf.ok_or(Error::UnsupportedEdit)?;
        if range == (0..self.projection().text().len()) && text.is_empty() {
            let mut transaction = self.transaction();
            transaction.replace(0..self.source().len(), "")?;
            return self.commit_visual(transaction, before, Selection::caret(0), &[], "");
        }
        let local = range.start - block.display.start..range.end - block.display.start;
        let original = fragments(self.markdown(), self.projection(), block)?;
        let mut style = self
            .typing_style
            .clone()
            .unwrap_or_else(|| selection_style(self.projection(), selection));
        if text.trim().is_empty() {
            style.bold = false;
            style.italic = false;
        }
        let mut changed = slice_fragments(&original, 0..local.start);
        if !text.is_empty() {
            changed.push(Fragment {
                text: text.into(),
                kind: if style.code {
                    FragmentKind::Code
                } else {
                    FragmentKind::Text
                },
                style,
            });
        }
        changed.extend(slice_fragments(&original, local.end..block.display.len()));
        let mut transaction = self.transaction();
        transaction.replace(
            block.source.clone(),
            serialize_leaf(self.markdown(), block, &changed, &block.style)?,
        )?;
        let caret = Selection::caret(range.start + text.len());
        let mut expected_text = self.projection().text().to_owned();
        expected_text.replace_range(range, text);
        self.commit_visual(
            transaction,
            before,
            caret,
            &[(index, changed)],
            &expected_text,
        )
    }

    fn replace_hidden_body(&mut self, before: Selection, text: &str) -> Result<bool, Error> {
        // CommonMark can omit every block in a reference-only source.
        // Generate literal native input using Loom's empty-body semantics,
        // then prove the result with the actual document dialect.
        let mut body = EditorDocument::new("", crate::Dialect::Loom)?;
        body.typing_style.clone_from(&self.typing_style);
        body.replace_visual(Selection::caret(0), text)?;
        let mut replacement = empty_body_prefix(self.source());
        replacement.push_str(body.source());
        let mut transaction = self.transaction();
        transaction.replace(self.source().len()..self.source().len(), replacement)?;
        self.commit_visual(
            transaction,
            before,
            body.visual_selection()?,
            &[],
            body.projection().text(),
        )
    }

    fn replace_paragraphs(
        &mut self,
        before_selection: Selection,
        selection: Selection,
        text: &str,
    ) -> Result<bool, Error> {
        let range = selection.range();
        let all = !range.is_empty() && range == (0..self.projection().text().len());
        let blocks = self.projection().blocks();
        let first = block_at(blocks, range.start)?;
        let last = block_at(blocks, range.end)?;
        let text = if !all && matches!(blocks[first].style, BlockStyle::Code(_)) {
            std::borrow::Cow::Borrowed(text)
        } else {
            std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
        };
        if !all
            && blocks[first..=last]
                .iter()
                .any(|b| !editable(b) || b.list_depth > 0 || b.quote_depth > 0)
        {
            return self.replace_across_structure(before_selection, selection, &text, first, last);
        }
        let start = &blocks[first];
        let end = &blocks[last];
        let before = if all {
            vec![]
        } else {
            slice_fragments(
                &fragments(self.markdown(), self.projection(), start)?,
                0..range.start - start.display.start,
            )
        };
        let after = if all {
            vec![]
        } else {
            slice_fragments(
                &fragments(self.markdown(), self.projection(), end)?,
                range.end - end.display.start..end.display.len(),
            )
        };
        let source_range = if all {
            0..self.source().len()
        } else {
            start.source.start..end.source.end
        };
        let newline = preferred_newline(&self.source()[source_range.clone()]);
        let inserted: Vec<_> = text.split('\n').collect();
        let mut replacement = if all {
            String::new()
        } else {
            structure::retained_references(self, source_range.clone())?
        };
        let mut expected = Vec::with_capacity(inserted.len());
        let mut typing = self
            .typing_style
            .clone()
            .unwrap_or_else(|| selection_style(self.projection(), selection));
        typing.code = false;
        for (index, value) in inserted.iter().enumerate() {
            let mut changed = if index == 0 { before.clone() } else { vec![] };
            if !value.is_empty() {
                changed.push(Fragment {
                    text: (*value).into(),
                    style: typing.clone(),
                    kind: FragmentKind::Text,
                });
            }
            if index + 1 == inserted.len() {
                changed.extend(after.clone());
            }
            // A new paragraph after a heading returns to body style.
            let style = if index == 0 && editable(start) && !changed.is_empty() {
                start.style.clone()
            } else {
                BlockStyle::Body
            };
            if index > 0 {
                replacement.push_str(newline);
                replacement.push_str(newline);
            }
            let mut leaf = start.clone();
            leaf.source = if index == 0 {
                source_range.start..source_range.start
            } else {
                0..0
            };
            replacement.push_str(&serialize_leaf(self.markdown(), &leaf, &changed, &style)?);
            expected.push((first + index, changed));
        }
        if !all {
            let original_end = &self.source()[end.source.clone()];
            replacement
                .push_str(&original_end[original_end.trim_end_matches(['\r', '\n']).len()..]);
        }
        let mut transaction = self.transaction();
        transaction.replace(source_range, replacement)?;
        let mut expected_text = self.projection().text().to_owned();
        expected_text.replace_range(range.clone(), &text);
        self.commit_visual(
            transaction,
            before_selection,
            Selection::caret(range.start + text.len()),
            &expected,
            &expected_text,
        )
    }

    fn commit_visual(
        &mut self,
        mut transaction: Transaction,
        before: Selection,
        selection: Selection,
        expected: &[(usize, Vec<Fragment>)],
        expected_text: &str,
    ) -> Result<bool, Error> {
        if transaction.edits().is_empty() {
            return Ok(false);
        }
        transaction.before_visual(before);
        // The semantic validator installs the final visual selection. Preparing
        // this candidate must not guess a source coordinate inside an entity.
        transaction.set_selection(Selection::caret(0));
        self.apply_checked(transaction, |markdown, projection| {
            if projection.text() != expected_text {
                return Err(Error::SerializationMismatch);
            }
            for (index, expected) in expected {
                let block = projection
                    .blocks()
                    .get(*index)
                    .ok_or(Error::SerializationMismatch)?;
                let actual = fragments(markdown, projection, block)?;
                if !same_fragments(&normalize_fragments(expected), &actual) {
                    return Err(Error::SerializationMismatch);
                }
            }
            let snap = |offset| {
                projection
                    .text()
                    .grapheme_indices(true)
                    .map(|(i, _)| i)
                    .chain([projection.text().len()])
                    .find(|&i| i >= offset)
                    .ok_or(Error::InvalidRange)
            };
            Ok(Some(EditorSelection::Visual(
                Selection {
                    anchor: snap(selection.anchor)?,
                    focus: snap(selection.focus)?,
                },
                Bias::Before,
            )))
        })
    }
}

fn checked_range(text: &str, selection: Selection) -> Result<Range<usize>, Error> {
    let boundaries = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect::<Vec<_>>();
    if boundaries.binary_search(&selection.anchor).is_err()
        || boundaries.binary_search(&selection.focus).is_err()
    {
        return Err(Error::InvalidRange);
    }
    Ok(selection.range())
}
fn editable(block: &VisualBlock) -> bool {
    matches!(block.style, BlockStyle::Body | BlockStyle::Heading(_))
}

fn selection_style(projection: &Projection, selection: Selection) -> InlineStyle {
    let range = selection.range();
    if range.is_empty() {
        return caret_style_at(projection, range.start);
    }
    // Replacing selected text inherits the first selected span, rather than
    // the text preceding the selection.
    projection
        .spans()
        .iter()
        .find(|s| s.display.start <= range.start && range.start < s.display.end)
        .map_or_else(InlineStyle::default, |s| s.style.clone())
}

pub(crate) fn caret_style_at(projection: &Projection, offset: usize) -> InlineStyle {
    let Ok(index) = block_at(projection.blocks(), offset) else {
        return InlineStyle::default();
    };
    let block = &projection.blocks()[index];
    if block.display.is_empty() {
        return InlineStyle::default();
    }
    let spans = projection.spans();
    let index = spans.partition_point(|span| span.display.end <= offset);
    let after = spans
        .get(index)
        .filter(|span| span.display.start <= offset && span.display.end <= block.display.end);
    if let Some(span) = after
        && span.display.start < offset
    {
        return span.style.clone();
    }
    let before = index
        .checked_sub(1)
        .and_then(|i| spans.get(i))
        .filter(|span| span.display.end == offset && block.display.start <= span.display.start);
    // Match the reviewed editor's $from.marks(): preceding marks are inclusive
    // at an inline boundary; the first span supplies paragraph-start marks.
    // Links are not inclusive, so they survive a boundary only inside the same
    // link (possibly with different emphasis on either side).
    let mut style = before
        .or(after)
        .map_or_else(InlineStyle::default, |s| s.style.clone());
    if !before.zip(after).is_some_and(|(a, b)| {
        a.style.link == b.style.link && a.style.link_title == b.style.link_title
    }) {
        style.link = None;
        style.link_title = None;
    }
    style
}
fn set_format(style: &mut InlineStyle, format: &InlineFormat, enabled: bool) {
    match format {
        InlineFormat::Bold => style.bold = enabled,
        InlineFormat::Italic => style.italic = enabled,
        InlineFormat::Link(url) => {
            style.link = Some(Arc::from(url.as_str()));
            style.link_title = None;
        }
        InlineFormat::Unlink => {
            style.link = None;
            style.link_title = None;
        }
    }
}

fn block_at(blocks: &[VisualBlock], offset: usize) -> Result<usize, Error> {
    let index = blocks.partition_point(|b| b.display.end < offset);
    blocks
        .get(index)
        .filter(|b| b.display.start <= offset)
        .map(|_| index)
        .ok_or(Error::UnsupportedEdit)
}

fn fragments(
    markdown: &Markdown,
    projection: &Projection,
    block: &VisualBlock,
) -> Result<Vec<Fragment>, Error> {
    let spans = projection.spans();
    let start = spans.partition_point(|span| span.display.end <= block.display.start);
    spans[start..]
        .iter()
        .take_while(|span| span.display.start < block.display.end)
        .filter(|s| s.display.start >= block.display.start && s.display.end <= block.display.end)
        .map(|span| {
            let node = markdown.node(span.node).ok_or(Error::InvalidParse)?;
            let kind = match &node.kind {
                NodeKind::Code(_) => FragmentKind::Code,
                NodeKind::SoftBreak => FragmentKind::SoftBreak,
                NodeKind::HardBreak => FragmentKind::HardBreak,
                NodeKind::Image { .. } | NodeKind::Html(_) => {
                    FragmentKind::Raw(markdown.source()[node.source.clone()].into())
                }
                _ => FragmentKind::Text,
            };
            Ok(Fragment {
                text: projection.text()[span.display.clone()].into(),
                style: span.style.clone(),
                kind,
            })
        })
        .collect()
}

fn slice_fragments(fragments: &[Fragment], range: Range<usize>) -> Vec<Fragment> {
    let mut result = Vec::new();
    let mut offset = 0;
    for f in fragments {
        let start = range.start.saturating_sub(offset).min(f.text.len());
        let end = range.end.saturating_sub(offset).min(f.text.len());
        if start < end {
            result.push(Fragment {
                text: f.text[start..end].into(),
                style: f.style.clone(),
                kind: f.kind.clone(),
            });
        }
        offset += f.text.len();
    }
    result
}

fn transform(
    fragments: &[Fragment],
    range: Range<usize>,
    change: impl Fn(Fragment) -> Fragment,
) -> Vec<Fragment> {
    let length = fragments.iter().map(|f| f.text.len()).sum();
    let mut result = slice_fragments(fragments, 0..range.start);
    result.extend(
        slice_fragments(fragments, range.clone())
            .into_iter()
            .map(change),
    );
    result.extend(slice_fragments(fragments, range.end..length));
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mark {
    Link(Arc<str>, Option<Arc<str>>),
    Bold,
    Italic,
}
impl Mark {
    fn is_set(&self, style: &InlineStyle) -> bool {
        match self {
            Self::Link(url, title) => {
                style.link.as_ref() == Some(url) && style.link_title == *title
            }
            Self::Bold => style.bold,
            Self::Italic => style.italic,
        }
    }
    fn slot(&self) -> usize {
        match self {
            Self::Link(_, _) => 0,
            Self::Bold => 1,
            Self::Italic => 2,
        }
    }
    fn start(&self) -> Event<'static> {
        match self {
            Self::Bold => Event::Start(Tag::Strong),
            Self::Italic => Event::Start(Tag::Emphasis),
            Self::Link(_, _) => Event::InlineHtml("[".into()),
        }
    }
    fn end(&self) -> Event<'static> {
        match self {
            Self::Bold => Event::End(TagEnd::Strong),
            Self::Italic => Event::End(TagEnd::Emphasis),
            // The general serializer writes destinations without escaping and
            // titles without entity protection. Emit these delimiters directly
            // so a literal entity, backslash or unmatched parenthesis cannot
            // change the authoritative link target when reparsed.
            Self::Link(url, title) => {
                let mut source = format!("]({}", escape_link_attribute(url));
                if let Some(title) = title.as_ref().filter(|t| !t.is_empty()) {
                    source.push_str(" \"");
                    source.push_str(&escape_link_attribute(title));
                    source.push('"');
                }
                source.push(')');
                Event::InlineHtml(source.into())
            }
        }
    }
}

fn escape_link_attribute(value: &str) -> String {
    use std::fmt::Write;
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_whitespace()
            || c.is_control()
            || matches!(c, '&' | '<' | '>' | '\\' | '(' | ')' | '"')
        {
            // Character references are decoded by the Markdown parser, unlike
            // percent encoding, which would alter the saved link destination.
            write!(escaped, "&#{};", u32::from(c)).expect("writing to a String");
        } else {
            escaped.push(c);
        }
    }
    escaped
}

fn serialize_leaf(
    markdown: &Markdown,
    block: &VisualBlock,
    fragments: &[Fragment],
    style: &BlockStyle,
) -> Result<String, Error> {
    let fragments = normalize_fragments(fragments);
    let tag = match style {
        BlockStyle::Body => Tag::Paragraph,
        BlockStyle::Heading(level) => Tag::Heading {
            level: HeadingLevel::try_from(usize::from(*level))
                .map_err(|_| Error::UnsupportedEdit)?,
            id: None,
            classes: vec![],
            attrs: vec![],
        },
        _ => return Err(Error::UnsupportedEdit),
    };
    let end = tag.to_end();
    let mut events = vec![Event::Start(tag)];
    let mut marks: Vec<Mark> = vec![];
    let ends = mark_ends(&fragments);
    let mut line_start = true;
    let mut adjacent_marks = false;
    for (index, fragment) in fragments.iter().enumerate() {
        let mut entering = Vec::new();
        if let Some(url) = &fragment.style.link {
            entering.push(Mark::Link(url.clone(), fragment.style.link_title.clone()));
        }
        if fragment.style.bold {
            entering.push(Mark::Bold);
        }
        if fragment.style.italic {
            entering.push(Mark::Italic);
        }
        // Keep existing outer marks open. New marks that continue farther belong
        // outside shorter runs; otherwise adjacent star delimiters can change
        // the visible text when bold is added inside inherited emphasis.
        let mut next: Vec<_> = marks
            .iter()
            .filter(|mark| mark.is_set(&fragment.style))
            .cloned()
            .collect();
        entering.retain(|mark| !next.contains(mark));
        entering.sort_by_key(|mark| std::cmp::Reverse(ends[index][mark.slot()]));
        next.extend(entering);
        let shared = marks.iter().zip(&next).take_while(|(a, b)| a == b).count();
        adjacent_marks |= shared < marks.len() && shared < next.len();
        for old in marks[shared..].iter().rev() {
            events.push(old.end());
        }
        for new in &next[shared..] {
            events.push(new.start());
        }
        marks = next;
        events.push(match &fragment.kind {
            // The serializer's Text escaping assumes parser-tokenized input.
            // Native typing supplies arbitrary text, including unfinished list
            // markers and delimiters in the middle of a span.
            FragmentKind::Text => {
                Event::InlineHtml(escape_literal(&fragment.text, line_start).into())
            }
            FragmentKind::Code => Event::Code(fragment.text.as_str().into()),
            FragmentKind::SoftBreak => Event::SoftBreak,
            FragmentKind::HardBreak => Event::HardBreak,
            FragmentKind::Raw(source) => Event::InlineHtml(source.as_str().into()),
        });
        line_start = matches!(
            fragment.kind,
            FragmentKind::SoftBreak | FragmentKind::HardBreak
        );
    }
    for mark in marks.iter().rev() {
        events.push(mark.end());
    }
    events.push(Event::End(end));
    let mut output = serialization::markup(&events, &fragments, style, adjacent_marks)?;
    let source = &markdown.source()[block.source.clone()];
    let suffix_start = source.trim_end_matches(['\r', '\n']).len();
    let suffix = &source[suffix_start..];
    let newline = preferred_newline(source);
    let line_start = markdown.source()[..block.source.start]
        .rfind(['\r', '\n'])
        .map_or(0, |i| i + 1);
    let prefix = markdown.source()[line_start..block.source.start]
        .chars()
        .map(|c| {
            if c == '>' || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>();
    if output.contains('\n') {
        output = output.replace('\n', &format!("{newline}{prefix}"));
    }
    output.push_str(suffix);
    if block.source.is_empty()
        && block.source.start == markdown.source().len()
        && block.list_depth == 0
        && block.quote_depth == 0
    {
        output.insert_str(0, &empty_body_prefix(markdown.source()));
    }
    Ok(output)
}

fn mark_ends(fragments: &[Fragment]) -> Vec<[usize; 3]> {
    let mut ends = vec![[0; 3]; fragments.len()];
    for i in (0..fragments.len()).rev() {
        let style = &fragments[i].style;
        ends[i] = [i + 1; 3];
        if let Some(next) = fragments.get(i + 1) {
            for (slot, continued) in [
                style.link.is_some()
                    && style.link == next.style.link
                    && style.link_title == next.style.link_title,
                style.bold && next.style.bold,
                style.italic && next.style.italic,
            ]
            .into_iter()
            .enumerate()
            {
                if continued {
                    ends[i][slot] = ends[i + 1][slot];
                }
            }
        }
    }
    ends
}

fn empty_body_prefix(source: &str) -> String {
    if source.is_empty() {
        return String::new();
    }
    let tail = &source[source.trim_end_matches(['\r', '\n']).len()..];
    let count = tail.replace("\r\n", "\n").chars().count();
    preferred_newline(source).repeat(2_usize.saturating_sub(count))
}

fn escape_literal(text: &str, line_start: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let leading = if line_start {
        text.len() - text.trim_start_matches(' ').len()
    } else {
        0
    };
    let digits = if line_start {
        text[leading..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count()
    } else {
        0
    };
    for (index, c) in text.char_indices() {
        if c == '\r' || c == '\n' {
            // Literal line endings can arrive from a joined code block or an
            // existing character reference. Keep them inside this text block.
            out.push_str(if c == '\r' { "&#13;" } else { "&#10;" });
            continue;
        }
        if index < leading {
            out.push_str("&#32;");
            continue;
        }
        let ordered =
            digits > 0 && digits <= 9 && index == leading + digits && matches!(c, '.' | ')');
        let marker = line_start && index == leading && matches!(c, '-' | '+' | '>');
        let entity = c == '&'
            && text[index + 1..].split_once(';').is_some_and(|(name, _)| {
                !name.is_empty()
                    && name.len() <= 32
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '#')
            });
        if ordered
            || marker
            || entity
            || matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '#')
        {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn preferred_newline(source: &str) -> &str {
    if source.contains("\r\n") {
        "\r\n"
    } else if source.contains('\r') {
        "\r"
    } else {
        "\n"
    }
}

fn same_fragments(expected: &[Fragment], actual: &[Fragment]) -> bool {
    let mut left = expected.iter().filter(|f| !f.text.is_empty());
    let mut right = actual.iter().filter(|f| !f.text.is_empty());
    let (mut a, mut b) = (left.next(), right.next());
    let (mut ai, mut bi) = (0, 0);
    while let (Some(af), Some(bf)) = (a, b) {
        let count = (af.text.len() - ai).min(bf.text.len() - bi);
        if af.style != bf.style
            || af.text.as_bytes()[ai..ai + count] != bf.text.as_bytes()[bi..bi + count]
        {
            return false;
        }
        ai += count;
        bi += count;
        if ai == af.text.len() {
            a = left.next();
            ai = 0;
        }
        if bi == bf.text.len() {
            b = right.next();
            bi = 0;
        }
    }
    a.is_none() && b.is_none()
}

fn coalesce(fragments: &[Fragment]) -> Vec<Fragment> {
    let mut result: Vec<Fragment> = Vec::new();
    for fragment in fragments {
        if let Some(previous) = result.last_mut()
            && previous.kind == fragment.kind
            && matches!(fragment.kind, FragmentKind::Text | FragmentKind::Code)
            && previous.style == fragment.style
        {
            previous.text.push_str(&fragment.text);
        } else {
            result.push(fragment.clone());
        }
    }
    result
}

/// `CommonMark` emphasis delimiters cannot surround leading/trailing whitespace.
/// Keep those characters outside the mark when a split creates a new boundary.
fn normalize_fragments(fragments: &[Fragment]) -> Vec<Fragment> {
    let mut fragments = coalesce(fragments);
    for bold in [true, false] {
        let active = |f: &Fragment| if bold { f.style.bold } else { f.style.italic };
        let mut normalized = Vec::new();
        let mut index = 0;
        while index < fragments.len() {
            if !active(&fragments[index]) {
                normalized.push(fragments[index].clone());
                index += 1;
                continue;
            }
            let end = index + fragments[index..].iter().take_while(|f| active(f)).count();
            let run = &fragments[index..end];
            let length = run.iter().map(|f| f.text.len()).sum::<usize>();
            let mut leading = 0;
            for f in run {
                leading += f.text.len() - f.text.trim_start().len();
                if !f.text.trim().is_empty() {
                    break;
                }
            }
            let mut trailing = 0;
            for f in run.iter().rev() {
                trailing += f.text.len() - f.text.trim_end().len();
                if !f.text.trim().is_empty() {
                    break;
                }
            }
            let clear = |mut f: Fragment| {
                if bold {
                    f.style.bold = false;
                } else {
                    f.style.italic = false;
                }
                f
            };
            normalized.extend(slice_fragments(run, 0..leading).into_iter().map(clear));
            let middle_end = length.saturating_sub(trailing).max(leading);
            normalized.extend(slice_fragments(run, leading..middle_end));
            normalized.extend(
                slice_fragments(run, middle_end..length)
                    .into_iter()
                    .map(clear),
            );
            index = end;
        }
        fragments = normalized;
    }
    coalesce(&fragments)
}
