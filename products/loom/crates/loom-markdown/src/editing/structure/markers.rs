//! Original list punctuation, indexed once for a structural serialization pass.
use crate::{Markdown, NodeKind};
use std::ops::Range;

struct Entry {
    source: Range<usize>,
    parent: Option<usize>,
    ordered: bool,
    marker: char,
}
pub(super) struct Markers(Vec<Entry>);
impl Markers {
    pub(super) fn new(markdown: &Markdown) -> Self {
        let mut entries: Vec<Entry> = vec![];
        let mut parents: Vec<usize> = vec![];
        for node in markdown.nodes() {
            let NodeKind::List(order) = node.kind else {
                continue;
            };
            while parents
                .last()
                .is_some_and(|&parent| entries[parent].source.end <= node.source.start)
            {
                parents.pop();
            }
            let raw = markdown.source()[node.source.clone()].trim_start_matches([' ', '\t']);
            let marker = if order.is_some() {
                raw.trim_start_matches(|c: char| c.is_ascii_digit())
                    .chars()
                    .next()
                    .filter(|&c| c == '.' || c == ')')
                    .unwrap_or('.')
            } else {
                raw.chars()
                    .next()
                    .filter(|&c| matches!(c, '*' | '-' | '+'))
                    .unwrap_or('*')
            };
            let parent = parents.last().copied();
            parents.push(entries.len());
            entries.push(Entry {
                source: node.source.clone(),
                parent,
                ordered: order.is_some(),
                marker,
            });
        }
        Self(entries)
    }
    pub(super) fn at(&self, offset: usize, ordered: bool) -> char {
        let mut index = self
            .0
            .partition_point(|entry| entry.source.start <= offset)
            .checked_sub(1);
        while let Some(i) = index {
            let entry = &self.0[i];
            if offset < entry.source.end && entry.ordered == ordered {
                return entry.marker;
            }
            index = entry.parent;
        }
        if ordered { '.' } else { '*' }
    }
}
