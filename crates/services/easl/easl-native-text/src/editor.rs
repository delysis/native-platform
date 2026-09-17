use crate::{Error, MAX_TEXT_BYTES, StyledSpan, TextStyle, TextSystem, prepare::validate_text};
pub use parley::ParagraphLayout;
// Ranged style resolution may create two endpoints per span, plus the default
// style and IME decoration. Parley's style identifiers are still u16.
pub const MAX_EDITOR_SPANS: usize = 32_766;
use parley::{PlainEditor, PlainEditorDriver};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug)]
pub enum Movement {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    TextStart,
    TextEnd,
    PageUp,
    PageDown,
}
#[derive(Clone, Debug)]
pub enum EditCommand {
    Insert(String),
    Paste(String),
    Backspace,
    Delete,
    BackspaceWord,
    DeleteWord,
    Move(Movement, bool),
    SelectAll,
    Click(f32, f32, u8, bool),
    Drag(f32, f32),
    DeleteSelection,
    Undo,
    Redo,
    CancelCompose,
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
}
#[derive(Clone, Debug)]
struct Snapshot {
    text: String,
    anchor: usize,
    focus: usize,
}
#[derive(Debug)]
struct Undo {
    before: Snapshot,
    after: Snapshot,
}

/// Geometry and Unicode facts for a language-owned editing policy. Offsets are
/// UTF-8 bytes in the widget's current text, not source-model or glyph indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditContext {
    pub text_len: usize,
    pub anchor: usize,
    pub focus: usize,
    pub grapheme_before: usize,
    pub grapheme_after: usize,
    pub word_before: usize,
    pub word_after: usize,
}

/// A shaped caret edge, in layout coordinates. Several stops can share a byte
/// offset at a soft wrap or bidi boundary. Only complete grapheme edges appear.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretStop {
    pub byte: usize,
    pub affinity: parley::Affinity,
    pub line: usize,
    pub x: f32,
    /// Opposite edge of the owning shaped cluster, used to resolve pointer ties.
    pub other_x: f32,
    pub top: f32,
    pub bottom: f32,
}

/// Layout facts for an external line/page navigation policy. All positions
/// refer to this widget's current shaped projection, not Markdown source bytes.
#[derive(Clone, Copy, Debug)]
pub struct NavigationContext {
    pub line: usize,
    pub lines: usize,
    pub x: f32,
    pub top: f32,
    pub bottom: f32,
    pub viewport_height: f32,
    pub extent_height: f32,
    pub preferred_x: Option<f32>,
}

#[derive(Clone, Copy, Debug)]
pub struct LineGeometry {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    /// Start of the complete trailing separator EGC, or `end` at a soft break.
    pub separator_start: usize,
    pub top: f32,
    pub bottom: f32,
}

pub struct TextEditor {
    inner: PlainEditor<[u8; 4]>,
    // Bounded by MAX_TEXT_BYTES + 1 entries. Navigation and deletion consult
    // this index; only a changed text buffer requires Unicode segmentation.
    graphemes: Vec<usize>,
    style: TextStyle,
    spans: Vec<StyledSpan>,
    paragraphs: Vec<ParagraphLayout>,
    width: f32,
    height: f32,
    undo: VecDeque<Undo>,
    redo: Vec<Undo>,
    composition: Option<Snapshot>,
    pub scroll: f32,
    pub reveal_caret: bool,
}
impl std::fmt::Debug for TextEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextEditor")
            .field("bytes", &self.inner.raw_text().len())
            .field("undo", &self.undo.len())
            .finish_non_exhaustive()
    }
}
impl TextEditor {
    pub fn new(text: &str, style: TextStyle) -> Result<Self, Error> {
        validate_text(text)?;
        style.validate()?;
        let mut inner = PlainEditor::new(style.size);
        inner.set_quantize(false);
        inner.set_text(text);
        for p in style.properties() {
            inner.edit_styles().insert(p);
        }
        let mut graphemes = Vec::new();
        index_graphemes(&mut graphemes, text);
        Ok(Self {
            inner,
            graphemes,
            style,
            spans: Vec::new(),
            paragraphs: Vec::new(),
            width: 0.,
            height: 0.,
            undo: VecDeque::new(),
            redo: Vec::new(),
            composition: None,
            scroll: 0.,
            reveal_caret: true,
        })
    }
    pub fn text(&self) -> String {
        self.composition.as_ref().map_or_else(
            || self.inner.text().into_iter().collect(),
            |snapshot| snapshot.text.clone(),
        )
    }
    pub fn equals(&self, text: &str) -> bool {
        self.composition.as_ref().map_or_else(
            || self.inner.text() == text,
            |snapshot| snapshot.text == text,
        )
    }
    pub fn selected_text(&self) -> Option<&str> {
        self.inner.selected_text()
    }
    pub fn inner(&self) -> &PlainEditor<[u8; 4]> {
        &self.inner
    }
    /// Install a styled projection supplied by an external document model.
    /// That model owns semantic edits and history; this widget owns geometry,
    /// selection and temporary IME presentation.
    pub fn set_projection(
        &mut self,
        system: &mut TextSystem,
        text: &str,
        spans: &[StyledSpan],
        selection: (usize, usize),
    ) -> Result<(), Error> {
        self.set_document_projection(system, text, spans, &[], selection)
    }
    /// Install all rich document geometry together, before refreshing selection.
    pub fn set_document_projection(
        &mut self,
        system: &mut TextSystem,
        text: &str,
        spans: &[StyledSpan],
        paragraphs: &[ParagraphLayout],
        selection: (usize, usize),
    ) -> Result<(), Error> {
        validate_text(text)?;
        validate_spans(text, spans)?;
        validate_paragraphs(text, paragraphs)?;
        if !text.is_char_boundary(selection.0) || !text.is_char_boundary(selection.1) {
            return Err(Error::InvalidRange);
        }
        self.inner.set_text(text);
        index_graphemes(&mut self.graphemes, text);
        self.spans.clear();
        self.paragraphs.clear();
        self.undo.clear();
        self.redo.clear();
        self.composition = None;
        self.set_spans(spans)?;
        self.set_paragraphs(paragraphs)?;
        let mut driver = self.inner.driver(&mut system.fonts, &mut system.layouts);
        driver.select_byte_range(selection.0, selection.1);
        snap_selection(&mut driver, &self.graphemes, None);
        self.reveal_caret = true;
        Ok(())
    }
    /// Unchanged paragraph boxes do not invalidate shaping or selection.
    pub fn set_paragraphs(&mut self, paragraphs: &[ParagraphLayout]) -> Result<(), Error> {
        if self.paragraphs == paragraphs {
            return Ok(());
        }
        validate_paragraphs(self.inner.raw_text(), paragraphs)?;
        if !self.inner.set_paragraph_layouts(paragraphs.to_vec()) {
            return Err(Error::InvalidRange);
        }
        self.paragraphs = paragraphs.to_vec();
        Ok(())
    }
    /// Reapply after a projection/style change or native undo/composition reset.
    /// Unchanged style lists do not invalidate shaping.
    pub fn set_spans(&mut self, spans: &[StyledSpan]) -> Result<(), Error> {
        if self.spans == spans {
            return Ok(());
        }
        validate_spans(self.inner.raw_text(), spans)?;
        let ranges = spans
            .iter()
            .map(|span| {
                let mut styles = parley::StyleSet::new(span.style.size);
                for property in span.style.properties() {
                    styles.insert(property);
                }
                (span.range.clone(), styles)
            })
            .collect();
        if !self.inner.set_ranged_styles(ranges) {
            return Err(Error::InvalidRange);
        }
        self.spans = spans.to_vec();
        Ok(())
    }
    pub fn selection_bytes(&self) -> (usize, usize) {
        (
            self.inner.raw_selection().anchor().index(),
            self.inner.raw_selection().focus().index(),
        )
    }

    pub fn edit_context(
        &mut self,
        system: &mut TextSystem,
        words: bool,
    ) -> Result<EditContext, Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        if words {
            self.inner
                .refresh_layout(&mut system.fonts, &mut system.layouts);
        }
        let selection = self.inner.raw_selection();
        let focus = selection.focus().index();
        let index = self.graphemes.partition_point(|&offset| offset < focus);
        let mut context = EditContext {
            text_len: self.inner.raw_text().len(),
            anchor: selection.anchor().index(),
            focus,
            grapheme_before: self.graphemes[index.saturating_sub(1)],
            grapheme_after: self.graphemes.get(index + 1).copied().unwrap_or(focus),
            word_before: focus,
            word_after: focus,
        };
        if words && let Some(layout) = self.inner.try_layout() {
            context.word_before = selection.focus().previous_logical_word(layout).index();
            context.word_after = selection.focus().next_logical_word(layout).index();
            // Layout may report the LF inside a CRLF. A word operation stops
            // before an adjacent grapheme rather than consuming part of it.
            context.word_before =
                self.graphemes[self.graphemes.partition_point(|&i| i < context.word_before)];
            context.word_after = self.graphemes[self
                .graphemes
                .partition_point(|&i| i <= context.word_after)
                .saturating_sub(1)];
        }
        Ok(context)
    }

    /// Apply a language-selected range without changing text or undo history.
    /// Every endpoint must be a complete grapheme boundary in the live buffer.
    pub fn select_range(
        &mut self,
        system: &mut TextSystem,
        selection: (usize, usize),
    ) -> Result<(), Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        if [selection.0, selection.1]
            .iter()
            .any(|offset| self.graphemes.binary_search(offset).is_err())
        {
            return Err(Error::InvalidRange);
        }
        self.inner
            .driver(&mut system.fonts, &mut system.layouts)
            .select_byte_range(selection.0, selection.1);
        self.reveal_caret = true;
        Ok(())
    }

    /// Stream the shaped caret stops on the indexed line at `y`, without
    /// copying source text or retaining a stale geometry snapshot. The callback
    /// can batch stops into a language runtime. Layout and Unicode supply facts;
    /// the consumer chooses the nearest edge and its tie/affinity policy.
    pub fn visit_caret_stops<E: From<Error>>(
        &mut self,
        system: &mut TextSystem,
        y: f32,
        emit: impl FnMut(CaretStop) -> Result<(), E>,
    ) -> Result<(), E> {
        if !y.is_finite() {
            return Err(Error::InvalidGeometry.into());
        }
        if self.composition.is_some() {
            return Err(Error::CompositionActive.into());
        }
        self.inner
            .refresh_layout(&mut system.fonts, &mut system.layouts);
        let layout = self.inner.try_layout().ok_or(Error::InvalidGeometry)?;
        let (line_index, _) = layout.line_for_offset(y).ok_or(Error::InvalidGeometry)?;
        self.visit_line_caret_stops(system, line_index, emit)
    }

    /// Stream one explicitly selected line after refreshing the current layout.
    pub fn visit_line_caret_stops<E: From<Error>>(
        &mut self,
        system: &mut TextSystem,
        line_index: usize,
        mut emit: impl FnMut(CaretStop) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive.into());
        }
        self.inner
            .refresh_layout(&mut system.fonts, &mut system.layouts);
        let layout = self.inner.try_layout().ok_or(Error::InvalidGeometry)?;
        let line = layout.get(line_index).ok_or(Error::InvalidGeometry)?;
        let metrics = line.metrics();
        let mut x = metrics.inline_min_coord + metrics.offset;
        if line.text_range().is_empty() {
            // Empty lines have a shaped font strut, not source glyphs. Its
            // phantom space advance must never become an extra caret position.
            return emit(CaretStop {
                byte: line.text_range().start,
                affinity: parley::Affinity::Downstream,
                line: line_index,
                x,
                other_x: x,
                top: metrics.block_min_coord,
                bottom: metrics.block_max_coord,
            });
        }
        let mut emitted = false;
        for run in line.runs() {
            let run_end = x + run.advance();
            for cluster in run.visual_clusters() {
                let range = cluster.text_range();
                let advance = cluster.advance();
                for trailing in [false, true] {
                    let byte = if trailing { range.end } else { range.start };
                    // A hard break's trailing caret belongs to the next line.
                    // Filtering edges independently also handles an EGC split
                    // between fonts or bidi runs without inventing an inner caret.
                    if (trailing && cluster.is_hard_line_break())
                        || self.graphemes.binary_search(&byte).is_err()
                    {
                        continue;
                    }
                    emit(CaretStop {
                        byte,
                        affinity: if trailing {
                            parley::Affinity::Upstream
                        } else {
                            parley::Affinity::Downstream
                        },
                        line: line_index,
                        x: x + if trailing != cluster.is_rtl() {
                            advance
                        } else {
                            0.
                        },
                        other_x: x + if trailing == cluster.is_rtl() {
                            advance
                        } else {
                            0.
                        },
                        top: metrics.block_min_coord,
                        bottom: metrics.block_max_coord,
                    })?;
                    emitted = true;
                }
                x += advance;
            }
            x = run_end;
        }
        if !emitted {
            emit(CaretStop {
                byte: line.text_range().start,
                affinity: parley::Affinity::Downstream,
                line: line_index,
                x: metrics.inline_min_coord + metrics.offset,
                other_x: metrics.inline_min_coord + metrics.offset,
                top: metrics.block_min_coord,
                bottom: metrics.block_max_coord,
            })?;
        }
        Ok(())
    }

    pub fn navigation_context(
        &mut self,
        system: &mut TextSystem,
    ) -> Result<NavigationContext, Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        self.inner
            .refresh_layout(&mut system.fonts, &mut system.layouts);
        let layout = self.inner.try_layout().ok_or(Error::InvalidGeometry)?;
        let selection = self.inner.raw_selection();
        let rect = selection.focus().geometry(layout, 0.);
        let (line, _) = layout
            .line_for_offset(rect.y0 as f32)
            .ok_or(Error::InvalidGeometry)?;
        Ok(NavigationContext {
            line,
            lines: layout.len(),
            x: rect.x0 as f32,
            top: rect.y0 as f32,
            bottom: rect.y1 as f32,
            viewport_height: self.height,
            extent_height: layout.height(),
            preferred_x: selection.horizontal_position(),
        })
    }

    /// Read an indexed line without retaining a geometry snapshot across events.
    pub fn line_geometry(
        &mut self,
        system: &mut TextSystem,
        index: usize,
    ) -> Result<LineGeometry, Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        self.inner
            .refresh_layout(&mut system.fonts, &mut system.layouts);
        let layout = self.inner.try_layout().ok_or(Error::InvalidGeometry)?;
        let line = layout.get(index).ok_or(Error::InvalidGeometry)?;
        let range = line.text_range();
        let separator_start = if line.break_reason() == parley::layout::BreakReason::Explicit {
            let boundary = self.graphemes.partition_point(|&byte| byte < range.end);
            self.graphemes[boundary.saturating_sub(1)]
        } else {
            range.end
        };
        Ok(LineGeometry {
            index,
            start: range.start,
            end: range.end,
            separator_start,
            top: line.metrics().block_min_coord,
            bottom: line.metrics().block_max_coord,
        })
    }

    pub fn line_at_y(&mut self, system: &mut TextSystem, y: f32) -> Result<usize, Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        self.inner
            .refresh_layout(&mut system.fonts, &mut system.layouts);
        self.inner
            .try_layout()
            .and_then(|layout| layout.line_for_offset(y))
            .map(|(index, _)| index)
            .ok_or(Error::InvalidGeometry)
    }

    /// Commit a validated navigation result without editing source or history.
    pub fn select_navigation_target(
        &mut self,
        system: &mut TextSystem,
        byte: usize,
        affinity: parley::Affinity,
        extend: bool,
        preferred_x: Option<f32>,
    ) -> Result<(), Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        if self.graphemes.binary_search(&byte).is_err() {
            return Err(Error::InvalidRange);
        }
        if preferred_x.is_some_and(|x| !x.is_finite()) {
            return Err(Error::InvalidGeometry);
        }
        self.inner
            .driver(&mut system.fonts, &mut system.layouts)
            .select_navigation_target(byte, affinity, extend, preferred_x);
        self.reveal_caret = true;
        Ok(())
    }

    /// Apply an external hit synchronously to the same widget. Count zero is
    /// a captured drag, two selects a word, three a logical line. This changes
    /// neither the text nor undo history. All offsets must be live EGC edges.
    pub fn select_pointer_target(
        &mut self,
        system: &mut TextSystem,
        byte: usize,
        affinity: parley::Affinity,
        click_count: u8,
        extend: bool,
    ) -> Result<(), Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        if self.graphemes.binary_search(&byte).is_err() {
            return Err(Error::InvalidRange);
        }
        let mut driver = self.inner.driver(&mut system.fonts, &mut system.layouts);
        driver.select_pointer_target(byte, affinity, click_count, extend);
        snap_selection(&mut driver, &self.graphemes, None);
        self.reveal_caret = true;
        Ok(())
    }

    /// Native text-buffer transaction for a language-owned edit. Validate all
    /// input before changing the buffer and retain the original selection in
    /// the single undo entry. No OS event interpretation or edit policy lives here.
    pub fn replace_range(
        &mut self,
        system: &mut TextSystem,
        range: std::ops::Range<usize>,
        text: &str,
        selection_after: (usize, usize),
    ) -> Result<(), Error> {
        if self.composition.is_some() {
            return Err(Error::CompositionActive);
        }
        if range.start > range.end
            || [range.start, range.end]
                .iter()
                .any(|offset| self.graphemes.binary_search(offset).is_err())
        {
            return Err(Error::InvalidRange);
        }
        if text.len() > MAX_TEXT_BYTES
            || self.inner.raw_text().len() - range.len() + text.len() > MAX_TEXT_BYTES
        {
            return Err(Error::Limit);
        }
        let mut candidate = self.inner.raw_text().to_owned();
        candidate.replace_range(range.clone(), text);
        validate_text(&candidate)?;
        if [selection_after.0, selection_after.1]
            .iter()
            .any(|&offset| !candidate.is_char_boundary(offset))
        {
            return Err(Error::InvalidRange);
        }
        let before = self.snapshot();
        let mut driver = self.inner.driver(&mut system.fonts, &mut system.layouts);
        driver.select_byte_range(range.start, range.end);
        driver.insert_or_replace_selection(text);
        index_graphemes(&mut self.graphemes, driver.editor.raw_text());
        driver.select_byte_range(selection_after.0, selection_after.1);
        // Insertion may join a combining sequence across the replacement edge.
        // Match the existing native edit primitive's grapheme normalization.
        snap_selection(&mut driver, &self.graphemes, None);
        let after = self.snapshot();
        self.record_edit(before, after);
        self.reveal_caret = true;
        Ok(())
    }
    /// Logical deletion geometry for an external document model. This does not
    /// mutate text or history, including in bidirectional paragraphs.
    pub fn deletion_range(
        &mut self,
        system: &mut TextSystem,
        backward: bool,
        word: bool,
    ) -> std::ops::Range<usize> {
        self.inner
            .refresh_layout(&mut system.fonts, &mut system.layouts);
        let selection = self.inner.raw_selection();
        let mut range = selection.text_range();
        if !range.is_empty() {
            return range;
        }
        if word {
            if let Some(layout) = self.inner.try_layout() {
                if backward {
                    range.start = selection.focus().previous_logical_word(layout).index();
                } else {
                    range.end = selection.focus().next_logical_word(layout).index();
                }
            }
            range.start = self.graphemes[self.graphemes.partition_point(|&i| i < range.start)];
            range.end = self.graphemes[self
                .graphemes
                .partition_point(|&i| i <= range.end)
                .saturating_sub(1)];
        } else {
            let boundaries = &self.graphemes;
            let index = boundaries.partition_point(|&i| i < range.start);
            if backward {
                range.start = boundaries[index.saturating_sub(1)];
            } else {
                range.end = boundaries.get(index + 1).copied().unwrap_or(range.end);
            }
        }
        range
    }
    pub fn ensure_layout(
        &mut self,
        system: &mut TextSystem,
        style: &TextStyle,
        width: f32,
        height: f32,
    ) -> Result<(), Error> {
        style.validate()?;
        if !width.is_finite() || !height.is_finite() || width < 0. || height < 0. {
            return Err(Error::InvalidGeometry);
        }
        self.height = height;
        if &self.style != style {
            for p in style.properties() {
                self.inner.edit_styles().insert(p);
            }
            self.style = style.clone();
        }
        if self.width != width {
            self.inner.set_width(Some(width));
            self.width = width;
        }
        self.inner
            .refresh_layout(&mut system.fonts, &mut system.layouts);
        if self.reveal_caret {
            if let Some(c) = self.inner.cursor_geometry(1.) {
                if c.y0 < f64::from(self.scroll) {
                    self.scroll = c.y0 as f32;
                }
                if c.y1 > f64::from(self.scroll + height) {
                    self.scroll = (c.y1 as f32 - height).max(0.);
                }
            }
            self.reveal_caret = false;
        }
        let max = self
            .inner
            .try_layout()
            .map_or(0., |l| (l.height() - height).max(0.));
        self.scroll = self.scroll.clamp(0., max);
        Ok(())
    }
    pub fn accessibility(
        &mut self,
        system: &mut TextSystem,
        update: &mut accesskit::TreeUpdate,
        node: &mut accesskit::Node,
        next_id: &mut u64,
        origin: [f64; 2],
    ) {
        self.inner
            .driver(&mut system.fonts, &mut system.layouts)
            .accessibility(
                update,
                node,
                || {
                    *next_id += 1;
                    accesskit::NodeId(*next_id)
                },
                origin[0],
                origin[1],
                |_, _| {},
            );
    }
    pub fn select_accessible(
        &mut self,
        system: &mut TextSystem,
        selection: &accesskit::TextSelection,
    ) {
        let mut driver = self.inner.driver(&mut system.fonts, &mut system.layouts);
        driver.select_from_accesskit(selection);
        snap_selection(&mut driver, &self.graphemes, None);
        self.reveal_caret = true;
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text(),
            anchor: self.inner.raw_selection().anchor().index(),
            focus: self.inner.raw_selection().focus().index(),
        }
    }
    fn restore(&mut self, system: &mut TextSystem, snapshot: &Snapshot) {
        self.inner.set_text(&snapshot.text);
        index_graphemes(&mut self.graphemes, &snapshot.text);
        self.spans.clear();
        self.paragraphs.clear();
        self.inner
            .driver(&mut system.fonts, &mut system.layouts)
            .select_byte_range(snapshot.anchor, snapshot.focus);
    }
    pub fn command(&mut self, system: &mut TextSystem, command: EditCommand) -> Result<(), Error> {
        if matches!(
            command,
            EditCommand::Move(..)
                | EditCommand::SelectAll
                | EditCommand::Click(..)
                | EditCommand::Drag(..)
        ) {
            if let EditCommand::Click(x, y, _, _) | EditCommand::Drag(x, y) = command
                && (!x.is_finite() || !y.is_finite())
            {
                return Err(Error::InvalidGeometry);
            }
            // Selection changes neither validate nor snapshot the manuscript.
            // Cancelling a live composition restores its original text first.
            self.cancel_compose(system);
            let mut driver = self.inner.driver(&mut system.fonts, &mut system.layouts);
            match command {
                EditCommand::Move(m, extend) => {
                    let previous = driver.editor.raw_selection().focus().index();
                    movement(&mut driver, m, extend, self.height);
                    snap_selection(&mut driver, &self.graphemes, Some(previous));
                }
                EditCommand::SelectAll => driver.select_all(),
                EditCommand::Click(x, y, count, extend) => {
                    if extend {
                        driver.shift_click_extension(x, y);
                    } else {
                        match count {
                            2 => driver.select_word_at_point(x, y),
                            3 => driver.select_hard_line_at_point(x, y),
                            _ => driver.move_to_point(x, y),
                        }
                    }
                }
                EditCommand::Drag(x, y) => driver.extend_selection_to_point(x, y),
                _ => unreachable!(),
            }
            snap_selection(&mut driver, &self.graphemes, None);
            self.reveal_caret = true;
            return Ok(());
        }
        // Validate the exact replacement before the driver's eager reshaping.
        // Repeated preedit updates replace the original selection, not each other.
        if let EditCommand::Insert(text)
        | EditCommand::Paste(text)
        | EditCommand::Commit(text)
        | EditCommand::Preedit(text, _) = &command
        {
            if text.len() > MAX_TEXT_BYTES {
                return Err(Error::Limit);
            }
            let base = self.composition.clone().unwrap_or_else(|| self.snapshot());
            let range = base.anchor.min(base.focus)..base.anchor.max(base.focus);
            if base.text.len() - range.len() + text.len() > MAX_TEXT_BYTES {
                return Err(Error::Limit);
            }
            let mut candidate = base.text;
            candidate.replace_range(range, text);
            validate_text(&candidate)?;
        }
        match command {
            EditCommand::Undo | EditCommand::Redo => {
                self.cancel_compose(system);
                let redo = matches!(command, EditCommand::Redo);
                let change = if redo {
                    self.redo.pop()
                } else {
                    self.undo.pop_back()
                };
                if let Some(change) = change {
                    self.restore(system, if redo { &change.after } else { &change.before });
                    if redo {
                        self.undo.push_back(change);
                    } else {
                        self.redo.push(change);
                    }
                }
                self.reveal_caret = true;
                return Ok(());
            }
            EditCommand::CancelCompose => {
                self.cancel_compose(system);
                return Ok(());
            }
            EditCommand::Preedit(ref text, range) => {
                if text.is_empty() {
                    self.cancel_compose(system);
                    return Ok(());
                }
                if range.is_some_and(|(a, b)| {
                    a > b || !text.is_char_boundary(a) || !text.is_char_boundary(b)
                }) {
                    return Err(Error::InvalidRange);
                }
                if self.composition.is_none() {
                    self.composition = Some(self.snapshot());
                }
                self.inner
                    .driver(&mut system.fonts, &mut system.layouts)
                    .set_compose(text, range);
                index_graphemes(&mut self.graphemes, self.inner.raw_text());
                self.reveal_caret = true;
                return Ok(());
            }
            _ => {}
        }
        let before = if let Some(snapshot) = self.composition.take() {
            self.restore(system, &snapshot);
            snapshot
        } else {
            self.snapshot()
        };
        let word_range = match command {
            EditCommand::BackspaceWord => Some(self.deletion_range(system, true, true)),
            EditCommand::DeleteWord => Some(self.deletion_range(system, false, true)),
            _ => None,
        };
        let mut driver = self.inner.driver(&mut system.fonts, &mut system.layouts);
        match command {
            EditCommand::Insert(ref text)
            | EditCommand::Paste(ref text)
            | EditCommand::Commit(ref text) => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(Error::Limit);
                }
                driver.insert_or_replace_selection(text);
            }
            EditCommand::Backspace => delete_grapheme(&mut driver, &self.graphemes, true)?,
            EditCommand::Delete => delete_grapheme(&mut driver, &self.graphemes, false)?,
            EditCommand::BackspaceWord | EditCommand::DeleteWord => {
                if let Some(range) = word_range {
                    driver.select_byte_range(range.start, range.end);
                    driver.delete_selection();
                }
            }
            EditCommand::DeleteSelection => driver.delete_selection(),
            _ => {}
        }
        index_graphemes(&mut self.graphemes, driver.editor.raw_text());
        snap_selection(&mut driver, &self.graphemes, None);
        if let Err(error) = validate_text(self.inner.raw_text()) {
            self.restore(system, &before);
            return Err(error);
        }
        let after = self.snapshot();
        self.record_edit(before, after);
        self.reveal_caret = true;
        Ok(())
    }
    fn record_edit(&mut self, before: Snapshot, after: Snapshot) {
        if before.text != after.text {
            self.redo.clear();
            self.undo.push_back(Undo { before, after });
            while self.undo.len() > 128
                || self
                    .undo
                    .iter()
                    .map(|u| u.before.text.len() + u.after.text.len())
                    .sum::<usize>()
                    > 4 * 1024 * 1024
            {
                self.undo.pop_front();
            }
        }
    }
    fn cancel_compose(&mut self, system: &mut TextSystem) {
        if let Some(snapshot) = self.composition.take() {
            self.restore(system, &snapshot);
        }
    }
}
fn index_graphemes(index: &mut Vec<usize>, text: &str) {
    index.clear();
    index.extend(
        text.grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([text.len()]),
    );
}

fn delete_grapheme(
    d: &mut PlainEditorDriver<'_, [u8; 4]>,
    boundaries: &[usize],
    backward: bool,
) -> Result<(), Error> {
    let selection = d.editor.raw_selection();
    let mut range = selection.text_range();
    if range.is_empty() {
        let index = boundaries.partition_point(|i| *i < range.start);
        if backward {
            range.start = boundaries[index.saturating_sub(1)];
        } else {
            range.end = boundaries.get(index + 1).copied().unwrap_or(range.end);
        }
    }
    let mut candidate = d.editor.raw_text().to_owned();
    candidate.replace_range(range.clone(), "");
    validate_text(&candidate)?;
    d.select_byte_range(range.start, range.end);
    d.delete_selection();
    Ok(())
}

fn snap_selection(
    d: &mut PlainEditorDriver<'_, [u8; 4]>,
    boundaries: &[usize],
    previous_focus: Option<usize>,
) {
    let snap = |byte: usize, direction: Option<usize>| {
        let index = boundaries.partition_point(|i| *i < byte);
        let high = boundaries
            .get(index)
            .copied()
            .unwrap_or(d.editor.raw_text().len());
        let low = boundaries[index.saturating_sub(1)];
        if high == byte {
            byte
        } else if let Some(previous) = direction {
            if byte > previous { high } else { low }
        } else if byte - low <= high - byte {
            low
        } else {
            high
        }
    };
    let selection = d.editor.raw_selection();
    let anchor = snap(selection.anchor().index(), None);
    let focus = snap(selection.focus().index(), previous_focus);
    if anchor != selection.anchor().index() || focus != selection.focus().index() {
        d.select_byte_range(anchor, focus);
    }
}

fn validate_spans(text: &str, spans: &[StyledSpan]) -> Result<(), Error> {
    if spans.len() > MAX_EDITOR_SPANS {
        return Err(Error::Limit);
    }
    for span in spans {
        if span.range.start > span.range.end
            || !text.is_char_boundary(span.range.start)
            || !text.is_char_boundary(span.range.end)
        {
            return Err(Error::InvalidRange);
        }
        span.style.validate()?;
    }
    Ok(())
}

fn validate_paragraphs(text: &str, paragraphs: &[ParagraphLayout]) -> Result<(), Error> {
    if paragraphs.len() > 131_072 {
        return Err(Error::Limit);
    }
    if paragraphs.windows(2).any(|p| p[0].start >= p[1].start) {
        return Err(Error::InvalidRange);
    }
    for p in paragraphs {
        if !text.is_char_boundary(p.start)
            || (p.start != 0 && !text[..p.start].ends_with(['\r', '\n', '\u{2028}', '\u{2029}']))
            || (text[..p.start].ends_with('\r') && text[p.start..].starts_with('\n'))
        {
            return Err(Error::InvalidRange);
        }
        if [p.inset_left, p.inset_right, p.space_before, p.space_after]
            .iter()
            .any(|n| !n.is_finite() || !(0. ..=131_072.).contains(n))
            || !p.first_line_indent.is_finite()
            || p.first_line_indent.abs() > 65_536.
            || p.inset_left + p.first_line_indent < 0.
        {
            return Err(Error::InvalidGeometry);
        }
    }
    Ok(())
}

fn movement(d: &mut PlainEditorDriver<'_, [u8; 4]>, m: Movement, extend: bool, height: f32) {
    match (m, extend) {
        (Movement::Left, false) => d.move_left(),
        (Movement::Left, true) => d.select_left(),
        (Movement::Right, false) => d.move_right(),
        (Movement::Right, true) => d.select_right(),
        (Movement::Up, false) => d.move_up(),
        (Movement::Up, true) => d.select_up(),
        (Movement::Down, false) => d.move_down(),
        (Movement::Down, true) => d.select_down(),
        (Movement::WordLeft, false) => d.move_word_left(),
        (Movement::WordLeft, true) => d.select_word_left(),
        (Movement::WordRight, false) => d.move_word_right(),
        (Movement::WordRight, true) => d.select_word_right(),
        (Movement::LineStart, false) => d.move_to_line_start(),
        (Movement::LineStart, true) => d.select_to_line_start(),
        (Movement::LineEnd, false) => d.move_to_line_end(),
        (Movement::LineEnd, true) => d.select_to_line_end(),
        (Movement::TextStart, false) => d.move_to_text_start(),
        (Movement::TextStart, true) => d.select_to_text_start(),
        (Movement::TextEnd, false) => d.move_to_text_end(),
        (Movement::TextEnd, true) => d.select_to_text_end(),
        (Movement::PageUp, extend) => page_movement(d, height, true, extend),
        (Movement::PageDown, extend) => page_movement(d, height, false, extend),
    }
}

fn page_movement(d: &mut PlainEditorDriver<'_, [u8; 4]>, height: f32, up: bool, extend: bool) {
    d.refresh_layout();
    let Some(layout) = d.editor.try_layout() else {
        return;
    };
    let geometry = d.editor.raw_selection().focus().geometry(layout, 0.);
    let Some((current, _)) = layout.line_for_offset(geometry.y0 as f32) else {
        return;
    };
    let line_height = geometry.y1 - geometry.y0;
    // Keep one line of overlap, with at least one-line movement in a tiny view.
    let distance = (f64::from(height) - line_height).max(line_height);
    let target_y = (geometry.y0 + geometry.y1) * 0.5 + if up { -distance } else { distance };
    let delta = if target_y < 0. {
        -(current as isize) - 1
    } else if target_y >= f64::from(layout.height()) {
        (layout.len() - current) as isize
    } else {
        let target = layout
            .line_for_offset(target_y as f32)
            .map_or(current, |(i, _)| i);
        if target == current {
            if up { -1 } else { 1 }
        } else {
            target as isize - current as isize
        }
    };
    d.move_lines(delta, extend);
}
