//! Block input rules wrap one leaf and join a preceding compatible list.
use super::{
    EditorDocument, Error, Kind, LeafData, Selection, Tree, container, forest, fragments,
    serialize_replacement, slice_fragments,
};
use crate::{BlockStyle, editing::input_rules::BlockRule};
use std::ops::Range;

impl EditorDocument {
    pub(in crate::editing) fn apply_block_input_rule(
        &mut self,
        before: Selection,
        range: Range<usize>,
        rule: BlockRule,
    ) -> Result<bool, Error> {
        let (index, block) = self
            .projection()
            .blocks()
            .iter()
            .enumerate()
            .find(|(_, b)| b.display.start == range.start && range.end <= b.display.end)
            .ok_or(Error::UnsupportedEdit)?;
        let mut leaf = block.clone();
        if let BlockRule::Heading(level) = rule {
            leaf.style = BlockStyle::Heading(level);
        }
        let replacement = Tree {
            source: block.source.clone(),
            leaves: index..index + 1,
            kind: Kind::Leaf(Box::new(LeafData {
                block: leaf,
                fragments: slice_fragments(
                    &fragments(self.markdown(), self.projection(), block)?,
                    range.end - block.display.start..block.display.len(),
                ),
            })),
        };
        let replacement = match rule {
            BlockRule::Heading(_) => replacement,
            BlockRule::Quote => container(Kind::Quote(vec![replacement]))?,
            BlockRule::List(order) => container(Kind::List(order, vec![vec![replacement]]))?,
        };
        let original = forest(self.markdown(), self.projection(), 0)?;
        let mut changed = original.clone();
        replace_leaf(&mut changed, index, replacement)?;
        let mut first = original
            .iter()
            .zip(&changed)
            .take_while(|(a, b)| a == b)
            .count();
        let tail = original
            .iter()
            .rev()
            .zip(changed.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        if first == original.len() && first == changed.len() {
            return Ok(false);
        }
        if first > 0
            && matches!(changed[first - 1].kind, Kind::List(_, _))
            && matches!(changed[first].kind, Kind::List(_, _))
        {
            // An intentionally separate list needs the serializer to choose
            // distinct punctuation, or CommonMark may merge and renumber it.
            first -= 1;
        }
        let source = original[first].source.start..original[original.len() - tail - 1].source.end;
        let mut transaction = self.transaction();
        let mut replacement =
            serialize_replacement(self, &changed[first..changed.len() - tail], source.clone())?;
        if source.is_empty() && source.start == self.source().len() {
            replacement.insert_str(0, &crate::editing::empty_body_prefix(self.source()));
        }
        transaction.replace(source, replacement)?;
        let mut expected = self.projection().text().to_owned();
        expected.replace_range(range.clone(), "");
        self.commit_structure(
            transaction,
            before,
            Selection::caret(range.start),
            &changed,
            &expected,
        )
    }
}

fn replace_leaf(trees: &mut Vec<Tree>, index: usize, replacement: Tree) -> Result<(), Error> {
    let at = trees
        .iter()
        .position(|tree| tree.leaves.contains(&index))
        .ok_or(Error::InvalidParse)?;
    match &mut trees[at].kind {
        Kind::Leaf(_) => {
            trees[at] = replacement;
            join_previous(trees, at)?;
        }
        Kind::Quote(children) => replace_leaf(children, index, replacement)?,
        Kind::List(_, items) => {
            let item = items
                .iter_mut()
                .find(|item| item.iter().any(|t| t.leaves.contains(&index)))
                .ok_or(Error::InvalidParse)?;
            replace_leaf(item, index, replacement)?;
        }
    }
    Ok(())
}

fn join_previous(trees: &mut Vec<Tree>, at: usize) -> Result<(), Error> {
    let Some(previous) = at.checked_sub(1) else {
        return Ok(());
    };
    let (Kind::List(before, items), Kind::List(next, _)) = (&trees[previous].kind, &trees[at].kind)
    else {
        return Ok(());
    };
    let compatible = match (before, next) {
        (None, None) => true,
        (Some(order), Some(next)) => order.checked_add(items.len() as u64) == Some(*next),
        _ => false,
    };
    if compatible {
        let Kind::List(_, next_items) = trees.remove(at).kind else {
            return Err(Error::InvalidParse);
        };
        let Kind::List(order, items) = &mut trees[previous].kind else {
            return Err(Error::InvalidParse);
        };
        items.extend(next_items);
        trees[previous] = container(Kind::List(*order, std::mem::take(items)))?;
    }
    Ok(())
}
