//! Split at semantic leaf boundaries without changing retained container depth.
use super::{Error, Kind, Tree, container};
use std::ops::Range;

pub(super) fn lift_quote(tree: Tree, selected: &Range<usize>) -> Result<Vec<Tree>, Error> {
    let (before, rest) = split_at(tree, selected.start)?;
    let (middle, after) = split_at(rest.ok_or(Error::InvalidParse)?, selected.end)?;
    let Kind::Quote(children) = middle.ok_or(Error::InvalidParse)?.kind else {
        return Err(Error::InvalidParse);
    };
    Ok(before.into_iter().chain(children).chain(after).collect())
}

fn split_at(tree: Tree, at: usize) -> Result<(Option<Tree>, Option<Tree>), Error> {
    if at <= tree.leaves.start {
        return Ok((None, Some(tree)));
    }
    if at >= tree.leaves.end {
        return Ok((Some(tree), None));
    }
    match tree.kind {
        Kind::Leaf(_) => Err(Error::InvalidParse),
        Kind::Quote(children) => {
            let (left, right) = split_forest(children, at)?;
            Ok((optional_quote(left)?, optional_quote(right)?))
        }
        Kind::List(order, items) => {
            let mut left = vec![];
            let mut right = vec![];
            let mut right_start = 0;
            for (index, item) in items.into_iter().enumerate() {
                let (before, after) = split_forest(item, at)?;
                if !before.is_empty() && !after.is_empty() {
                    // A list item is a semantic unit. Dividing it would invent
                    // another bullet/number for an unselected continuation.
                    return Err(Error::UnsupportedEdit);
                }
                if !before.is_empty() {
                    left.push(before);
                }
                if !after.is_empty() {
                    if right.is_empty() {
                        right_start = index;
                    }
                    right.push(after);
                }
            }
            // Splitting a quoted ordered list must keep each retained item's number.
            let right_order = order.map(|n| n.saturating_add(right_start as u64));
            Ok((
                optional_list(order, left)?,
                optional_list(right_order, right)?,
            ))
        }
    }
}

fn split_forest(trees: Vec<Tree>, at: usize) -> Result<(Vec<Tree>, Vec<Tree>), Error> {
    let mut left = vec![];
    let mut right = vec![];
    for tree in trees {
        let (before, after) = split_at(tree, at)?;
        left.extend(before);
        right.extend(after);
    }
    Ok((left, right))
}

fn optional_quote(children: Vec<Tree>) -> Result<Option<Tree>, Error> {
    if children.is_empty() {
        Ok(None)
    } else {
        container(Kind::Quote(children)).map(Some)
    }
}

fn optional_list(order: Option<u64>, items: Vec<Vec<Tree>>) -> Result<Option<Tree>, Error> {
    if items.is_empty() {
        Ok(None)
    } else {
        container(Kind::List(order, items)).map(Some)
    }
}
