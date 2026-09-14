use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use thiserror::Error;

use crate::MAX_DOCUMENT_PARTS;

/// The ID denotes one occurrence, never its content digest. Parent references
/// represent the caller's explicit branch relation, not textual similarity.
pub trait HistoryNode {
    type Id: Eq + Hash;
    fn id(&self) -> &Self::Id;
    fn parent_id(&self) -> Option<&Self::Id>;
}

/// Validated, linear-space parent graph. Multiple roots and sibling branches
/// are intentional; duplicate IDs, missing parents and cycles are not.
#[derive(Debug)]
pub struct BranchIndex<'a, Node: HistoryNode> {
    nodes: &'a [Node],
    by_id: HashMap<&'a Node::Id, usize>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

impl<'a, Node: HistoryNode> BranchIndex<'a, Node> {
    pub fn new(nodes: &'a [Node]) -> Result<Self, HistoryError> {
        if nodes.len() > MAX_DOCUMENT_PARTS {
            return Err(HistoryError::NodeBudget);
        }
        let mut by_id = HashMap::with_capacity(nodes.len());
        for (index, node) in nodes.iter().enumerate() {
            if by_id.insert(node.id(), index).is_some() {
                return Err(HistoryError::DuplicateId);
            }
        }
        let mut children = vec![Vec::new(); nodes.len()];
        let mut parents = vec![None; nodes.len()];
        let mut roots = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent_id() {
                let parent = *by_id.get(parent).ok_or(HistoryError::MissingParent)?;
                parents[index] = Some(parent);
                children[parent].push(index);
            } else {
                roots.push(index);
            }
        }
        // 0 = unseen, 1 = on the current walk, 2 = fully validated. Each node
        // is visited at most twice; a deep chat does not become quadratic.
        let mut marks = vec![0_u8; nodes.len()];
        for start in 0..nodes.len() {
            let mut current = Some(start);
            let mut path = Vec::new();
            while let Some(index) = current {
                match marks[index] {
                    1 => return Err(HistoryError::Cycle),
                    2 => break,
                    _ => {
                        marks[index] = 1;
                        path.push(index);
                        current = parents[index];
                    }
                }
            }
            for index in path {
                marks[index] = 2;
            }
        }
        Ok(Self {
            nodes,
            by_id,
            children,
            roots,
        })
    }

    pub fn path(&self, head: Option<&Node::Id>) -> Result<Vec<&'a Node>, HistoryError> {
        let mut path = Vec::new();
        let mut current = head;
        while let Some(id) = current {
            let node = self.node(id)?;
            path.push(node);
            current = node.parent_id();
        }
        path.reverse();
        Ok(path)
    }

    pub fn siblings(&self, id: &Node::Id) -> Result<Vec<&'a Node>, HistoryError> {
        let node = self.node(id)?;
        let positions = if let Some(parent) = node.parent_id() {
            &self.children[*self.by_id.get(parent).ok_or(HistoryError::MissingParent)?]
        } else {
            &self.roots
        };
        Ok(positions.iter().map(|index| &self.nodes[*index]).collect())
    }

    pub fn preferred_leaf(
        &self,
        start: &Node::Id,
        compare: impl Fn(&Node, &Node) -> std::cmp::Ordering,
    ) -> Result<&'a Node, HistoryError> {
        let mut index = *self.by_id.get(start).ok_or(HistoryError::MissingHead)?;
        while let Some(next) = self.children[index]
            .iter()
            .max_by(|left, right| compare(&self.nodes[**left], &self.nodes[**right]))
        {
            index = *next;
        }
        Ok(&self.nodes[index])
    }

    pub fn descendants(&self, start: &Node::Id) -> Result<HashSet<&'a Node::Id>, HistoryError> {
        let start = *self.by_id.get(start).ok_or(HistoryError::MissingHead)?;
        let mut result = HashSet::new();
        let mut pending = vec![start];
        while let Some(index) = pending.pop() {
            result.insert(self.nodes[index].id());
            pending.extend(self.children[index].iter().copied());
        }
        Ok(result)
    }

    fn node(&self, id: &Node::Id) -> Result<&'a Node, HistoryError> {
        let index = *self.by_id.get(id).ok_or(HistoryError::MissingHead)?;
        Ok(&self.nodes[index])
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HistoryError {
    #[error("document history exceeds the 65536 node budget")]
    NodeBudget,
    #[error("document history contains duplicate occurrence IDs")]
    DuplicateId,
    #[error("document history references a missing parent")]
    MissingParent,
    #[error("document history contains a cycle")]
    Cycle,
    #[error("document history references a missing selected head")]
    MissingHead,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Node(u32, Option<u32>);
    impl HistoryNode for Node {
        type Id = u32;
        fn id(&self) -> &u32 {
            &self.0
        }
        fn parent_id(&self) -> Option<&u32> {
            self.1.as_ref()
        }
    }
    #[test]
    fn branches_keep_ancestry_and_explicit_head() {
        let nodes = [
            Node(1, None),
            Node(2, Some(1)),
            Node(3, Some(1)),
            Node(4, Some(2)),
        ];
        let index = BranchIndex::new(&nodes).expect("valid history");
        assert_eq!(
            index
                .path(Some(&4))
                .expect("valid history")
                .iter()
                .map(|node| node.0)
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
        assert_eq!(
            index
                .preferred_leaf(&1, |left, right| left.0.cmp(&right.0))
                .expect("valid history")
                .0,
            3
        );
        assert_eq!(
            index.descendants(&2).expect("valid history"),
            HashSet::from([&2, &4])
        );
        assert_eq!(
            index
                .siblings(&2)
                .expect("siblings")
                .iter()
                .map(|node| node.0)
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(
            index.path(Some(&9)).expect_err("invalid history"),
            HistoryError::MissingHead
        );
    }
    #[test]
    fn rejects_corrupt_unselected_branches_too() {
        for (nodes, error) in [
            (
                vec![Node(1, None), Node(1, None)],
                HistoryError::DuplicateId,
            ),
            (vec![Node(1, Some(2))], HistoryError::MissingParent),
            (
                vec![Node(1, None), Node(2, Some(3)), Node(3, Some(2))],
                HistoryError::Cycle,
            ),
            (vec![Node(1, Some(1))], HistoryError::Cycle),
        ] {
            assert_eq!(
                BranchIndex::new(&nodes).expect_err("invalid history"),
                error
            );
        }
    }
    #[test]
    fn deep_history_is_iterative() {
        let nodes = (0..50_000)
            .map(|id| Node(id, id.checked_sub(1)))
            .collect::<Vec<_>>();
        assert_eq!(
            BranchIndex::new(&nodes)
                .expect("valid history")
                .path(Some(&49_999))
                .expect("valid history")
                .len(),
            50_000
        );
    }
}
