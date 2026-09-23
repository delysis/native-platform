use super::*;
use accesskit::Action;

#[test]
fn suspended_tree_retains_only_a_noninteractive_unbounded_window_root() {
    let update = cleared_tree(NodeId(7));
    assert_eq!(window_root(&update).unwrap(), NodeId(7));
    assert_eq!(update.focus, NodeId(7));
    assert_eq!(update.nodes.len(), 1);
    let root = &update.nodes[0].1;
    assert_eq!(root.role(), Role::Window);
    assert!(root.children().is_empty());
    assert!(root.bounds().is_none());
    assert!(!root.supports_action(Action::Focus));
    assert!(!root.supports_action(Action::SetValue));
}

#[test]
fn publication_requires_a_complete_unique_window_root() {
    let mut update = cleared_tree(NodeId(7));
    update.tree = None;
    assert_eq!(window_root(&update), Err(Error::Tree));
    let mut update = cleared_tree(NodeId(7));
    update.nodes.push((NodeId(7), Node::new(Role::Window)));
    assert_eq!(window_root(&update), Err(Error::Tree));
    let mut update = cleared_tree(NodeId(7));
    update.nodes[0].1 = Node::new(Role::Button);
    assert_eq!(window_root(&update), Err(Error::Tree));
}
