use crate::{Error, MAX_DEPTH, MAX_NODES, MAX_SOURCE_BYTES};
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
use std::{borrow::Cow, ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// Literal UTF-8 editing, including verse and exact mixed line endings.
    PlainText,
    /// `CommonMark` input, retaining source syntax even outside the visual subset.
    CommonMark,
    /// Loom reserves literal tabs for indentation, including leading tabs.
    Loom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Paragraph,
    Heading(u8),
    Quote,
    List(Option<u64>),
    Item,
    CodeBlock {
        language: String,
        fenced: bool,
    },
    Emphasis,
    Strong,
    Link {
        destination: String,
        title: String,
    },
    Image {
        destination: String,
        title: String,
    },
    Text(String),
    Code(String),
    SoftBreak,
    HardBreak,
    Rule,
    HtmlBlock,
    /// Kept literal. Native views must never execute source HTML.
    Html(String),
}

pub type NodeId = usize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    pub source: Range<usize>,
    pub children: Vec<NodeId>,
}

#[derive(Clone, Debug)]
pub struct Markdown {
    source: Arc<str>,
    nodes: Vec<Node>,
    dialect: Dialect,
    references: Vec<ReferenceDefinition>,
}

#[derive(Clone, Debug)]
pub(crate) struct ReferenceDefinition {
    pub source: Range<usize>,
    pub label: String,
    pub destination: String,
    pub title: Option<String>,
}

impl Markdown {
    #[allow(
        clippy::too_many_lines,
        reason = "One bounded parser event pass owns the source mapping and stack"
    )]
    pub fn parse(source: &str, dialect: Dialect) -> Result<Self, Error> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(Error::Limit);
        }
        if dialect == Dialect::PlainText {
            return Ok(Self {
                source: Arc::from(source),
                dialect,
                references: vec![],
                nodes: vec![
                    Node {
                        kind: NodeKind::Document,
                        source: 0..source.len(),
                        children: vec![1],
                    },
                    Node {
                        kind: NodeKind::Paragraph,
                        source: 0..source.len(),
                        children: vec![2],
                    },
                    Node {
                        kind: NodeKind::Text(source.into()),
                        source: 0..source.len(),
                        children: vec![],
                    },
                ],
            });
        }
        let input = ParserInput::new(source, dialect);
        let mut nodes = vec![Node {
            kind: NodeKind::Document,
            source: 0..source.len(),
            children: vec![],
        }];
        let mut stack = vec![0];
        let parser = Parser::new(&input.text);
        let mut references: Vec<_> = parser
            .reference_definitions()
            .iter()
            .map(|(label, definition)| ReferenceDefinition {
                source: input.source_offset(definition.span.start)
                    ..input.source_offset(definition.span.end),
                label: label.into(),
                destination: definition.dest.to_string(),
                title: definition.title.as_ref().map(ToString::to_string),
            })
            .collect();
        if references.len() > MAX_NODES {
            return Err(Error::Limit);
        }
        references.sort_by_key(|reference| reference.source.start);
        let mut owned_bytes = references
            .iter()
            .map(|r| r.label.len() + r.destination.len() + r.title.as_ref().map_or(0, String::len))
            .sum::<usize>();
        if owned_bytes > MAX_SOURCE_BYTES * 4 {
            return Err(Error::Limit);
        }
        for (event, range) in parser.into_offset_iter() {
            if matches!(event, Event::End(_)) {
                if stack.len() <= 1 {
                    return Err(Error::InvalidParse);
                }
                stack.pop();
                continue;
            }
            let container = matches!(event, Event::Start(_));
            let mut kind = match event {
                Event::Start(tag) => match tag {
                    Tag::Paragraph => NodeKind::Paragraph,
                    Tag::Heading { level, .. } => NodeKind::Heading(level as u8),
                    Tag::BlockQuote(_) => NodeKind::Quote,
                    Tag::List(order) => NodeKind::List(order),
                    Tag::Item => NodeKind::Item,
                    Tag::CodeBlock(kind) => match kind {
                        CodeBlockKind::Indented => NodeKind::CodeBlock {
                            language: String::new(),
                            fenced: false,
                        },
                        CodeBlockKind::Fenced(language) => NodeKind::CodeBlock {
                            language: language.into_string(),
                            fenced: true,
                        },
                    },
                    Tag::Emphasis => NodeKind::Emphasis,
                    Tag::Strong => NodeKind::Strong,
                    Tag::Link {
                        dest_url, title, ..
                    } => NodeKind::Link {
                        destination: dest_url.into_string(),
                        title: title.into_string(),
                    },
                    Tag::Image {
                        dest_url, title, ..
                    } => NodeKind::Image {
                        destination: dest_url.into_string(),
                        title: title.into_string(),
                    },
                    Tag::HtmlBlock => NodeKind::HtmlBlock,
                    // Extensions are not enabled. Reject a future parser variant
                    // rather than fabricating a source projection for it.
                    _ => return Err(Error::InvalidParse),
                },
                Event::Text(text) => NodeKind::Text(text.into_string()),
                Event::Code(text) => NodeKind::Code(text.into_string()),
                Event::SoftBreak => NodeKind::SoftBreak,
                Event::HardBreak => NodeKind::HardBreak,
                Event::Rule => NodeKind::Rule,
                Event::Html(text) | Event::InlineHtml(text) => NodeKind::Html(text.into_string()),
                _ => return Err(Error::InvalidParse),
            };
            let mut range = input.source_offset(range.start)..input.source_offset(range.end);
            if range.start > range.end
                || !source.is_char_boundary(range.start)
                || !source.is_char_boundary(range.end)
            {
                return Err(Error::InvalidParse);
            }
            if dialect == Dialect::Loom {
                if matches!(kind, NodeKind::Text(_))
                    && stack
                        .iter()
                        .any(|&i| matches!(nodes[i].kind, NodeKind::CodeBlock { .. }))
                {
                    // pulldown-cmark starts a CRLF code-text event at LF. Keep
                    // the omitted CR in this native literal span, unless the
                    // preceding sibling already owns it. Container prefixes
                    // still follow the parser's per-line source boundaries.
                    let previous_end = stack
                        .last()
                        .and_then(|&parent| nodes[parent].children.last())
                        .map_or(0, |&previous| nodes[previous].source.end);
                    if range.start > previous_end
                        && source.as_bytes().get(range.start) == Some(&b'\n')
                        && source.as_bytes()[range.start - 1] == b'\r'
                    {
                        range.start -= 1;
                    }
                    kind = NodeKind::Text(source[range.clone()].into());
                } else if matches!(kind, NodeKind::Code(_)) {
                    kind = NodeKind::Code(inline_code(&source[range.clone()])?);
                }
            }
            owned_bytes = owned_bytes.saturating_add(match &kind {
                NodeKind::Text(s) | NodeKind::Code(s) | NodeKind::Html(s) => s.len(),
                NodeKind::CodeBlock { language, .. } => language.len(),
                NodeKind::Link { destination, title } | NodeKind::Image { destination, title } => {
                    destination.len() + title.len()
                }
                _ => 0,
            });
            if owned_bytes > MAX_SOURCE_BYTES * 4 {
                return Err(Error::Limit);
            }
            if nodes.len() >= MAX_NODES || stack.len() >= MAX_DEPTH {
                return Err(Error::Limit);
            }
            let id = nodes.len();
            nodes.push(Node {
                kind,
                source: range,
                children: vec![],
            });
            let parent = *stack.last().ok_or(Error::InvalidParse)?;
            nodes[parent].children.push(id);
            if container {
                stack.push(id);
            }
        }
        if stack.len() != 1 {
            return Err(Error::InvalidParse);
        }
        if dialect == Dialect::Loom {
            preserve_whitespace(source, &mut nodes)?;
            preserve_empty_paragraphs(source, &mut nodes)?;
        }
        Ok(Self {
            source: Arc::from(source),
            nodes,
            dialect,
            references,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }
    pub(crate) fn references(&self) -> &[ReferenceDefinition] {
        &self.references
    }
}

fn inline_code(source: &str) -> Result<String, Error> {
    let fence = source.bytes().take_while(|&b| b == b'`').count();
    if fence == 0 || source.len() < fence * 2 {
        return Err(Error::InvalidParse);
    }
    let inner = source[fence..source.len() - fence]
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ");
    Ok(
        if inner.starts_with(' ') && inner.ends_with(' ') && inner.chars().any(|c| c != ' ') {
            inner[1..inner.len() - 1].into()
        } else {
            inner
        },
    )
}

/// `CommonMark` discards indentation and trailing prose spaces. Loom keeps these
/// author-entered bytes visible, without changing code or delimiter semantics.
fn preserve_whitespace(source: &str, nodes: &mut Vec<Node>) -> Result<(), Error> {
    if nodes[0].children.is_empty() && source.chars().all(char::is_whitespace) {
        let only_endings = source.chars().all(|c| c == '\r' || c == '\n');
        nodes.push(Node {
            kind: NodeKind::Paragraph,
            source: 0..if only_endings { 0 } else { source.len() },
            children: if only_endings { vec![] } else { vec![2] },
        });
        nodes[0].children.push(1);
        if !nodes[1].children.is_empty() {
            nodes.push(Node {
                kind: NodeKind::Text(source.into()),
                source: 0..source.len(),
                children: vec![],
            });
        }
        return Ok(());
    }
    let count = nodes.len();
    for id in 1..count {
        if !matches!(
            nodes[id].kind,
            NodeKind::Paragraph | NodeKind::Heading(_) | NodeKind::Item
        ) {
            continue;
        }
        let node = &nodes[id];
        let Some(&first) = node.children.first() else {
            continue;
        };
        let Some(&last) = node.children.last() else {
            continue;
        };
        let start = nodes[first].source.start.max(node.source.start);
        let end = nodes[last].source.end.min(node.source.end);
        let leading = node.source.start..start;
        let tail = &source[end..node.source.end];
        let trailing = end..end + tail.trim_end_matches(['\r', '\n']).len();
        for (range, before) in [(leading, true), (trailing, false)] {
            let text = &source[range.clone()];
            if text.is_empty() || !text.chars().all(|c| c == ' ' || c == '\t') {
                continue;
            }
            if nodes.len() >= MAX_NODES {
                return Err(Error::Limit);
            }
            let child = nodes.len();
            nodes.push(Node {
                kind: NodeKind::Text(text.into()),
                source: range,
                children: vec![],
            });
            if before {
                nodes[id].children.insert(0, child);
            } else {
                nodes[id].children.push(child);
            }
        }
    }
    Ok(())
}

/// Two source line endings delimit a paragraph. Preserve explicitly inserted
/// empty paragraphs in Loom's visual model, including at the document edges.
fn preserve_empty_paragraphs(source: &str, nodes: &mut Vec<Node>) -> Result<(), Error> {
    if nodes[0].children.is_empty() {
        // A reference-definition-only file still needs an editable body. Its
        // caret follows the hidden bytes; visual typing must never splice into
        // a definition or erase it by confusing an empty caret with Select All.
        let content_end = source.trim_end_matches(['\r', '\n']).len();
        let endings =
            line_end_positions(&source[content_end..], false).ok_or(Error::InvalidParse)?;
        let offset = endings.get(1).map_or(source.len(), |end| content_end + end);
        let id = empty_paragraph(nodes, offset)?;
        nodes[0].children.push(id);
    }
    for parent in (0..nodes.len()).rev() {
        if matches!(nodes[parent].kind, NodeKind::Document | NodeKind::Quote) {
            preserve_container_paragraphs(source, nodes, parent)?;
        }
    }
    Ok(())
}

fn preserve_container_paragraphs(
    source: &str,
    nodes: &mut Vec<Node>,
    parent: NodeId,
) -> Result<(), Error> {
    let quoted = nodes[parent].kind == NodeKind::Quote;
    let extent = nodes[parent].source.clone();
    if nodes[parent].children.is_empty() && quoted {
        let offset = extent.start + after_prefix(&source[extent.clone()], 0, true);
        let id = empty_paragraph(nodes, offset)?;
        nodes[parent].children.push(id);
    }
    let original = nodes[parent].children.clone();
    let mut children = Vec::new();
    let mut previous_end = extent.start;
    for (index, id) in original.iter().copied().enumerate() {
        let end = nodes[id].source.start;
        if previous_end <= end {
            let gap = &source[previous_end..end];
            if let Some(endings) = line_end_positions(gap, quoted) {
                let count = endings.len() / 2;
                if index == 0 {
                    for paragraph in 0..count {
                        let offset = if paragraph == 0 {
                            0
                        } else {
                            endings[paragraph * 2 - 1]
                        };
                        children.push(empty_paragraph(
                            nodes,
                            previous_end + after_prefix(gap, offset, quoted),
                        )?);
                    }
                } else {
                    for paragraph in 1..count {
                        children.push(empty_paragraph(
                            nodes,
                            previous_end + after_prefix(gap, endings[paragraph * 2 - 1], quoted),
                        )?);
                    }
                }
            }
        }
        children.push(id);
        let raw = &source[nodes[id].source.clone()];
        previous_end = nodes[id].source.start + raw.trim_end_matches(['\r', '\n']).len();
    }
    let trailing = &source[previous_end..extent.end];
    if let Some(endings) = line_end_positions(trailing, quoted) {
        for pair in endings.chunks_exact(2) {
            children.push(empty_paragraph(
                nodes,
                previous_end + after_prefix(trailing, pair[1], quoted),
            )?);
        }
    }
    nodes[parent].children = children;
    Ok(())
}

fn empty_paragraph(nodes: &mut Vec<Node>, offset: usize) -> Result<NodeId, Error> {
    if nodes.len() >= MAX_NODES {
        return Err(Error::Limit);
    }
    let id = nodes.len();
    nodes.push(Node {
        kind: NodeKind::Paragraph,
        source: offset..offset,
        children: vec![],
    });
    Ok(id)
}

fn after_prefix(text: &str, offset: usize, quoted: bool) -> usize {
    if quoted {
        offset
            + text[offset..]
                .bytes()
                .take_while(|b| matches!(b, b'>' | b' ' | b'\t'))
                .count()
    } else {
        offset
    }
}

fn line_end_positions(text: &str, quoted: bool) -> Option<Vec<usize>> {
    let mut positions = Vec::new();
    let bytes = text.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        match bytes[offset] {
            b'\r' | b'\n' => {
                offset += if bytes[offset..].starts_with(b"\r\n") {
                    2
                } else {
                    1
                };
                positions.push(offset);
            }
            b'>' | b' ' | b'\t' if quoted => offset += 1,
            _ => return None,
        }
    }
    Some(positions)
}

/// Sparse offset map: each tab expands to four bytes in the parser input. The
/// mapping remains linear in tab count instead of allocating a map per byte.
struct ParserInput<'a> {
    text: Cow<'a, str>,
    tabs: Vec<usize>,
}
impl<'a> ParserInput<'a> {
    fn new(source: &'a str, dialect: Dialect) -> Self {
        let lone_cr = source
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'\r' && source.as_bytes().get(i + 1) != Some(&b'\n'));
        if dialect == Dialect::CommonMark || !source.contains('\t') && !lone_cr {
            return Self {
                text: Cow::Borrowed(source),
                tabs: vec![],
            };
        }
        let mut text = String::with_capacity(source.len());
        let mut tabs = Vec::new();
        let mut previous = 0;
        for (offset, value) in source.match_indices(['\t', '\r']) {
            if value == "\r" && source.as_bytes().get(offset + 1) == Some(&b'\n') {
                continue;
            }
            text.push_str(&source[previous..offset]);
            if value == "\t" {
                tabs.push(text.len());
                text.push_str("&#9;");
            } else {
                // A lone CR can make the parser coalesce code-text offsets
                // across a container prefix. Normalize only its parser-facing
                // byte; literal code spans still read the original CR.
                text.push('\n');
            }
            previous = offset + 1;
        }
        text.push_str(&source[previous..]);
        Self {
            text: Cow::Owned(text),
            tabs,
        }
    }

    fn source_offset(&self, position: usize) -> usize {
        let before = self.tabs.partition_point(|start| *start + 4 <= position);
        if self.tabs.get(before).is_some_and(|start| *start < position) {
            // Parser offsets into a generated entity name refer to the tab's
            // source start. Its end is handled by the completed-entry count.
            return self.tabs[before] - before * 3;
        }
        position - before * 3
    }
}
