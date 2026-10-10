use crate::{Bias, Dialect, Error, InlineStyle, MAX_SOURCE_BYTES, Markdown, Projection};
use std::{
    collections::{BTreeMap, VecDeque},
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};
use unicode_segmentation::UnicodeSegmentation;
mod formatting;
mod normalization;
pub use formatting::*;

const MAX_EDITS: usize = 4096;
const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
const MAX_HISTORY_RECORDS: usize = 256;
const MAX_TRACKED_SELECTIONS: usize = 32;
static NEXT_EDITOR: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub focus: usize,
}
impl Selection {
    pub fn caret(offset: usize) -> Self {
        Self {
            anchor: offset,
            focus: offset,
        }
    }
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

/// A live view's source selection, owned by one editor document. This is not a
/// revision-bound authorization token for persistence or generated edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackedSelection {
    editor: u64,
    id: u64,
}

#[derive(Debug)]
struct TrackedView {
    selection: Selection,
    typing_style: Option<InlineStyle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub replacement: String,
}

#[derive(Clone, Debug)]
pub struct Transaction {
    editor: u64,
    revision: u64,
    edits: Vec<TextEdit>,
    selection: Option<EditorSelection>,
    before: Option<EditorSelection>,
}
impl Transaction {
    pub fn replace(&mut self, range: Range<usize>, text: impl Into<String>) -> Result<(), Error> {
        let replacement = text.into();
        if self.edits.len() >= MAX_EDITS
            || replacement.len() > MAX_SOURCE_BYTES
            || self
                .edits
                .iter()
                .map(|e| e.replacement.len())
                .sum::<usize>()
                .saturating_add(replacement.len())
                > MAX_SOURCE_BYTES
        {
            return Err(Error::Limit);
        }
        self.edits.push(TextEdit { range, replacement });
        Ok(())
    }
    pub fn set_selection(&mut self, selection: Selection) {
        self.selection = Some(EditorSelection::Source(selection));
    }
    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }
    pub(crate) fn before_visual(&mut self, selection: Selection) {
        self.before = Some(EditorSelection::Visual(selection, Bias::After));
    }
    pub(crate) fn before_source(&mut self, selection: Selection) {
        self.before = Some(EditorSelection::Source(selection));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorSelection {
    Source(Selection),
    Visual(Selection, Bias),
}

#[derive(Debug)]
struct Change {
    forward: Vec<TextEdit>,
    inverse: Vec<TextEdit>,
    before: EditorSelection,
    after: EditorSelection,
}
impl Change {
    fn bytes(&self) -> usize {
        self.forward
            .iter()
            .chain(&self.inverse)
            .map(|e| std::mem::size_of::<TextEdit>() + e.replacement.len())
            .sum()
    }
}

#[derive(Debug)]
pub struct EditorDocument {
    identity: u64,
    revision: u64,
    markdown: Markdown,
    projection: Projection,
    selection: EditorSelection,
    undo: VecDeque<Change>,
    redo: Vec<Change>,
    history_bytes: usize,
    pub(crate) typing_style: Option<InlineStyle>,
    admission: Option<AdmissionCheck>,
    tracked: BTreeMap<u64, TrackedView>,
    next_tracked: u64,
}

/// An adapter can impose a bounded renderer/storage admission contract without
/// making this document model depend on that adapter. Checked before publishing
/// source, selection, revision or history, including undo and redo.
pub type AdmissionCheck = fn(&str, &Projection) -> Result<(), Error>;

impl EditorDocument {
    pub fn new(source: &str, dialect: Dialect) -> Result<Self, Error> {
        let markdown = Markdown::parse(source, dialect)?;
        let projection = markdown.project()?;
        let identity = NEXT_EDITOR
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::Limit)?;
        Ok(Self {
            identity,
            revision: 0,
            markdown,
            projection,
            selection: EditorSelection::Source(Selection::default()),
            undo: VecDeque::new(),
            redo: vec![],
            history_bytes: 0,
            typing_style: None,
            admission: None,
            tracked: BTreeMap::new(),
            next_tracked: 0,
        })
    }
    pub fn source(&self) -> &str {
        self.markdown.source()
    }
    pub fn set_admission_check(&mut self, check: AdmissionCheck) -> Result<(), Error> {
        check(self.source(), self.projection())?;
        self.admission = Some(check);
        Ok(())
    }
    pub fn markdown(&self) -> &Markdown {
        &self.markdown
    }
    pub fn projection(&self) -> &Projection {
        &self.projection
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Track a view selection through successful edits, undo and redo. Insertions
    /// at an endpoint have right affinity; deleted endpoints collapse to the end
    /// of the replacement. Resulting endpoints snap to whole graphemes.
    pub fn track_selection(&mut self, selection: Selection) -> Result<TrackedSelection, Error> {
        validate_selection(self.source(), selection)?;
        if self.tracked.len() == MAX_TRACKED_SELECTIONS {
            return Err(Error::Limit);
        }
        let id = self.next_tracked;
        let next = id.checked_add(1).ok_or(Error::Limit)?;
        self.tracked.insert(
            id,
            TrackedView {
                selection,
                typing_style: None,
            },
        );
        self.next_tracked = next;
        Ok(TrackedSelection {
            editor: self.identity,
            id,
        })
    }
    pub fn tracked_selection(&self, handle: TrackedSelection) -> Result<Selection, Error> {
        if handle.editor != self.identity {
            return Err(Error::UnknownSelection);
        }
        self.tracked
            .get(&handle.id)
            .map(|view| view.selection)
            .ok_or(Error::UnknownSelection)
    }
    pub fn update_tracked_selection(
        &mut self,
        handle: TrackedSelection,
        selection: Selection,
    ) -> Result<(), Error> {
        self.tracked_selection(handle)?;
        validate_selection(self.source(), selection)?;
        self.tracked
            .get_mut(&handle.id)
            .ok_or(Error::UnknownSelection)?
            .selection = selection;
        Ok(())
    }
    /// Save and restore only already-validated, editor-owned typing marks.
    pub fn capture_typing_style(&mut self, handle: TrackedSelection) -> Result<(), Error> {
        self.tracked_selection(handle)?;
        self.tracked
            .get_mut(&handle.id)
            .ok_or(Error::UnknownSelection)?
            .typing_style
            .clone_from(&self.typing_style);
        Ok(())
    }
    pub fn restore_typing_style(&mut self, handle: TrackedSelection) -> Result<(), Error> {
        self.tracked_selection(handle)?;
        self.typing_style.clone_from(
            &self
                .tracked
                .get(&handle.id)
                .ok_or(Error::UnknownSelection)?
                .typing_style,
        );
        Ok(())
    }
    pub fn release_selection(&mut self, handle: TrackedSelection) -> Result<(), Error> {
        self.tracked_selection(handle)?;
        self.tracked.remove(&handle.id);
        Ok(())
    }
    /// Exact source coordinates for persistence/completion anchors. A visual
    /// caret inside an entity can be edited, but cannot authorize a source splice.
    pub fn source_selection(&self) -> Result<Selection, Error> {
        match self.selection {
            EditorSelection::Source(selection) => Ok(selection),
            EditorSelection::Visual(selection, bias) => Ok(Selection {
                anchor: self.projection.source_at(selection.anchor, bias)?,
                focus: self.projection.source_at(selection.focus, bias)?,
            }),
        }
    }
    pub fn visual_selection(&self) -> Result<Selection, Error> {
        match self.selection {
            EditorSelection::Visual(selection, _) => Ok(selection),
            EditorSelection::Source(selection) => Ok(Selection {
                anchor: self.projection.display_at(selection.anchor, Bias::After)?,
                focus: self.projection.display_at(selection.focus, Bias::After)?,
            }),
        }
    }
    pub fn select_visual(&mut self, selection: Selection, bias: Bias) -> Result<(), Error> {
        validate_selection(self.projection.text(), selection)?;
        self.selection = EditorSelection::Visual(selection, bias);
        self.typing_style = None;
        Ok(())
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn select(&mut self, selection: Selection) -> Result<(), Error> {
        validate_selection(self.source(), selection)?;
        self.selection = EditorSelection::Source(selection);
        self.typing_style = None;
        Ok(())
    }
    pub fn transaction(&self) -> Transaction {
        Transaction {
            editor: self.identity,
            revision: self.revision,
            edits: vec![],
            selection: None,
            before: None,
        }
    }
    pub fn replace_selection(&mut self, text: &str) -> Result<bool, Error> {
        let range = self.source_selection()?.range();
        let mut transaction = self.transaction();
        transaction.replace(range.clone(), text)?;
        transaction.set_selection(Selection::caret(range.start.saturating_add(text.len())));
        self.apply(transaction)
    }

    /// Validate, parse and project a complete candidate before changing any live
    /// state. A failed transaction preserves source, selection and both histories.
    pub fn apply(&mut self, transaction: Transaction) -> Result<bool, Error> {
        self.apply_checked(transaction, |_, _| Ok(None))
    }
    pub(crate) fn apply_checked(
        &mut self,
        mut transaction: Transaction,
        check: impl FnOnce(&Markdown, &Projection) -> Result<Option<EditorSelection>, Error>,
    ) -> Result<bool, Error> {
        if transaction.editor != self.identity || transaction.revision != self.revision {
            return Err(Error::StaleTransaction);
        }
        if let Some(before) = transaction.before {
            validate_editor_selection(self.source(), &self.projection, before)?;
        }
        transaction
            .edits
            .sort_by_key(|edit| (edit.range.start, edit.range.end));
        let mut prepared = self.prepare(&transaction.edits, transaction.selection, true)?;
        if let Some(selection) = check(&prepared.markdown, &prepared.projection)? {
            validate_editor_selection(prepared.markdown.source(), &prepared.projection, selection)?;
            prepared.selection = selection;
        }
        let changed = prepared.markdown.source() != self.source();
        if !changed {
            self.selection = prepared.selection;
            return Ok(false);
        }
        let revision = self.revision.checked_add(1).ok_or(Error::Limit)?;
        let change = Change {
            forward: transaction.edits,
            inverse: prepared.inverse,
            before: transaction.before.unwrap_or(self.selection),
            after: prepared.selection,
        };
        self.history_bytes -= self.redo.iter().map(Change::bytes).sum::<usize>();
        self.redo.clear();
        self.history_bytes += change.bytes();
        self.undo.push_back(change);
        while self.history_bytes > MAX_HISTORY_BYTES || self.undo.len() > MAX_HISTORY_RECORDS {
            if let Some(old) = self.undo.pop_front() {
                self.history_bytes -= old.bytes();
            } else {
                break;
            }
        }
        self.markdown = prepared.markdown;
        self.projection = prepared.projection;
        self.selection = prepared.selection;
        self.tracked = prepared.tracked;
        self.revision = revision;
        Ok(true)
    }

    pub fn undo(&mut self) -> Result<bool, Error> {
        self.restore_history(false)
    }
    pub fn redo(&mut self) -> Result<bool, Error> {
        self.restore_history(true)
    }

    fn restore_history(&mut self, redo: bool) -> Result<bool, Error> {
        let Some(change) = (if redo {
            self.redo.last()
        } else {
            self.undo.back()
        }) else {
            return Ok(false);
        };
        let (edits, selection) = if redo {
            (&change.forward, change.after)
        } else {
            (&change.inverse, change.before)
        };
        // An edit can merge neighbouring graphemes (for example a combining
        // mark). Its inverse restores exact bytes and may cut that new cluster.
        let prepared = self.prepare(edits, Some(selection), false)?;
        let revision = self.revision.checked_add(1).ok_or(Error::Limit)?;
        let change = if redo {
            self.redo.pop()
        } else {
            self.undo.pop_back()
        }
        .ok_or(Error::InvalidParse)?;
        self.markdown = prepared.markdown;
        self.projection = prepared.projection;
        self.selection = prepared.selection;
        self.tracked = prepared.tracked;
        self.revision = revision;
        if redo {
            self.undo.push_back(change);
        } else {
            self.redo.push(change);
        }
        self.typing_style = None;
        Ok(true)
    }

    fn prepare(
        &self,
        edits: &[TextEdit],
        selection: Option<EditorSelection>,
        require_graphemes: bool,
    ) -> Result<PreparedChange, Error> {
        let source = self.source();
        let boundaries = source
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([source.len()])
            .collect::<Vec<_>>();
        let mut source_len = source.len();
        let mut previous: Option<&TextEdit> = None;
        for edit in edits {
            if edit.range.start > edit.range.end
                || !source.is_char_boundary(edit.range.start)
                || !source.is_char_boundary(edit.range.end)
                || (require_graphemes
                    && (boundaries.binary_search(&edit.range.start).is_err()
                        || boundaries.binary_search(&edit.range.end).is_err()))
            {
                return Err(Error::InvalidRange);
            }
            if previous.is_some_and(|p| {
                edit.range.start < p.range.end || edit.range.start == p.range.start
            }) {
                return Err(Error::OverlappingEdits);
            }
            source_len = source_len
                .checked_sub(edit.range.len())
                .and_then(|n| n.checked_add(edit.replacement.len()))
                .ok_or(Error::Limit)?;
            previous = Some(edit);
        }
        if source_len > MAX_SOURCE_BYTES {
            return Err(Error::Limit);
        }
        let mut candidate = String::with_capacity(source_len);
        let mut inverse: Vec<TextEdit> = Vec::with_capacity(edits.len());
        let mut end = 0;
        for edit in edits {
            candidate.push_str(&source[end..edit.range.start]);
            let start = candidate.len();
            candidate.push_str(&edit.replacement);
            if let Some(previous) = inverse.last_mut()
                && previous.range.end == start
            {
                previous.range.end = candidate.len();
                previous.replacement.push_str(&source[edit.range.clone()]);
            } else {
                inverse.push(TextEdit {
                    range: start..candidate.len(),
                    replacement: source[edit.range.clone()].into(),
                });
            }
            end = edit.range.end;
        }
        candidate.push_str(&source[end..]);
        let markdown = Markdown::parse(&candidate, self.markdown.dialect())?;
        let projection = markdown.project()?;
        if let Some(check) = self.admission {
            check(markdown.source(), &projection)?;
        }
        let selection = match selection {
            Some(EditorSelection::Source(selection)) => {
                EditorSelection::Source(snap_selection(&candidate, selection)?)
            }
            Some(visual @ EditorSelection::Visual(_, _)) => visual,
            None => {
                let before = self.source_selection()?;
                EditorSelection::Source(snap_selection(
                    &candidate,
                    Selection {
                        anchor: map_position(before.anchor, edits),
                        focus: map_position(before.focus, edits),
                    },
                )?)
            }
        };
        validate_editor_selection(&candidate, &projection, selection)?;
        let tracked = self.tracked_after(&candidate, edits)?;
        Ok(PreparedChange {
            markdown,
            projection,
            inverse,
            selection,
            tracked,
        })
    }

    fn tracked_after(
        &self,
        candidate: &str,
        edits: &[TextEdit],
    ) -> Result<BTreeMap<u64, TrackedView>, Error> {
        // All pane endpoints share one boundary index for this edit. Repeatedly
        // scanning a long manuscript per view would make split panes expensive.
        let tracked_boundaries = if self.tracked.is_empty() {
            Vec::new()
        } else {
            candidate
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([candidate.len()])
                .collect()
        };
        self.tracked
            .iter()
            .map(|(&id, view)| {
                snap_selection_with_boundaries(
                    candidate,
                    Selection {
                        anchor: map_position(view.selection.anchor, edits),
                        focus: map_position(view.selection.focus, edits),
                    },
                    &tracked_boundaries,
                )
                .map(|selection| {
                    (
                        id,
                        TrackedView {
                            selection,
                            typing_style: view.typing_style.clone(),
                        },
                    )
                })
            })
            .collect()
    }
}

struct PreparedChange {
    markdown: Markdown,
    projection: Projection,
    inverse: Vec<TextEdit>,
    selection: EditorSelection,
    tracked: BTreeMap<u64, TrackedView>,
}

fn map_position(position: usize, edits: &[TextEdit]) -> usize {
    let mut old_end = 0;
    let mut new_end = 0;
    for edit in edits {
        if position < edit.range.start {
            break;
        }
        new_end += edit.range.start - old_end + edit.replacement.len();
        old_end = edit.range.end;
        if position <= old_end {
            return new_end;
        }
    }
    new_end + position - old_end
}

fn validate_selection(source: &str, selection: Selection) -> Result<(), Error> {
    let mut anchor = false;
    let mut focus = false;
    for offset in source
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([source.len()])
    {
        anchor |= offset == selection.anchor;
        focus |= offset == selection.focus;
    }
    if anchor && focus {
        Ok(())
    } else {
        Err(Error::InvalidRange)
    }
}

fn validate_editor_selection(
    source: &str,
    projection: &Projection,
    selection: EditorSelection,
) -> Result<(), Error> {
    match selection {
        EditorSelection::Source(selection) => validate_selection(source, selection),
        EditorSelection::Visual(selection, _) => validate_selection(projection.text(), selection),
    }
}

fn snap_selection(source: &str, selection: Selection) -> Result<Selection, Error> {
    let boundaries = source
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([source.len()])
        .collect::<Vec<_>>();
    snap_selection_with_boundaries(source, selection, &boundaries)
}

fn snap_selection_with_boundaries(
    source: &str,
    selection: Selection,
    boundaries: &[usize],
) -> Result<Selection, Error> {
    if !source.is_char_boundary(selection.anchor) || !source.is_char_boundary(selection.focus) {
        return Err(Error::InvalidRange);
    }
    let snap = |offset| boundaries[boundaries.partition_point(|&i| i < offset)];
    Ok(Selection {
        anchor: snap(selection.anchor),
        focus: snap(selection.focus),
    })
}
