// Copyright 2026 the EASL Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Brush, Layout, YieldData};

/// Paragraph boxes supplied by an external document model. Coordinates are in
/// layout units, like the editor width. Insets name physical horizontal edges;
/// callers may resolve logical start/end against their paragraph direction.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParagraphLayout {
    /// UTF-8 byte index of the first character, or an empty final paragraph.
    pub start: usize,
    pub inset_left: f32,
    pub inset_right: f32,
    /// Additional inset on the first visual line, including negative hanging indents.
    pub first_line_indent: f32,
    pub space_before: f32,
    pub space_after: f32,
}

impl ParagraphLayout {
    pub(crate) fn valid(&self, text: &str) -> bool {
        text.is_char_boundary(self.start)
            && !(text[..self.start].ends_with('\r') && text[self.start..].starts_with('\n'))
            && (self.start == 0
                || text[..self.start].ends_with(['\n', '\r', '\u{2028}', '\u{2029}']))
            && [
                self.inset_left,
                self.inset_right,
                self.space_before,
                self.space_after,
            ]
            .iter()
            .all(|n| n.is_finite() && *n >= 0.)
            && self.first_line_indent.is_finite()
            && (self.inset_left + self.first_line_indent).is_finite()
            && self.inset_left + self.first_line_indent >= 0.
    }
}

pub(super) fn break_editor_lines<B: Brush>(
    layout: &mut Layout<B>,
    width: Option<f32>,
    paragraphs: &[ParagraphLayout],
) {
    if paragraphs.is_empty() {
        layout.break_all_lines(width);
        return;
    }
    let width = width.unwrap_or(f32::MAX);
    let mut breaker = layout.break_lines();
    breaker.state_mut().set_layout_max_advance(width);
    let mut previous = None;
    let mut after = 0.;
    let height;
    loop {
        let start = breaker.next_text_start();
        let index = paragraphs
            .partition_point(|p| p.start <= start)
            .checked_sub(1);
        let paragraph = index.map_or_else(ParagraphLayout::default, |i| paragraphs[i]);
        let first = start == paragraph.start && (previous != index || start == 0);
        let before = if previous != index || first {
            after + paragraph.space_before
        } else {
            0.
        };
        let left = paragraph.inset_left
            + if first {
                paragraph.first_line_indent
            } else {
                0.
            };
        let y = breaker.state().line_y() + f64::from(before);
        breaker.state_mut().set_line_y(y);
        breaker.state_mut().set_line_x(left);
        breaker
            .state_mut()
            .set_line_max_advance((width - left - paragraph.inset_right).max(0.));
        match breaker.break_next() {
            Some(YieldData::LineBreak(_)) => {
                previous = index;
                after = paragraph.space_after;
                if breaker.is_done() {
                    height = breaker.committed_y() + f64::from(after);
                    break;
                }
            }
            None => {
                // No extra paragraph space for a failed final iteration.
                height = breaker.committed_y() - f64::from(before) + f64::from(after);
                break;
            }
            // PlainEditor never installs inline boxes or a maximum line height.
            Some(_) => unreachable!("plain editor yielded a custom flow object"),
        }
    }
    breaker.finish();
    // Generic fragmented layout computes the sum of line heights. Here the
    // document height includes the paragraph gaps used by glyphs and hit testing.
    layout.data.height = (height as f32).max(
        layout
            .data
            .lines
            .last()
            .map_or(0., |line| line.metrics.block_max_coord + after),
    );
}
