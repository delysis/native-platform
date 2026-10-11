//! Structural commands operate on a bounded semantic tree. They replace only
//! affected root source ranges, then verify text, marks and container geometry.
use super::{
    Fragment, FragmentKind, checked_range, fragments, normalize_fragments, preferred_newline,
    same_fragments, selection_style, serialize_leaf, slice_fragments,
};
use crate::NodeId;
use crate::transaction::EditorSelection;
use crate::{
    Bias, BlockStyle, EditorDocument, Error, Markdown, NodeKind, Projection, Selection,
    Transaction, VisualBlock,
};
use std::ops::Range;
mod boundary;
mod input;
mod lists;
mod markers;
mod partition;
mod references;
mod replacement;
pub use boundary::DeleteDirection;
pub use lists::ListIndent;
pub(super) use references::retained as retained_references;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureFormat {
    Quote,
    BulletList,
    OrderedList,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Leaf(Box<LeafData>),
    Quote(Vec<Tree>),
    List(Option<u64>, Vec<Vec<Tree>>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct LeafData {
    block: VisualBlock,
    fragments: Vec<Fragment>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tree {
    source: Range<usize>,
    leaves: Range<usize>,
    kind: Kind,
}

impl EditorDocument {
    pub fn format_structure(
        &mut self,
        selection: Selection,
        format: StructureFormat,
    ) -> Result<bool, Error> {
        if self.markdown().dialect() == crate::Dialect::PlainText {
            return Err(Error::UnsupportedEdit);
        }
        let range = checked_range(self.projection().text(), selection)?;
        let selected: Vec<_> = self
            .projection()
            .blocks()
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                if range.is_empty() {
                    b.display.start <= range.start && range.start <= b.display.end
                } else {
                    b.display.start < range.end && range.start < b.display.end
                        || b.display.is_empty() && range.contains(&b.display.start)
                }
            })
            .map(|(i, _)| i)
            .collect();
        let (Some(&first), Some(&last)) = (selected.first(), selected.last()) else {
            return Ok(false);
        };
        let selected = first..last + 1;
        let original = forest(self.markdown(), self.projection(), 0)?;
        let active = original.iter().any(|tree| active(tree, &selected, format));
        let mut changed = Vec::new();
        let mut transaction = self.transaction();
        if active {
            for tree in &original {
                let replacement = unwrap(tree.clone(), &selected, format)?;
                if replacement.as_slice() != std::slice::from_ref(tree) {
                    transaction.replace(
                        tree.source.clone(),
                        serialize_replacement(self, &replacement, tree.source.clone())?,
                    )?;
                }
                changed.extend(replacement);
            }
        } else {
            let start = original
                .iter()
                .position(|t| overlaps(&t.leaves, &selected))
                .ok_or(Error::UnsupportedEdit)?;
            let end = original
                .iter()
                .rposition(|t| overlaps(&t.leaves, &selected))
                .ok_or(Error::UnsupportedEdit)?
                + 1;
            let source = original[start].source.start..original[end - 1].source.end;
            let replacement = wrap(original[start..end].to_vec(), &selected, format)?;
            transaction.replace(
                source.clone(),
                serialize_replacement(self, &replacement, source)?,
            )?;
            changed.extend_from_slice(&original[..start]);
            changed.extend(replacement);
            changed.extend_from_slice(&original[end..]);
        }
        let text = self.projection().text().to_owned();
        self.commit_structure(transaction, selection, selection, &changed, &text)
    }

    pub(super) fn replace_in_structure(
        &mut self,
        before: Selection,
        selection: Selection,
        inserted: &str,
    ) -> Result<bool, Error> {
        let range = checked_range(self.projection().text(), selection)?;
        let (index, block) = self
            .projection()
            .blocks()
            .iter()
            .enumerate()
            .find(|(_, b)| b.display.start <= range.start && range.end <= b.display.end)
            .ok_or(Error::UnsupportedEdit)?;
        if !matches!(block.style, BlockStyle::Body | BlockStyle::Heading(_)) {
            return Err(Error::UnsupportedEdit);
        }
        let text = inserted.replace("\r\n", "\n").replace('\r', "\n");
        let exiting = text == "\n"
            && range.is_empty()
            && block.display.is_empty()
            && block.style == BlockStyle::Body;
        let original_fragments = fragments(self.markdown(), self.projection(), block)?;
        let leading = slice_fragments(&original_fragments, 0..range.start - block.display.start);
        let trailing = slice_fragments(
            &original_fragments,
            range.end - block.display.start..block.display.len(),
        );
        let parts: Vec<_> = text.split('\n').collect();
        if parts.len() > crate::MAX_NODES / 2 {
            return Err(Error::Limit);
        }
        let typing = self
            .typing_style
            .clone()
            .unwrap_or_else(|| selection_style(self.projection(), selection));
        let mut replacement = Vec::new();
        for (part, value) in parts.iter().enumerate() {
            let mut fragments = if part == 0 { leading.clone() } else { vec![] };
            if !value.is_empty() {
                fragments.push(Fragment {
                    text: (*value).into(),
                    style: typing.clone(),
                    kind: if typing.code {
                        FragmentKind::Code
                    } else {
                        FragmentKind::Text
                    },
                });
            }
            if part + 1 == parts.len() {
                fragments.extend(trailing.clone());
            }
            let mut leaf_block = block.clone();
            if part > 0 || fragments.is_empty() {
                leaf_block.style = BlockStyle::Body;
            }
            replacement.push(Tree {
                source: block.source.clone(),
                leaves: index..index + 1,
                kind: Kind::Leaf(Box::new(LeafData {
                    block: leaf_block,
                    fragments,
                })),
            });
        }
        let mut changed = forest(self.markdown(), self.projection(), 0)?;
        let root = changed
            .iter()
            .position(|tree| tree.leaves.contains(&index))
            .ok_or(Error::InvalidParse)?;
        let source = changed[root].source.clone();
        let replacements = split_contained(changed[root].clone(), index, &replacement, exiting)?;
        let mut transaction = self.transaction();
        transaction.replace(
            source.clone(),
            serialize_replacement(self, &replacements, source)?,
        )?;
        changed.splice(root..=root, replacements);
        let mut expected = self.projection().text().to_owned();
        let after = if exiting {
            selection
        } else {
            expected.replace_range(range.clone(), &text);
            Selection::caret(range.start + text.len())
        };
        self.commit_structure(transaction, before, after, &changed, &expected)
    }

    fn commit_structure(
        &mut self,
        mut transaction: Transaction,
        before: Selection,
        after: Selection,
        changed: &[Tree],
        text: &str,
    ) -> Result<bool, Error> {
        let mut expected = Vec::new();
        geometry(changed, 0, 0, None, &mut expected);
        transaction.before_visual(before);
        transaction.set_selection(Selection::caret(0));
        self.apply_checked(transaction, |markdown, projection| {
            if projection.text() != text || projection.blocks().len() != expected.len() {
                return Err(Error::SerializationMismatch);
            }
            for (block, expected) in projection.blocks().iter().zip(&expected) {
                if block.quote_depth != expected.quote
                    || block.list_depth != expected.list
                    || block.marker != expected.marker
                    || block.style != expected.leaf.block.style
                    || !same_fragments(
                        &normalize_fragments(&expected.leaf.fragments),
                        &fragments(markdown, projection, block)?,
                    )
                {
                    return Err(Error::SerializationMismatch);
                }
            }
            Ok(Some(EditorSelection::Visual(after, Bias::After)))
        })
    }
}

fn split_contained(
    mut tree: Tree,
    index: usize,
    replacement: &[Tree],
    exiting: bool,
) -> Result<Vec<Tree>, Error> {
    match &mut tree.kind {
        Kind::Leaf(_) => return Ok(replacement.to_vec()),
        Kind::Quote(children) => {
            let child = children
                .iter()
                .position(|t| t.leaves.contains(&index))
                .ok_or(Error::InvalidParse)?;
            if exiting && matches!(children[child].kind, Kind::Leaf(_)) {
                return unwrap(tree, &(index..index + 1), StructureFormat::Quote);
            }
            let changed = split_contained(children[child].clone(), index, replacement, exiting)?;
            children.splice(child..=child, changed);
        }
        Kind::List(order, items) => {
            let item = items
                .iter()
                .position(|item| item.iter().any(|t| t.leaves.contains(&index)))
                .ok_or(Error::InvalidParse)?;
            let child = items[item]
                .iter()
                .position(|t| t.leaves.contains(&index))
                .ok_or(Error::InvalidParse)?;
            if matches!(items[item][child].kind, Kind::Leaf(_)) {
                if exiting {
                    let format = if order.is_some() {
                        StructureFormat::OrderedList
                    } else {
                        StructureFormat::BulletList
                    };
                    return unwrap(tree, &(index..index + 1), format);
                }
                let tail = items[item].split_off(child + 1);
                items[item].truncate(child);
                let mut split = replacement.iter().cloned();
                items[item].push(split.next().ok_or(Error::InvalidParse)?);
                let mut following: Vec<Vec<Tree>> = split.map(|t| vec![t]).collect();
                if let Some(last) = following.last_mut() {
                    last.extend(tail);
                } else {
                    items[item].extend(tail);
                }
                let insertion = item + 1;
                items.splice(insertion..insertion, following);
            } else {
                let nested_list = matches!(items[item][child].kind, Kind::List(_, _));
                let changed =
                    split_contained(items[item][child].clone(), index, replacement, exiting)?;
                if exiting
                    && nested_list
                    && let Some(lifted) =
                        changed.iter().position(|t| matches!(t.kind, Kind::Leaf(_)))
                {
                    let tail = items[item].split_off(child + 1);
                    items[item].truncate(child);
                    items[item].extend_from_slice(&changed[..lifted]);
                    let mut next = changed[lifted..].to_vec();
                    next.extend(tail);
                    items.insert(item + 1, next);
                } else {
                    items[item].splice(child..=child, changed);
                }
            }
        }
    }
    Ok(vec![tree])
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}
fn matches(kind: &Kind, format: StructureFormat) -> bool {
    matches!(
        (kind, format),
        (Kind::Quote(_), StructureFormat::Quote)
            | (Kind::List(None, _), StructureFormat::BulletList)
            | (Kind::List(Some(_), _), StructureFormat::OrderedList)
    )
}
fn active(tree: &Tree, selected: &Range<usize>, format: StructureFormat) -> bool {
    overlaps(&tree.leaves, selected)
        && (matches(&tree.kind, format)
            || match &tree.kind {
                Kind::Leaf(_) => false,
                Kind::Quote(children) => children.iter().any(|t| active(t, selected, format)),
                Kind::List(_, items) => items.iter().flatten().any(|t| active(t, selected, format)),
            })
}
fn container(kind: Kind) -> Result<Tree, Error> {
    let (first, last) = match &kind {
        Kind::Quote(children) => (children.first(), children.last()),
        Kind::List(_, items) => (
            items.first().and_then(|i| i.first()),
            items.last().and_then(|i| i.last()),
        ),
        Kind::Leaf(_) => return Err(Error::InvalidParse),
    };
    let (Some(first), Some(last)) = (first, last) else {
        return Err(Error::UnsupportedEdit);
    };
    Ok(Tree {
        source: first.source.start..last.source.end,
        leaves: first.leaves.start..last.leaves.end,
        kind,
    })
}

fn forest(markdown: &Markdown, projection: &Projection, id: NodeId) -> Result<Vec<Tree>, Error> {
    // Tight list items can own more than one text block, with descendant blocks
    // between them. Index their exact ownership once, not once per AST node.
    let mut blocks_by_node = vec![Vec::new(); markdown.nodes().len()];
    for (index, block) in projection.blocks().iter().enumerate() {
        blocks_by_node
            .get_mut(block.node)
            .ok_or(Error::InvalidParse)?
            .push(index);
    }
    forest_indexed(markdown, projection, id, &blocks_by_node)
}

fn forest_indexed(
    markdown: &Markdown,
    projection: &Projection,
    id: NodeId,
    blocks_by_node: &[Vec<usize>],
) -> Result<Vec<Tree>, Error> {
    let node = markdown.node(id).ok_or(Error::InvalidParse)?;
    let result = match &node.kind {
        NodeKind::Document => node
            .children
            .iter()
            .map(|&i| forest_indexed(markdown, projection, i, blocks_by_node))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect(),
        NodeKind::Quote => vec![container(Kind::Quote(
            node.children
                .iter()
                .map(|&i| forest_indexed(markdown, projection, i, blocks_by_node))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect(),
        ))?],
        NodeKind::List(order) => vec![container(Kind::List(
            *order,
            node.children
                .iter()
                .map(|&i| forest_indexed(markdown, projection, i, blocks_by_node))
                .collect::<Result<Vec<_>, _>>()?,
        ))?],
        NodeKind::Item => {
            let mut result = Vec::new();
            let mut own = blocks_by_node[id]
                .iter()
                .map(|&index| (index, &projection.blocks()[index]));
            let mut in_inline = false;
            for &child in &node.children {
                let child_node = &markdown.nodes()[child];
                let inline = matches!(
                    child_node.kind,
                    NodeKind::Text(_)
                        | NodeKind::Code(_)
                        | NodeKind::Strong
                        | NodeKind::Emphasis
                        | NodeKind::Link { .. }
                        | NodeKind::Image { .. }
                        | NodeKind::Html(_)
                        | NodeKind::SoftBreak
                        | NodeKind::HardBreak
                );
                if inline {
                    if !in_inline {
                        let (index, block) = own.next().ok_or(Error::InvalidParse)?;
                        result.push(leaf(index, block, markdown, projection)?);
                    }
                } else {
                    result.extend(forest_indexed(markdown, projection, child, blocks_by_node)?);
                }
                in_inline = inline;
            }
            if result.is_empty() {
                let (index, block) = own.next().ok_or(Error::InvalidParse)?;
                result.push(leaf(index, block, markdown, projection)?);
            }
            result
        }
        _ => {
            let &index = blocks_by_node[id].first().ok_or(Error::UnsupportedEdit)?;
            vec![leaf(
                index,
                &projection.blocks()[index],
                markdown,
                projection,
            )?]
        }
    };
    Ok(result
        .into_iter()
        .map(|mut tree| {
            if matches!(node.kind, NodeKind::Quote | NodeKind::List(_)) {
                tree.source = node.source.clone();
            }
            tree
        })
        .collect())
}
fn leaf(
    index: usize,
    block: &VisualBlock,
    markdown: &Markdown,
    projection: &Projection,
) -> Result<Tree, Error> {
    let mut source = block.source.clone();
    if matches!(
        markdown.nodes()[block.node].kind,
        NodeKind::CodeBlock { fenced: false, .. }
    ) && let Some(start) = source.start.checked_sub(4)
        && markdown.source().get(start..source.start) == Some("    ")
    {
        source.start = start;
    }
    Ok(Tree {
        source,
        leaves: index..index + 1,
        kind: Kind::Leaf(Box::new(LeafData {
            block: block.clone(),
            fragments: fragments(markdown, projection, block)?,
        })),
    })
}

fn wrap(
    mut trees: Vec<Tree>,
    selected: &Range<usize>,
    format: StructureFormat,
) -> Result<Vec<Tree>, Error> {
    if trees.len() == 1 {
        let tree = &mut trees[0];
        match &mut tree.kind {
            Kind::Quote(children) => {
                *children = wrap(std::mem::take(children), selected, format)?;
                return Ok(trees);
            }
            Kind::List(order, items) => {
                if selected.start <= tree.leaves.start
                    && tree.leaves.end <= selected.end
                    && format != StructureFormat::Quote
                {
                    *order = (format == StructureFormat::OrderedList).then_some(1);
                    return Ok(trees);
                }
                if let Some(item) = items.iter_mut().find(|item| {
                    item.first()
                        .is_some_and(|t| t.leaves.start <= selected.start)
                        && item.last().is_some_and(|t| selected.end <= t.leaves.end)
                }) {
                    *item = wrap(std::mem::take(item), selected, format)?;
                    return Ok(trees);
                }
            }
            Kind::Leaf(_) => {}
        }
    }
    let start = trees
        .iter()
        .position(|t| overlaps(&t.leaves, selected))
        .ok_or(Error::UnsupportedEdit)?;
    let end = trees
        .iter()
        .rposition(|t| overlaps(&t.leaves, selected))
        .ok_or(Error::UnsupportedEdit)?
        + 1;
    let children: Vec<_> = trees.drain(start..end).collect();
    let kind = match format {
        StructureFormat::Quote => Kind::Quote(children),
        StructureFormat::BulletList | StructureFormat::OrderedList => Kind::List(
            (format == StructureFormat::OrderedList).then_some(1),
            children.into_iter().map(|t| vec![t]).collect(),
        ),
    };
    trees.insert(start, container(kind)?);
    Ok(trees)
}

fn unwrap(
    mut tree: Tree,
    selected: &Range<usize>,
    format: StructureFormat,
) -> Result<Vec<Tree>, Error> {
    if !overlaps(&tree.leaves, selected) {
        return Ok(vec![tree]);
    }
    // A selection contained in a deeper matching wrapper targets that level.
    // Lifting its ancestor would also lift unselected siblings or a parent item.
    if !(selected.start <= tree.leaves.start && tree.leaves.end <= selected.end)
        && unwrap_contained(&mut tree, selected, format)?
    {
        return Ok(vec![tree]);
    }
    if matches(&tree.kind, format) {
        if matches!(&tree.kind, Kind::Quote(_)) {
            return partition::lift_quote(tree, selected);
        }
        let (groups, order) = match tree.kind {
            Kind::List(order, items) => (items, order),
            Kind::Quote(_) | Kind::Leaf(_) => return Err(Error::InvalidParse),
        };
        let mut result = Vec::new();
        let mut retained = Vec::new();
        let mut run_start = 0;
        for (index, group) in groups.into_iter().enumerate() {
            if group.iter().any(|t| overlaps(&t.leaves, selected)) {
                flush_retained(&mut result, &mut retained, order, run_start)?;
                result.extend(group);
            } else {
                if retained.is_empty() {
                    run_start = index;
                }
                retained.push(group);
            }
        }
        flush_retained(&mut result, &mut retained, order, run_start)?;
        return Ok(result);
    }
    match &mut tree.kind {
        Kind::Leaf(_) => {}
        Kind::Quote(children) => {
            *children = std::mem::take(children)
                .into_iter()
                .map(|t| unwrap(t, selected, format))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
        }
        Kind::List(_, items) => {
            for item in items {
                *item = std::mem::take(item)
                    .into_iter()
                    .map(|t| unwrap(t, selected, format))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect();
            }
        }
    }
    Ok(vec![tree])
}

fn unwrap_contained(
    tree: &mut Tree,
    selected: &Range<usize>,
    format: StructureFormat,
) -> Result<bool, Error> {
    let lists: &mut [Vec<Tree>] = match &mut tree.kind {
        Kind::Leaf(_) => return Ok(false),
        Kind::Quote(children) => std::slice::from_mut(children),
        Kind::List(_, items) => items,
    };
    for children in lists {
        if let Some(index) = children.iter().position(|child| {
            child.leaves.start <= selected.start
                && selected.end <= child.leaves.end
                && common_wrapper(child, selected, format)
        }) {
            let replacement = unwrap(children[index].clone(), selected, format)?;
            children.splice(index..=index, replacement);
            return Ok(true);
        }
    }
    Ok(false)
}

fn common_wrapper(tree: &Tree, selected: &Range<usize>, format: StructureFormat) -> bool {
    if matches(&tree.kind, format) {
        return true;
    }
    let contains = |child: &Tree| {
        child.leaves.start <= selected.start
            && selected.end <= child.leaves.end
            && common_wrapper(child, selected, format)
    };
    match &tree.kind {
        Kind::Leaf(_) => false,
        Kind::Quote(children) => children.iter().any(contains),
        Kind::List(_, items) => items.iter().flatten().any(contains),
    }
}
fn flush_retained(
    result: &mut Vec<Tree>,
    retained: &mut Vec<Vec<Tree>>,
    order: Option<u64>,
    start: usize,
) -> Result<(), Error> {
    if retained.is_empty() {
        return Ok(());
    }
    let groups = std::mem::take(retained);
    result.push(container(Kind::List(
        order.map(|n| n.saturating_add(start as u64)),
        groups,
    ))?);
    Ok(())
}

struct ExpectedBlock<'a> {
    leaf: &'a LeafData,
    quote: usize,
    list: usize,
    marker: Option<String>,
}

fn geometry<'a>(
    trees: &'a [Tree],
    quote: usize,
    list: usize,
    mut marker: Option<String>,
    out: &mut Vec<ExpectedBlock<'a>>,
) {
    for tree in trees {
        match &tree.kind {
            Kind::Leaf(leaf) => out.push(ExpectedBlock {
                leaf,
                quote,
                list,
                marker: marker.take(),
            }),
            Kind::Quote(children) => geometry(children, quote + 1, list, marker.take(), out),
            Kind::List(order, items) => {
                for (index, item) in items.iter().enumerate() {
                    let marker = order.map_or_else(
                        || "•".into(),
                        |start| format!("{}.", start.saturating_add(index as u64)),
                    );
                    geometry(item, quote, list + 1, Some(marker), out);
                }
            }
        }
        marker = None;
    }
}

fn serialize_replacement(
    doc: &EditorDocument,
    trees: &[Tree],
    range: Range<usize>,
) -> Result<String, Error> {
    let retained = references::retained(doc, range.clone())?;
    let original = &doc.source()[range];
    let markers = markers::Markers::new(doc.markdown());
    let newline = preferred_newline(original);
    let mut text = serialize_forest(doc, trees, false, &markers, newline)?;
    text.push_str(&original[original.trim_end_matches(['\r', '\n']).len()..]);
    text.insert_str(0, &retained);
    if text.len() > crate::MAX_SOURCE_BYTES {
        return Err(Error::Limit);
    }
    Ok(text)
}
fn serialize_forest(
    doc: &EditorDocument,
    trees: &[Tree],
    inside_item: bool,
    markers: &markers::Markers,
    newline: &str,
) -> Result<String, Error> {
    let mut out = String::new();
    let mut previous_marker = None;
    for (index, tree) in trees.iter().enumerate() {
        let delimiter = if let Kind::List(order, _) = &tree.kind {
            let ordered = order.is_some();
            let original = markers.at(tree.source.start, ordered);
            // Blank lines cannot separate same-kind CommonMark lists reliably.
            // Alternate native Markdown punctuation to retain distinct starts.
            let delimiter = if previous_marker == Some((ordered, original)) {
                match original {
                    '.' => ')',
                    ')' => '.',
                    '*' => '-',
                    _ => '*',
                }
            } else {
                original
            };
            previous_marker = Some((ordered, delimiter));
            delimiter
        } else {
            previous_marker = None;
            '*'
        };
        if index > 0 {
            let previous = &trees[index - 1];
            let count = if matches!(
                (&previous.kind, &tree.kind),
                (Kind::List(_, _), Kind::List(_, _))
            ) {
                3
            } else if inside_item && matches!(tree.kind, Kind::List(_, _)) {
                1
            } else {
                2
            };
            out.push_str(&newline.repeat(count));
        }
        out.push_str(&serialize_tree(doc, tree, markers, delimiter, newline)?);
        if out.len() > crate::MAX_SOURCE_BYTES {
            return Err(Error::Limit);
        }
    }
    Ok(out)
}
fn serialize_tree(
    doc: &EditorDocument,
    tree: &Tree,
    markers: &markers::Markers,
    delimiter: char,
    newline: &str,
) -> Result<String, Error> {
    match &tree.kind {
        Kind::Leaf(leaf) => {
            let block = &leaf.block;
            let text: String = leaf.fragments.iter().map(|f| f.text.as_str()).collect();
            match &block.style {
                BlockStyle::Body | BlockStyle::Heading(_) => {
                    let mut bare = block.clone();
                    bare.source = 0..0;
                    serialize_leaf(doc.markdown(), &bare, &leaf.fragments, &block.style)
                        .map(|text| text.replace('\n', newline))
                }
                BlockStyle::Rule => Ok("---".into()),
                BlockStyle::Raw => Ok(text.trim_end_matches(['\r', '\n']).into()),
                BlockStyle::Code(language) => super::code::render(language, &text, newline, '`'),
            }
        }
        Kind::Quote(children) => super::code::prefix_lines(
            &serialize_forest(doc, children, false, markers, newline)?,
            "> ",
            "> ",
            true,
        ),
        Kind::List(order, items) => {
            let mut out = String::new();
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(newline);
                }
                let marker = order.map_or_else(
                    || format!("{delimiter} "),
                    |n| format!("{}{delimiter} ", n.saturating_add(index as u64)),
                );
                let indent = " ".repeat(marker.len());
                out.push_str(&super::code::prefix_lines(
                    &serialize_forest(doc, item, true, markers, newline)?,
                    &marker,
                    &indent,
                    false,
                )?);
                if out.len() > crate::MAX_SOURCE_BYTES {
                    return Err(Error::Limit);
                }
            }
            Ok(out)
        }
    }
}
