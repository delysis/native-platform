//! Join the two open edges of a cross-block replacement. Compatible containers
//! join at the boundary; unmatched right-hand containers retain their children.
use super::{
    Fragment, FragmentKind, Kind, LeafData, Tree, container, forest, fragments, selection_style,
    serialize_replacement, slice_fragments, split_contained,
};
use crate::{BlockStyle, EditorDocument, Error, InlineStyle, Selection, VisualBlock};

impl EditorDocument {
    pub(in crate::editing) fn replace_across_structure(
        &mut self,
        before: Selection,
        selection: Selection,
        text: &str,
        first: usize,
        last: usize,
    ) -> Result<bool, Error> {
        let replacement = self.cross_block_content(selection, text, first, last)?;
        let original = forest(self.markdown(), self.projection(), 0)?;
        let start = original
            .iter()
            .position(|t| t.leaves.contains(&first))
            .ok_or(Error::InvalidParse)?;
        let end = original
            .iter()
            .position(|t| t.leaves.contains(&last))
            .ok_or(Error::InvalidParse)?;
        let left = split_contained(original[start].clone(), first, &replacement, false)?;
        let left = cut_forest(left, first, true)?;
        let right = cut_forest(vec![original[end].clone()], last, false)?;
        let joined = join_edges(left, right)?;
        let source = original[start].source.start..original[end].source.end;
        let mut transaction = self.transaction();
        transaction.replace(
            source.clone(),
            serialize_replacement(self, &joined, source)?,
        )?;
        let mut changed = original[..start].to_vec();
        changed.extend(joined);
        changed.extend_from_slice(&original[end + 1..]);
        let mut expected = self.projection().text().to_owned();
        let range = selection.range();
        expected.replace_range(range.clone(), text);
        self.commit_structure(
            transaction,
            before,
            Selection::caret(range.start + text.len()),
            &changed,
            &expected,
        )
    }

    fn cross_block_content(
        &self,
        selection: Selection,
        text: &str,
        first: usize,
        last: usize,
    ) -> Result<Vec<Tree>, Error> {
        let start = &self.projection().blocks()[first];
        let end = &self.projection().blocks()[last];
        if !matches!(
            start.style,
            BlockStyle::Body | BlockStyle::Heading(_) | BlockStyle::Code(_)
        ) || !matches!(
            end.style,
            BlockStyle::Body | BlockStyle::Heading(_) | BlockStyle::Code(_)
        ) {
            return Err(Error::UnsupportedEdit);
        }
        let range = selection.range();
        let leading = slice_fragments(
            &fragments(self.markdown(), self.projection(), start)?,
            0..range.start - start.display.start,
        );
        let mut trailing = slice_fragments(
            &fragments(self.markdown(), self.projection(), end)?,
            range.end - end.display.start..end.display.len(),
        );
        if matches!(start.style, BlockStyle::Code(_)) {
            return code_content(start, first, &leading, text, &trailing);
        }
        if matches!(end.style, BlockStyle::Code(_)) {
            for fragment in &mut trailing {
                fragment.style = InlineStyle::default();
                fragment.kind = FragmentKind::Text;
            }
        }
        let parts: Vec<_> = text.split('\n').collect();
        if parts.len() > crate::MAX_NODES / 2 {
            return Err(Error::Limit);
        }
        let mut typing = self
            .typing_style
            .clone()
            .unwrap_or_else(|| selection_style(self.projection(), selection));
        typing.code = false;
        let mut result = Vec::with_capacity(parts.len());
        for (i, text) in parts.iter().enumerate() {
            let mut fragments = if i == 0 { leading.clone() } else { vec![] };
            if !text.is_empty() {
                fragments.push(Fragment {
                    text: (*text).into(),
                    style: typing.clone(),
                    kind: FragmentKind::Text,
                });
            }
            if i + 1 == parts.len() {
                fragments.extend(trailing.clone());
            }
            let mut block = start.clone();
            if i > 0 || fragments.is_empty() {
                block.style = BlockStyle::Body;
            }
            result.push(Tree {
                source: start.source.clone(),
                leaves: first..first + 1,
                kind: Kind::Leaf(Box::new(LeafData { block, fragments })),
            });
        }
        Ok(result)
    }
}

fn code_content(
    block: &VisualBlock,
    first: usize,
    leading: &[Fragment],
    inserted: &str,
    trailing: &[Fragment],
) -> Result<Vec<Tree>, Error> {
    if trailing
        .iter()
        .any(|f| matches!(f.kind, FragmentKind::Raw(_)) && f.text == "\u{fffc}")
    {
        return Err(Error::UnsupportedEdit);
    }
    let mut text: String = leading.iter().map(|f| f.text.as_str()).collect();
    text.push_str(inserted);
    text.extend(trailing.iter().map(|f| f.text.as_str()));
    Ok(vec![Tree {
        source: block.source.clone(),
        leaves: first..first + 1,
        kind: Kind::Leaf(Box::new(LeafData {
            block: block.clone(),
            fragments: vec![Fragment {
                text,
                style: InlineStyle {
                    code: true,
                    ..InlineStyle::default()
                },
                kind: FragmentKind::Text,
            }],
        })),
    }])
}

fn cut_forest(trees: Vec<Tree>, at: usize, before: bool) -> Result<Vec<Tree>, Error> {
    trees
        .into_iter()
        .filter_map(|tree| cut(tree, at, before).transpose())
        .collect()
}

fn cut(mut tree: Tree, at: usize, before: bool) -> Result<Option<Tree>, Error> {
    if if before {
        tree.leaves.start > at
    } else {
        tree.leaves.end <= at
    } {
        return Ok(None);
    }
    if if before {
        tree.leaves.end <= at + 1
    } else {
        tree.leaves.start >= at
    } {
        return Ok(Some(tree));
    }
    let kind = match tree.kind {
        Kind::Leaf(_) => return Err(Error::InvalidParse),
        Kind::Quote(children) => Kind::Quote(cut_forest(children, at, before)?),
        Kind::List(order, items) => Kind::List(
            order,
            items
                .into_iter()
                .map(|item| cut_forest(item, at, before))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|item| !item.is_empty())
                .collect(),
        ),
    };
    let retained = container(kind)?;
    tree.kind = retained.kind;
    tree.leaves = retained.leaves;
    Ok(Some(tree))
}

fn join_edges(mut left: Vec<Tree>, mut right: Vec<Tree>) -> Result<Vec<Tree>, Error> {
    let Some(left_edge) = left.pop() else {
        return Ok(right);
    };
    if right.is_empty() {
        left.push(left_edge);
        return Ok(left);
    }
    let right_edge = right.remove(0);
    let source = left_edge.source.start..right_edge.source.end;
    let merged = match (&left_edge.kind, &right_edge.kind) {
        (Kind::Leaf(_), Kind::Leaf(_)) => vec![left_edge],
        (Kind::Quote(_), Kind::Quote(_)) => {
            let (Kind::Quote(a), Kind::Quote(b)) = (left_edge.kind, right_edge.kind) else {
                unreachable!()
            };
            vec![with_source(Kind::Quote(join_edges(a, b)?), source)?]
        }
        (Kind::List(a, _), Kind::List(b, _)) if a.is_some() == b.is_some() => {
            let (Kind::List(order, mut a), Kind::List(_, mut b)) =
                (left_edge.kind, right_edge.kind)
            else {
                unreachable!()
            };
            let end = a.pop().ok_or(Error::InvalidParse)?;
            let start = b.remove(0);
            a.push(join_edges(end, start)?);
            a.extend(b);
            vec![with_source(Kind::List(order, a), source)?]
        }
        _ => std::iter::once(left_edge)
            .chain(remove_first(right_edge)?)
            .collect(),
    };
    left.extend(merged);
    left.extend(right);
    Ok(left)
}

fn with_source(kind: Kind, source: std::ops::Range<usize>) -> Result<Tree, Error> {
    let mut tree = container(kind)?;
    tree.source = source;
    Ok(tree)
}

fn remove_first(mut tree: Tree) -> Result<Option<Tree>, Error> {
    let children = match &mut tree.kind {
        Kind::Leaf(_) => return Ok(None),
        Kind::Quote(children) => children,
        Kind::List(_, items) => items.first_mut().ok_or(Error::InvalidParse)?,
    };
    let first = children.remove(0);
    if let Some(retained) = remove_first(first)? {
        children.insert(0, retained);
    }
    if children.is_empty() {
        match &mut tree.kind {
            Kind::List(_, items) if items.len() > 1 => {
                items.remove(0);
            }
            _ => return Ok(None),
        }
    }
    with_source(tree.kind, tree.source).map(Some)
}
