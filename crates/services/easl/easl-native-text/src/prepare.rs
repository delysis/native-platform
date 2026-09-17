use crate::{Alignment, Error, InlineBox, StyledSpan, TextStyle, WhiteSpace};
use parley::{AlignmentOptions, FontContext, Layout, LayoutContext, PositionedLayoutItem};
use serde::Serialize;
use std::ops::Range;

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_SPANS: usize = 4096;
pub const MAX_LINES: usize = 131_072;

/// Bounds source allocation and context shaping before entering the text engine.
pub fn validate_text(source: &str) -> Result<(), Error> {
    if source.len() > MAX_TEXT_BYTES {
        return Err(Error::Limit);
    }
    let mut word_start = 0;
    let mut discretionary_work = 0usize;
    for (i, c) in source.char_indices() {
        if c.is_whitespace() {
            word_start = i + c.len_utf8();
        }
        if c == '\u{ad}' {
            discretionary_work = discretionary_work.saturating_add(i - word_start);
        }
    }
    if discretionary_work > 8 * 1024 * 1024 {
        return Err(Error::Limit);
    }
    Ok(())
}

/// Original byte boundaries are retained through whitespace normalization.
#[derive(Debug)]
struct Normalized {
    text: String,
    boundaries: Vec<(usize, usize)>,
}
impl Normalized {
    fn new(source: &str, mode: WhiteSpace) -> Self {
        let mut text = String::new();
        let mut boundaries = vec![(0, 0)];
        let mut pending = None;
        for (i, c) in source.char_indices() {
            if mode == WhiteSpace::Collapse && matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                pending = Some(i + c.len_utf8());
                continue;
            }
            if let Some(end) = pending.take()
                && !text.is_empty()
            {
                text.push(' ');
                boundaries.push((text.len(), end));
            }
            if let Some(last) = boundaries.last_mut() {
                last.1 = i;
            }
            text.push(c);
            boundaries.push((text.len(), i + c.len_utf8()));
        }
        if let Some(last) = boundaries.last_mut() {
            last.1 = source.len();
        }
        Self { text, boundaries }
    }
    fn source_offset(&self, offset: usize) -> usize {
        let i = self
            .boundaries
            .partition_point(|(normalized, _)| *normalized < offset);
        self.boundaries
            .get(i)
            .map_or_else(|| self.boundaries.last().map_or(0, |b| b.1), |b| b.1)
    }
    fn normalized_offset(&self, offset: usize) -> usize {
        let i = self
            .boundaries
            .partition_point(|(_, source)| *source < offset);
        self.boundaries.get(i).map_or(self.text.len(), |b| b.0)
    }
}

pub struct TextSystem {
    pub fonts: FontContext,
    pub layouts: LayoutContext<[u8; 4]>,
    pub preparations: u64,
}
impl std::fmt::Debug for TextSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextSystem")
            .field("preparations", &self.preparations)
            .finish_non_exhaustive()
    }
}
impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}
impl TextSystem {
    pub fn new() -> Self {
        Self {
            fonts: FontContext::new(),
            layouts: LayoutContext::new(),
            preparations: 0,
        }
    }
    pub fn prepare(
        &mut self,
        source: &str,
        style: &TextStyle,
        white_space: WhiteSpace,
        spans: &[StyledSpan],
        boxes: &[InlineBox],
    ) -> Result<PreparedText, Error> {
        if source.len() > MAX_TEXT_BYTES || spans.len() > MAX_SPANS || boxes.len() > 512 {
            return Err(Error::Limit);
        }
        validate_text(source)?;
        style.validate()?;
        for span in spans {
            span.style.validate()?;
            if span.range.start > span.range.end
                || !source.is_char_boundary(span.range.start)
                || !source.is_char_boundary(span.range.end)
            {
                return Err(Error::InvalidRange);
            }
        }
        for b in boxes {
            if !source.is_char_boundary(b.index)
                || !b.width.is_finite()
                || !b.height.is_finite()
                || !(0. ..=16384.).contains(&b.width)
                || !(0. ..=16384.).contains(&b.height)
            {
                return Err(Error::InvalidGeometry);
            }
        }
        let normalized = Normalized::new(source, white_space);
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, &normalized.text, 1., false);
        for property in style.properties() {
            builder.push_default(property);
        }
        for span in spans {
            let range = normalized.normalized_offset(span.range.start)
                ..normalized.normalized_offset(span.range.end);
            if range.is_empty() {
                continue;
            }
            for property in span.style.properties() {
                builder.push(property, range.clone());
            }
        }
        for b in boxes {
            builder.push_inline_box(parley::InlineBox {
                id: b.id,
                kind: parley::InlineBoxKind::InFlow,
                index: normalized.normalized_offset(b.index),
                width: b.width,
                height: b.height,
            });
        }
        let layout = builder.build(&normalized.text);
        self.preparations = self.preparations.saturating_add(1);
        Ok(PreparedText {
            source: source.into(),
            normalized,
            layout,
            reflows: 0,
        })
    }
}

pub struct PreparedText {
    source: String,
    normalized: Normalized,
    pub(crate) layout: Layout<[u8; 4]>,
    pub reflows: u64,
}
impl std::fmt::Debug for PreparedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedText")
            .field("bytes", &self.source.len())
            .field("reflows", &self.reflows)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct LineBox {
    pub x: f32,
    pub y: f64,
    pub width: f32,
}
#[derive(Clone, Debug, Serialize)]
pub struct LineRange {
    pub source: Range<usize>,
    pub normalized: Range<usize>,
    pub x: f32,
    pub y: f64,
    pub baseline: f32,
    pub width: f32,
    pub height: f32,
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct LineStats {
    pub line_count: usize,
    pub width: f32,
    pub height: f32,
}
impl PreparedText {
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn normalized_text(&self) -> &str {
        &self.normalized.text
    }
    pub fn layout(&self) -> &Layout<[u8; 4]> {
        &self.layout
    }
    /// Reuses font selection and shaped glyphs. The callback can route each row
    /// around exclusions or into another column without re-preparing the text.
    pub fn flow(
        &mut self,
        max_width: f32,
        alignment: Alignment,
        mut line_box: impl FnMut(usize, f64) -> LineBox,
    ) -> Result<LineStats, Error> {
        validate_width(max_width)?;
        // A caller can reject any row, including after several committed lines.
        // Preserve the last drawable layout on every error. Font data are shared.
        let previous = self.layout.clone();
        match self.flow_inner(max_width, alignment, &mut line_box) {
            Ok(stats) => Ok(stats),
            Err(error) => {
                self.layout = previous;
                Err(error)
            }
        }
    }
    fn flow_inner(
        &mut self,
        max_width: f32,
        alignment: Alignment,
        line_box: &mut impl FnMut(usize, f64) -> LineBox,
    ) -> Result<LineStats, Error> {
        let mut breaker = self.layout.break_lines();
        breaker.state_mut().set_layout_max_advance(max_width);
        let mut count = 0;
        while !breaker.is_done() {
            if count >= MAX_LINES {
                return Err(Error::Limit);
            }
            let box_ = line_box(count, breaker.committed_y());
            validate_width(box_.width)?;
            if box_.width > max_width
                || !box_.x.is_finite()
                || box_.x.abs() > 1e7
                || !box_.y.is_finite()
                || box_.y.abs() > 1e9
            {
                return Err(Error::InvalidGeometry);
            }
            breaker.state_mut().set_line_x(box_.x);
            breaker.state_mut().set_line_y(box_.y);
            breaker.state_mut().set_line_max_advance(box_.width);
            match breaker.break_next() {
                Some(parley::YieldData::LineBreak(_)) => count += 1,
                None => break,
                _ => return Err(Error::UnsupportedFlow),
            }
        }
        breaker.finish();
        self.layout
            .align(alignment.into(), AlignmentOptions::default());
        self.reflows = self.reflows.saturating_add(1);
        self.check_geometry()?;
        Ok(self.stats())
    }
    pub fn reflow(&mut self, width: f32, alignment: Alignment) -> Result<LineStats, Error> {
        self.flow(width, alignment, |_, y| LineBox { x: 0., y, width })
    }
    /// Balance a short heading at its existing line count. Search is bounded and
    /// only re-breaks cached glyphs; it does not reshape or mutate source text.
    pub fn balance(&mut self, width: f32, alignment: Alignment) -> Result<LineStats, Error> {
        let target = self.reflow(width, alignment)?.line_count;
        if target <= 1 || target > 12 {
            return Ok(self.stats());
        }
        let mut low = 1.;
        let mut high = width;
        for _ in 0..16 {
            let mid = (low + high) * 0.5;
            if self.reflow(mid, alignment)?.line_count > target {
                low = mid;
            } else {
                high = mid;
            }
        }
        self.reflow(high, alignment)
    }
    pub fn natural_width(&mut self) -> Result<f32, Error> {
        Ok(self.reflow(1e7, Alignment::Start)?.width)
    }
    pub fn stats(&self) -> LineStats {
        if self.normalized.text.is_empty() && self.layout.inline_boxes().is_empty() {
            return LineStats::default();
        }
        LineStats {
            line_count: self.layout.len(),
            width: self.layout.width(),
            height: self.layout.height(),
        }
    }
    pub fn lines(&self) -> impl Iterator<Item = LineRange> + '_ {
        self.layout
            .lines()
            .filter(|_| !self.normalized.text.is_empty() || !self.layout.inline_boxes().is_empty())
            .map(|line| {
                let r = line.text_range();
                let m = line.metrics();
                LineRange {
                    source: self.normalized.source_offset(r.start)
                        ..self.normalized.source_offset(r.end),
                    normalized: r,
                    x: m.inline_min_coord + m.offset,
                    y: f64::from(m.block_min_coord),
                    baseline: m.baseline,
                    width: m.advance,
                    height: m.line_height,
                }
            })
    }
    pub fn materialize<'a>(&'a self, line: &LineRange) -> Result<&'a str, Error> {
        self.normalized
            .text
            .get(line.normalized.clone())
            .ok_or(Error::InvalidRange)
    }
    pub fn hit_test(&self, x: f32, y: f32) -> Result<usize, Error> {
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::InvalidGeometry);
        }
        let nearest = self.layout.lines().enumerate().min_by(|(_, a), (_, b)| {
            let distance = |line: &parley::Line<'_, [u8; 4]>| {
                let m = line.metrics();
                let dx = (m.inline_min_coord - x).max(0.).max(x - m.inline_max_coord);
                let dy = (m.block_min_coord - y).max(0.).max(y - m.block_max_coord);
                f64::from(dx).powi(2) + f64::from(dy).powi(2)
            };
            distance(a).total_cmp(&distance(b))
        });
        let Some((index, line)) = nearest else {
            return Ok(0);
        };
        let byte = parley::Cluster::from_point_in_line(&self.layout, index, x).map_or(
            line.text_range().start,
            |(cluster, side)| {
                let left = side == parley::ClusterSide::Left;
                if left != cluster.is_rtl() {
                    cluster.text_range().start
                } else {
                    cluster.text_range().end
                }
            },
        );
        // Combining marks and fallback fragments must not expose interior carets.
        use unicode_segmentation::UnicodeSegmentation;
        let byte = self
            .normalized
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([self.normalized.text.len()])
            .min_by_key(|i| i.abs_diff(byte))
            .unwrap_or(0);
        Ok(self.normalized.source_offset(byte))
    }
    /// Occupied line-box bounds, including caller-specified columns and offsets.
    pub fn bounds(&self) -> [f32; 4] {
        if self.stats().line_count == 0 {
            return [0.; 4];
        }
        let mut bounds: Option<[f32; 4]> = None;
        for line in self.layout.lines() {
            let m = line.metrics();
            let r = [
                m.inline_min_coord + m.offset,
                m.block_min_coord,
                m.inline_min_coord + m.offset + m.advance,
                m.block_max_coord,
            ];
            bounds = Some(bounds.map_or(r, |b| {
                [
                    b[0].min(r[0]),
                    b[1].min(r[1]),
                    b[2].max(r[2]),
                    b[3].max(r[3]),
                ]
            }));
        }
        bounds.map_or([0.; 4], |b| [b[0], b[1], b[2] - b[0], b[3] - b[1]])
    }

    pub(crate) fn check_geometry(&self) -> Result<(), Error> {
        for line in self.layout.lines() {
            let m = line.metrics();
            if !m.advance.is_finite() || !m.baseline.is_finite() || !m.line_height.is_finite() {
                return Err(Error::InvalidGeometry);
            }
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    for glyph in run.positioned_glyphs() {
                        if !glyph.x.is_finite() || !glyph.y.is_finite() {
                            return Err(Error::InvalidGeometry);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
fn validate_width(width: f32) -> Result<(), Error> {
    if !width.is_finite() || !(0. ..=1e7).contains(&width) {
        Err(Error::InvalidGeometry)
    } else {
        Ok(())
    }
}
