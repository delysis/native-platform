use crate::{Error, MAX_SOURCE_BYTES, Markdown, NodeId, NodeKind};
use std::{ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Before,
    After,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<Arc<str>>,
    pub link_title: Option<Arc<str>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapping {
    /// Display bytes are identical to the source slice.
    Direct,
    /// Entity, escape, code span, break or image: only endpoints map exactly.
    Decoded,
    /// Visual paragraph separation; requires a structural editing operation.
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpan {
    pub node: NodeId,
    pub display: Range<usize>,
    pub source: Range<usize>,
    pub style: InlineStyle,
    pub mapping: Mapping,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockStyle {
    Body,
    Heading(u8),
    Code(String),
    Rule,
    Raw,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualBlock {
    pub node: NodeId,
    pub source: Range<usize>,
    pub display: Range<usize>,
    pub style: BlockStyle,
    pub quote_depth: usize,
    pub list_depth: usize,
    pub marker: Option<String>,
    pub empty_caret_source: usize,
}

#[derive(Clone, Debug)]
pub struct Projection {
    text: String,
    spans: Vec<TextSpan>,
    blocks: Vec<VisualBlock>,
    graphemes: Vec<usize>,
    source_len: usize,
}

impl Projection {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn spans(&self) -> &[TextSpan] {
        &self.spans
    }
    pub fn blocks(&self) -> &[VisualBlock] {
        &self.blocks
    }

    pub fn source_at(&self, offset: usize, bias: Bias) -> Result<usize, Error> {
        if self.graphemes.binary_search(&offset).is_err() {
            return Err(Error::InvalidRange);
        }
        if let Some(block) = self
            .blocks
            .iter()
            .find(|b| b.display.is_empty() && b.display.start == offset)
        {
            return Ok(block.empty_caret_source);
        }
        let index = self
            .spans
            .partition_point(|span| span.display.end <= offset);
        if let Some(span) = self.spans.get(index)
            && span.display.start < offset
            && offset < span.display.end
        {
            return if span.mapping == Mapping::Direct {
                Ok(span.source.start + offset - span.display.start)
            } else {
                Err(Error::AmbiguousBoundary)
            };
        }
        let previous = index.checked_sub(1).and_then(|i| self.spans.get(i));
        let next = self.spans.get(index);
        let before = previous
            .filter(|s| s.display.end == offset)
            .map(|s| s.source.end);
        let after = next
            .filter(|s| s.display.start == offset)
            .map(|s| s.source.start);
        match bias {
            Bias::Before => before.or(after),
            Bias::After => after.or(before),
        }
        .or_else(|| {
            self.text.is_empty().then_some(if bias == Bias::Before {
                0
            } else {
                self.source_len
            })
        })
        .ok_or(Error::AmbiguousBoundary)
    }

    /// Map source positions inside hidden markup toward an adjacent visible
    /// boundary. Callers retaining a source selection should keep its exact bytes.
    pub fn display_at(&self, offset: usize, bias: Bias) -> Result<usize, Error> {
        if offset > self.source_len {
            return Err(Error::InvalidRange);
        }
        if let Some(block) = self
            .blocks
            .iter()
            .find(|b| b.display.is_empty() && b.empty_caret_source == offset)
        {
            return Ok(block.display.start);
        }
        let index = self.spans.partition_point(|span| span.source.end <= offset);
        if let Some(span) = self.spans.get(index)
            && span.source.start <= offset
            && offset < span.source.end
        {
            let mapped = if span.mapping == Mapping::Direct {
                span.display.start + offset - span.source.start
            } else if bias == Bias::Before {
                span.display.start
            } else {
                span.display.end
            };
            return self
                .graphemes
                .binary_search(&mapped)
                .map(|_| mapped)
                .map_err(|_| Error::InvalidRange);
        }
        Ok(match bias {
            Bias::Before => index
                .checked_sub(1)
                .map_or(0, |i| self.spans[i].display.end),
            Bias::After => self
                .spans
                .get(index)
                .map_or(self.text.len(), |s| s.display.start),
        })
    }
}

#[derive(Clone, Debug, Default)]
struct Context {
    quote: usize,
    list: usize,
    marker: Option<String>,
}

impl Markdown {
    pub fn project(&self) -> Result<Projection, Error> {
        let mut out = Projection {
            text: String::new(),
            spans: vec![],
            blocks: vec![],
            graphemes: vec![],
            source_len: self.source().len(),
        };
        self.project_blocks(0, &Context::default(), &mut out)?;
        out.graphemes = out
            .text
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain([out.text.len()])
            .collect();
        Ok(out)
    }

    fn project_blocks(
        &self,
        id: NodeId,
        context: &Context,
        out: &mut Projection,
    ) -> Result<(), Error> {
        let node = self.node(id).ok_or(Error::InvalidParse)?;
        match &node.kind {
            NodeKind::Document => {
                for &child in &node.children {
                    self.project_blocks(child, context, out)?;
                }
            }
            NodeKind::Quote => {
                let mut next = Context {
                    quote: context.quote + 1,
                    ..context.clone()
                };
                for &child in &node.children {
                    self.project_blocks(child, &next, out)?;
                    next.marker = None;
                }
            }
            NodeKind::List(order) => {
                for (index, &child) in node.children.iter().enumerate() {
                    let marker = order.map_or_else(
                        || "•".into(),
                        |start| format!("{}.", start.saturating_add(index as u64)),
                    );
                    self.project_blocks(
                        child,
                        &Context {
                            list: context.list + 1,
                            marker: Some(marker),
                            ..context.clone()
                        },
                        out,
                    )?;
                }
            }
            NodeKind::Item => {
                // Tight lists omit Paragraph events. Group their inline children
                // without confusing nested lists with text in the same paragraph.
                let mut inline = Vec::new();
                let mut next = context.clone();
                if node.children.is_empty() {
                    self.project_leaf(id, &[], BlockStyle::Body, &next, out)?;
                }
                for &child in &node.children {
                    if is_inline(&self.nodes()[child].kind) {
                        inline.push(child);
                    } else {
                        if !inline.is_empty() {
                            self.project_leaf(id, &inline, BlockStyle::Body, &next, out)?;
                            inline.clear();
                            next.marker = None;
                        }
                        self.project_blocks(child, &next, out)?;
                        next.marker = None;
                    }
                }
                if !inline.is_empty() {
                    self.project_leaf(id, &inline, BlockStyle::Body, &next, out)?;
                }
            }
            NodeKind::Paragraph => {
                self.project_leaf(id, &node.children, BlockStyle::Body, context, out)?;
            }
            NodeKind::Heading(level) => self.project_leaf(
                id,
                &node.children,
                BlockStyle::Heading(*level),
                context,
                out,
            )?,
            NodeKind::CodeBlock { language, .. } => self.project_leaf(
                id,
                &node.children,
                BlockStyle::Code(language.clone()),
                context,
                out,
            )?,
            NodeKind::Rule => self.project_leaf(id, &[id], BlockStyle::Rule, context, out)?,
            NodeKind::HtmlBlock | NodeKind::Html(_) => {
                let children = if node.children.is_empty() {
                    vec![id]
                } else {
                    node.children.clone()
                };
                self.project_leaf(id, &children, BlockStyle::Raw, context, out)?;
            }
            _ => return Err(Error::InvalidParse),
        }
        Ok(())
    }

    fn project_leaf(
        &self,
        id: NodeId,
        children: &[NodeId],
        style: BlockStyle,
        context: &Context,
        out: &mut Projection,
    ) -> Result<(), Error> {
        let node = &self.nodes()[id];
        let source = if node.kind == NodeKind::Item {
            children
                .first()
                .map_or(node.source.start, |&i| self.nodes()[i].source.start)
                ..children
                    .last()
                    .map_or(node.source.end, |&i| self.nodes()[i].source.end)
        } else {
            node.source.clone()
        };
        if let Some(previous) = out.blocks.last() {
            // One editable boundary between blocks. Paragraph spacing belongs
            // to native layout metrics, not an extra selectable blank character.
            let separator = "\n";
            let range = previous.source.end.min(source.start)..source.start;
            append(
                out,
                id,
                separator,
                range,
                InlineStyle::default(),
                Mapping::Separator,
            )?;
        }
        let start = out.text.len();
        let inline_style = InlineStyle {
            code: matches!(style, BlockStyle::Code(_) | BlockStyle::Raw),
            ..InlineStyle::default()
        };
        for &child in children {
            self.project_inline(child, &inline_style, out)?;
        }
        let empty_caret_source = if matches!(style, BlockStyle::Code(_)) {
            hide_code_ending(out, start, self.source());
            children.first().map_or_else(
                || {
                    let raw = &self.source()[source.clone()];
                    let end = raw.find(['\r', '\n']).unwrap_or(raw.len());
                    source.start
                        + end
                        + if raw[end..].starts_with("\r\n") {
                            2
                        } else {
                            usize::from(end < raw.len())
                        }
                },
                |&child| self.nodes()[child].source.start,
            )
        } else {
            source.start
                + self.source()[source.clone()]
                    .trim_end_matches(['\r', '\n'])
                    .len()
        };
        out.blocks.push(VisualBlock {
            node: id,
            source,
            display: start..out.text.len(),
            style,
            quote_depth: context.quote,
            list_depth: context.list,
            marker: context.marker.clone(),
            empty_caret_source,
        });
        Ok(())
    }

    fn project_inline(
        &self,
        id: NodeId,
        style: &InlineStyle,
        out: &mut Projection,
    ) -> Result<(), Error> {
        let node = &self.nodes()[id];
        let mut next = style.clone();
        let text = match &node.kind {
            NodeKind::Text(text) | NodeKind::Html(text) => Some(text.as_str()),
            NodeKind::Code(text) => {
                next.code = true;
                Some(text.as_str())
            }
            NodeKind::SoftBreak => Some(" "),
            NodeKind::HardBreak => Some("\n"),
            NodeKind::Rule | NodeKind::Image { .. } => Some("\u{fffc}"),
            NodeKind::Strong => {
                next.bold = true;
                None
            }
            NodeKind::Emphasis => {
                next.italic = true;
                None
            }
            NodeKind::Link { destination, title } => {
                next.link = Some(Arc::from(destination.as_str()));
                next.link_title = (!title.is_empty()).then(|| Arc::from(title.as_str()));
                None
            }
            _ => return Err(Error::InvalidParse),
        };
        if let Some(text) = text {
            let mapping = if self.source().get(node.source.clone()) == Some(text) {
                Mapping::Direct
            } else {
                Mapping::Decoded
            };
            append(out, id, text, node.source.clone(), next, mapping)?;
        } else {
            for &child in &node.children {
                self.project_inline(child, &next, out)?;
            }
        }
        Ok(())
    }
}

/// A code block's final physical ending separates content from its fence. Keep
/// every preceding ending and its exact byte anchors; the delimiter is syntax.
fn hide_code_ending(out: &mut Projection, start: usize, source: &str) {
    let content = &out.text[start..];
    let count = if content.ends_with("\r\n") {
        2
    } else {
        usize::from(content.ends_with(['\r', '\n']))
    };
    let end = out.text.len() - count;
    out.text.truncate(end);
    while let Some(span) = out.spans.last_mut() {
        if span.display.start >= end && span.display.start >= start {
            out.spans.pop();
        } else {
            if span.display.end > end {
                if span.mapping == Mapping::Direct {
                    span.source.end -= span.display.end - end;
                } else {
                    let raw = &source[span.source.clone()];
                    span.source.end -= if raw.ends_with("\r\n") {
                        2
                    } else {
                        usize::from(raw.ends_with(['\r', '\n']))
                    };
                }
                span.display.end = end;
            }
            break;
        }
    }
}

fn is_inline(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Text(_)
            | NodeKind::Code(_)
            | NodeKind::Emphasis
            | NodeKind::Strong
            | NodeKind::Link { .. }
            | NodeKind::Image { .. }
            | NodeKind::Html(_)
            | NodeKind::SoftBreak
            | NodeKind::HardBreak
    )
}

fn append(
    out: &mut Projection,
    node: NodeId,
    text: &str,
    source: Range<usize>,
    style: InlineStyle,
    mapping: Mapping,
) -> Result<(), Error> {
    if text.is_empty() {
        return Ok(());
    }
    if out.text.len().saturating_add(text.len()) > MAX_SOURCE_BYTES
        || out.spans.len() >= crate::MAX_NODES
    {
        return Err(Error::Limit);
    }
    let start = out.text.len();
    out.text.push_str(text);
    out.spans.push(TextSpan {
        node,
        display: start..out.text.len(),
        source,
        style,
        mapping,
    });
    Ok(())
}
