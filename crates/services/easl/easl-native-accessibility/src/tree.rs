use crate::Error;
use accesskit::{Node, NodeId, Role, Tree, TreeId, TreeUpdate};

pub(crate) fn window_root(update: &TreeUpdate) -> Result<NodeId, Error> {
    let root = update.tree.as_ref().ok_or(Error::Tree)?.root;
    let mut nodes = update.nodes.iter().filter(|(id, _)| *id == root);
    let (_, node) = nodes.next().ok_or(Error::Tree)?;
    if update.tree_id != TreeId::ROOT || node.role() != Role::Window || nodes.next().is_some() {
        return Err(Error::Tree);
    }
    Ok(root)
}

/// Retire child handles before the native window can detach from its `NSView`.
pub(crate) fn cleared_tree(root: NodeId) -> TreeUpdate {
    TreeUpdate {
        nodes: vec![(root, Node::new(Role::Window))],
        tree: Some(Tree::new(root)),
        tree_id: TreeId::ROOT,
        focus: root,
    }
}

#[cfg(test)]
mod tests;
