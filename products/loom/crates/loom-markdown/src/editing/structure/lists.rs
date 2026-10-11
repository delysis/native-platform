use super::{Kind, Tree, checked_range, container, forest, overlaps, serialize_replacement};
use crate::{BlockStyle, EditorDocument, Error, Selection};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListIndent {
    Indent,
    Outdent,
}
#[derive(Clone, Copy, Debug)]
enum Step {
    Quote(usize),
    Item(usize, usize),
}

impl EditorDocument {
    /// Shift complete selected list items by one level. `false` lets the view
    /// fall back to ordinary Tab insertion when no list operation applies.
    pub fn indent_list(
        &mut self,
        selection: Selection,
        direction: ListIndent,
    ) -> Result<bool, Error> {
        let range = checked_range(self.projection().text(), selection)?;
        let blocks = self.projection().blocks();
        let first = blocks
            .iter()
            .position(|b| b.display.start <= range.start && range.start <= b.display.end)
            .ok_or(Error::UnsupportedEdit)?;
        let last = blocks
            .iter()
            .rposition(|b| {
                if range.is_empty() {
                    b.display.start <= range.end && range.end <= b.display.end
                } else {
                    b.display.start < range.end
                }
            })
            .ok_or(Error::UnsupportedEdit)?;
        if first == last && matches!(blocks[first].style, BlockStyle::Code(_)) {
            return Ok(false);
        }
        let selected = first..last + 1;
        let mut roots = forest(self.markdown(), self.projection(), 0)?;
        let Some(root) = roots.iter().position(|t| contains(&t.leaves, &selected)) else {
            return Ok(false);
        };
        let Some(path) = nearest_list(&roots[root], &selected) else {
            return Ok(false);
        };
        let original = roots[root].source.clone();
        let affected = match direction {
            ListIndent::Indent => {
                let target = descend(&mut roots[root], &path)?;
                let Kind::List(order, items) = &mut target.kind else {
                    return Err(Error::InvalidParse);
                };
                let selected_items = item_range(items, &selected)?;
                if selected_items.start == 0 {
                    return Ok(false);
                }
                let previous = selected_items.start - 1;
                let moved: Vec<_> = items.drain(selected_items).collect();
                if let Some(last) = items[previous].last_mut()
                    && let Kind::List(nested_order, nested_items) = &mut last.kind
                    && nested_order.is_some() == order.is_some()
                {
                    nested_items.extend(moved);
                } else {
                    items[previous].push(container(Kind::List(order.map(|_| 1), moved))?);
                }
                1
            }
            ListIndent::Outdent => lift(&mut roots, root, &path, &selected)?,
        };
        let mut transaction = self.transaction();
        transaction.replace(
            original.clone(),
            serialize_replacement(self, &roots[root..root + affected], original)?,
        )?;
        let text = self.projection().text().to_owned();
        self.commit_structure(transaction, selection, selection, &roots, &text)
    }
}

fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}
fn nearest_list(tree: &Tree, selected: &Range<usize>) -> Option<Vec<Step>> {
    if !contains(&tree.leaves, selected) {
        return None;
    }
    match &tree.kind {
        Kind::Leaf(_) => None,
        Kind::Quote(children) => children.iter().enumerate().find_map(|(index, tree)| {
            let mut path = nearest_list(tree, selected)?;
            path.insert(0, Step::Quote(index));
            Some(path)
        }),
        Kind::List(_, items) => {
            for (item, children) in items.iter().enumerate() {
                for (child, tree) in children.iter().enumerate() {
                    if let Some(mut path) = nearest_list(tree, selected) {
                        path.insert(0, Step::Item(item, child));
                        return Some(path);
                    }
                }
            }
            Some(vec![])
        }
    }
}
fn descend<'a>(mut tree: &'a mut Tree, path: &[Step]) -> Result<&'a mut Tree, Error> {
    for step in path {
        tree = match (step, &mut tree.kind) {
            (Step::Quote(index), Kind::Quote(children)) => children.get_mut(*index),
            (Step::Item(item, child), Kind::List(_, items)) => {
                items.get_mut(*item).and_then(|item| item.get_mut(*child))
            }
            _ => None,
        }
        .ok_or(Error::InvalidParse)?;
    }
    Ok(tree)
}
fn item_range(items: &[Vec<Tree>], selected: &Range<usize>) -> Result<Range<usize>, Error> {
    let first = items
        .iter()
        .position(|item| item.iter().any(|t| overlaps(&t.leaves, selected)))
        .ok_or(Error::InvalidParse)?;
    let last = items
        .iter()
        .rposition(|item| item.iter().any(|t| overlaps(&t.leaves, selected)))
        .ok_or(Error::InvalidParse)?;
    Ok(first..last + 1)
}

fn lift(
    roots: &mut Vec<Tree>,
    root: usize,
    path: &[Step],
    selected: &Range<usize>,
) -> Result<usize, Error> {
    let target = descend(&mut roots[root], path)?;
    let Kind::List(order, items) = &target.kind else {
        return Err(Error::InvalidParse);
    };
    let selected_items = item_range(items, selected)?;
    let before = if selected_items.start > 0 {
        Some(container(Kind::List(
            *order,
            items[..selected_items.start].to_vec(),
        ))?)
    } else {
        None
    };
    let after = if selected_items.end < items.len() {
        Some(container(Kind::List(
            order.map(|n| n.saturating_add(selected_items.end as u64)),
            items[selected_items.end..].to_vec(),
        ))?)
    } else {
        None
    };
    let mut moved = items[selected_items].to_vec();
    match path.split_last() {
        Some((Step::Item(item, child), parents)) => {
            let Kind::List(_, outer) = &mut descend(&mut roots[root], parents)?.kind else {
                return Err(Error::InvalidParse);
            };
            let tail = outer[*item].split_off(child + 1);
            outer[*item].truncate(*child);
            outer[*item].extend(before);
            let last = moved.last_mut().ok_or(Error::InvalidParse)?;
            last.extend(after);
            last.extend(tail);
            let insertion = if outer[*item].is_empty() {
                outer.remove(*item);
                *item
            } else {
                item + 1
            };
            outer.splice(insertion..insertion, moved);
            Ok(1)
        }
        Some((Step::Quote(child), parents)) => {
            let Kind::Quote(children) = &mut descend(&mut roots[root], parents)?.kind else {
                return Err(Error::InvalidParse);
            };
            let replacement = before
                .into_iter()
                .chain(moved.into_iter().flatten())
                .chain(after);
            children.splice(*child..=*child, replacement);
            Ok(1)
        }
        None => {
            let replacement: Vec<_> = before
                .into_iter()
                .chain(moved.into_iter().flatten())
                .chain(after)
                .collect();
            let count = replacement.len();
            roots.splice(root..=root, replacement);
            Ok(count)
        }
    }
}
