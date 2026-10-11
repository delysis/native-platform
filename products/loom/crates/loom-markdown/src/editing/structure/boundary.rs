//! Keyboard deletion first removes a structural boundary, then a text boundary
//! on the next keypress. Explicit range replacement uses the open-edge join.
use super::{Kind, Tree, checked_range, forest, serialize_replacement};
use crate::{Dialect, EditorDocument, Error, Selection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteDirection {
    Backward,
    Forward,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    NoCut,
    Text,
    Changed,
}

impl EditorDocument {
    /// Handle Backspace/Delete at a block edge. `false` permits ordinary native
    /// grapheme deletion. Selection replacement is never treated as this command.
    pub fn delete_boundary(
        &mut self,
        selection: Selection,
        direction: DeleteDirection,
    ) -> Result<bool, Error> {
        let range = checked_range(self.projection().text(), selection)?;
        if !range.is_empty() || self.markdown().dialect() == Dialect::PlainText {
            return Ok(false);
        }
        let backwards = direction == DeleteDirection::Backward;
        let Some(index) = self.projection().blocks().iter().position(|b| {
            if backwards {
                b.display.start == range.start
            } else {
                b.display.end == range.start
            }
        }) else {
            return Ok(false);
        };
        let original = forest(self.markdown(), self.projection(), 0)?;
        let mut changed = original.clone();
        let outcome = at_cut(&mut changed, index, backwards)?;
        if outcome == Outcome::NoCut && backwards {
            let root = changed
                .iter()
                .position(|t| t.leaves.contains(&index))
                .ok_or(Error::InvalidParse)?;
            let Some(replacement) = lift_first(changed[root].clone())? else {
                return Ok(false);
            };
            changed.splice(root..=root, replacement);
        } else if outcome != Outcome::Changed {
            return Ok(false);
        }
        let first = original
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
        let source = original[first].source.start..original[original.len() - tail - 1].source.end;
        let mut transaction = self.transaction();
        transaction.replace(
            source.clone(),
            serialize_replacement(self, &changed[first..changed.len() - tail], source)?,
        )?;
        let text = self.projection().text().to_owned();
        self.commit_structure(transaction, selection, selection, &changed, &text)
    }
}

fn at_cut(trees: &mut Vec<Tree>, index: usize, backwards: bool) -> Result<Outcome, Error> {
    let at = trees
        .iter()
        .position(|t| t.leaves.contains(&index))
        .ok_or(Error::InvalidParse)?;
    let nested = match &mut trees[at].kind {
        Kind::Leaf(_) => Outcome::NoCut,
        Kind::Quote(children) => at_cut(children, index, backwards)?,
        Kind::List(_, items) => {
            let item = items
                .iter()
                .position(|item| item.iter().any(|t| t.leaves.contains(&index)))
                .ok_or(Error::InvalidParse)?;
            let outcome = at_cut(&mut items[item], index, backwards)?;
            if outcome == Outcome::NoCut
                && let Some(right) = neighbor(item, items.len(), backwards)
            {
                let following = items.remove(right);
                items[right - 1].extend(following);
                Outcome::Changed
            } else {
                outcome
            }
        }
    };
    if nested != Outcome::NoCut {
        return Ok(nested);
    }
    if let Some(right) = neighbor(at, trees.len(), backwards) {
        barrier(trees, right)
    } else {
        Ok(Outcome::NoCut)
    }
}

fn neighbor(at: usize, count: usize, backwards: bool) -> Option<usize> {
    if backwards {
        (at > 0).then_some(at)
    } else {
        (at + 1 < count).then_some(at + 1)
    }
}

fn barrier(trees: &mut Vec<Tree>, right: usize) -> Result<Outcome, Error> {
    if matches!(trees[right - 1].kind, Kind::Leaf(_)) {
        if matches!(trees[right].kind, Kind::Leaf(_)) {
            return Ok(Outcome::Text);
        }
        let Some(lifted) = lift_first(trees[right].clone())? else {
            return Ok(Outcome::Text);
        };
        trees.splice(right..=right, lifted);
        return Ok(Outcome::Changed);
    }
    let following = trees.remove(right);
    match (&mut trees[right - 1].kind, following.kind) {
        (Kind::Quote(children), Kind::Quote(next)) => children.extend(next),
        (Kind::List(a, items), Kind::List(b, next)) if a.is_some() == b.is_some() => {
            items.extend(next);
        }
        (Kind::Quote(children), kind) => children.push(Tree { kind, ..following }),
        (Kind::List(_, items), kind) => items.push(vec![Tree { kind, ..following }]),
        (Kind::Leaf(_), _) => return Err(Error::InvalidParse),
    }
    Ok(Outcome::Changed)
}

fn lift_first(mut tree: Tree) -> Result<Option<Vec<Tree>>, Error> {
    let children = match &mut tree.kind {
        Kind::Leaf(_) => return Ok(None),
        Kind::Quote(children) => children,
        Kind::List(_, items) => items.first_mut().ok_or(Error::InvalidParse)?,
    };
    let first = children.first().ok_or(Error::InvalidParse)?;
    if let Some(lifted) = lift_first(first.clone())? {
        children.splice(0..1, lifted);
        return Ok(Some(vec![tree]));
    }
    let lifted = children.remove(0);
    let retain = if children.is_empty() {
        match &mut tree.kind {
            Kind::List(_, items) => {
                items.remove(0);
                !items.is_empty()
            }
            _ => false,
        }
    } else {
        true
    };
    Ok(Some(
        std::iter::once(lifted)
            .chain(retain.then_some(tree))
            .collect(),
    ))
}
