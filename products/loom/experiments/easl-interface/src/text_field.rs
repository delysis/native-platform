//! Native view adapter. Markdown owns source/history; Parley owns geometry.
//! EASL resolves rich view styles, pointer hits and selection/replacement policy.
use easl_native_text::{EditCommand, StyledSpan, TextEditor, TextStyle, TextSystem};
use easl_text::{
    EditAction, EditPlan, Editing, HitTesting, LineMovement, Navigation, StyleInput,
    StyleProperties, StyleRule, Styling,
};
use loom_markdown::{
    Bias, BlockStyle, Dialect, EditorDocument, InlineFormat, ListIndent, ParagraphFormat,
    Selection, StructureFormat,
};
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct FormattingCapture {
    view: u32,
    selection: loom_markdown::FormattingSelection,
}

#[derive(Debug)]
struct CachedStyles {
    revision: u64,
    source_mode: bool,
    spans: Vec<StyledSpan>,
}

#[derive(Debug)]
struct StoredView {
    editor: TextEditor,
    source_mode: bool,
    composing: bool,
    composition_selection: Option<Selection>,
    body: TextStyle,
    roles: Vec<(u32, TextStyle)>,
    paragraph_roles: Vec<(u32, crate::interface::ParagraphStyle)>,
    selection: loom_markdown::TrackedSelection,
    revision: u64,
    captured_selection: Selection,
    styles: Option<CachedStyles>,
}

#[derive(Debug)]
pub struct TextField {
    pub editor: TextEditor,
    pub saved: String,
    editing: Editing,
    hit_testing: HitTesting,
    navigation: Navigation,
    styling: Styling,
    styles: Option<CachedStyles>,
    document: EditorDocument,
    source_mode: bool,
    composing: bool,
    composition_selection: Option<Selection>,
    body: TextStyle,
    roles: Vec<(u32, TextStyle)>,
    paragraph_roles: Vec<(u32, crate::interface::ParagraphStyle)>,
    active_view: u32,
    selection: loom_markdown::TrackedSelection,
    view_revision: u64,
    captured_selection: Selection,
    views: BTreeMap<u32, StoredView>,
}

impl TextField {
    pub fn new(text: &str, serif: bool) -> Result<Self, String> {
        Self::with_kind(text, serif, false)
    }
    pub fn with_kind(text: &str, serif: bool, verse: bool) -> Result<Self, String> {
        let body = TextStyle {
            family: if serif {
                "Baskerville, Georgia, serif"
            } else {
                "Helvetica Neue, sans-serif"
            }
            .into(),
            ..TextStyle::default()
        };
        let mut document = EditorDocument::new(
            text,
            if verse {
                Dialect::PlainText
            } else {
                Dialect::Loom
            },
        )
        .map_err(|e| e.to_string())?;
        document
            .set_admission_check(|source, projection| {
                if projection.spans().len() + 1 > easl_native_text::MAX_EDITOR_SPANS {
                    return Err(loom_markdown::Error::Limit);
                }
                easl_native_text::validate_text(source)
                    .and_then(|()| easl_native_text::validate_text(projection.text()))
                    .map_err(|_| loom_markdown::Error::Limit)
            })
            .map_err(|e| e.to_string())?;
        let editor = TextEditor::new(document.projection().text(), body.clone())
            .map_err(|e| e.to_string())?;
        let selection = document
            .track_selection(Selection::default())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            editor,
            saved: text.into(),
            editing: Editing::new().map_err(|error| error.to_string())?,
            hit_testing: HitTesting::new().map_err(|error| error.to_string())?,
            navigation: Navigation::new().map_err(|error| error.to_string())?,
            styling: Styling::new().map_err(|error| error.to_string())?,
            styles: None,
            document,
            source_mode: false,
            composing: false,
            composition_selection: None,
            body,
            roles: vec![],
            paragraph_roles: vec![],
            active_view: 1,
            selection,
            view_revision: 0,
            captured_selection: Selection::default(),
            views: BTreeMap::new(),
        })
    }
    /// A transient literal UI field uses the same EASL edit policy and atomic
    /// history as document views, without registering with persistence.
    pub fn single_line<const LIMIT: usize>(text: &str) -> Result<Self, String> {
        let mut field = Self::with_kind(text, false, true)?;
        field
            .document
            .set_admission_check(|source, _| {
                if source.len() > LIMIT {
                    return Err(loom_markdown::Error::Limit);
                }
                if source.contains(['\r', '\n', '\u{2028}', '\u{2029}']) {
                    return Err(loom_markdown::Error::UnsupportedEdit);
                }
                easl_native_text::validate_text(source).map_err(|_| loom_markdown::Error::Limit)
            })
            .map_err(|e| e.to_string())?;
        Ok(field)
    }
    /// Each pane retains its geometry, scroll, mode and caret while the document
    /// and undo history stay singular. Switching a view never edits the source.
    pub fn activate_view(&mut self, id: u32, system: &mut TextSystem) -> Result<(), String> {
        if id == self.active_view {
            return Ok(());
        }
        if !(1..=8).contains(&id) {
            return Err("Unknown document view".into());
        }
        if !self.composing {
            self.capture_view_selection()?;
        }
        self.document
            .capture_typing_style(self.selection)
            .map_err(|e| e.to_string())?;
        let mut next = match self.views.remove(&id) {
            Some(view) => view,
            None => StoredView {
                editor: TextEditor::new(self.document.projection().text(), self.body.clone())
                    .map_err(|e| e.to_string())?,
                source_mode: false,
                composing: false,
                composition_selection: None,
                body: self.body.clone(),
                roles: self.roles.clone(),
                paragraph_roles: self.paragraph_roles.clone(),
                selection: self
                    .document
                    .track_selection(Selection::default())
                    .map_err(|e| e.to_string())?,
                revision: u64::MAX,
                captured_selection: Selection::default(),
                styles: None,
            },
        };
        std::mem::swap(&mut self.editor, &mut next.editor);
        std::mem::swap(&mut self.source_mode, &mut next.source_mode);
        std::mem::swap(&mut self.composing, &mut next.composing);
        std::mem::swap(
            &mut self.composition_selection,
            &mut next.composition_selection,
        );
        std::mem::swap(&mut self.body, &mut next.body);
        std::mem::swap(&mut self.roles, &mut next.roles);
        std::mem::swap(&mut self.styles, &mut next.styles);
        std::mem::swap(&mut self.paragraph_roles, &mut next.paragraph_roles);
        std::mem::swap(&mut self.selection, &mut next.selection);
        std::mem::swap(&mut self.view_revision, &mut next.revision);
        std::mem::swap(&mut self.captured_selection, &mut next.captured_selection);
        self.views.insert(self.active_view, next);
        self.active_view = id;
        if let Some(selection) = self.composition_selection {
            if self.source_mode {
                self.document.select(selection)
            } else {
                self.document.select_visual(selection, Bias::After)
            }
            .map_err(|e| e.to_string())?;
        }
        if self.view_revision != self.document.revision() && !self.composing {
            let selection = self
                .document
                .tracked_selection(self.selection)
                .map_err(|e| e.to_string())?;
            self.document.select(selection).map_err(|e| e.to_string())?;
            let reveal_caret = self.editor.reveal_caret;
            self.sync(system)?;
            self.editor.reveal_caret = reveal_caret;
        } else if !self.composing {
            // A renderer may visit sibling panes without changing the document.
            // Restore this view's selection before restoring its typing marks;
            // a later selection repair would otherwise clear those marks.
            self.remember_selection()?;
        }
        self.document
            .restore_typing_style(self.selection)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn capture_view_selection(&mut self) -> Result<(), String> {
        if self.captured_selection == self.widget_selection() {
            return Ok(());
        }
        self.remember_selection()?;
        let selection = self.source_selection_for_view()?;
        self.document
            .update_tracked_selection(self.selection, selection)
            .map_err(|e| e.to_string())?;
        self.captured_selection = self.widget_selection();
        Ok(())
    }
    fn check_other_composition(&self) -> Result<(), String> {
        if self.views.values().any(|view| view.composing) {
            Err("Finish text composition in the other pane before editing".into())
        } else {
            Ok(())
        }
    }
    pub fn source(&self) -> &str {
        self.document.source()
    }
    pub fn text(&self) -> String {
        self.document.source().into()
    }
    pub fn dirty(&self) -> bool {
        self.document.source() != self.saved
    }
    pub fn revision(&self) -> u64 {
        self.document.revision()
    }
    pub fn is_composing(&self) -> bool {
        self.composing || self.views.values().any(|view| view.composing)
    }
    pub fn source_mode(&self) -> bool {
        self.source_mode
    }
    pub fn view_source_mode(&self, id: u32) -> bool {
        if id == self.active_view {
            self.source_mode
        } else {
            self.views.get(&id).is_some_and(|view| view.source_mode)
        }
    }
    pub fn verse(&self) -> bool {
        self.document.markdown().dialect() == Dialect::PlainText
    }
    fn widget_selection(&self) -> Selection {
        let (anchor, focus) = self.editor.selection_bytes();
        Selection { anchor, focus }
    }
    fn remember_selection(&mut self) -> Result<(), String> {
        let selection = self.widget_selection();
        if self.source_mode {
            if self.document.source_selection() != Ok(selection) {
                self.document.select(selection).map_err(|e| e.to_string())?;
            }
        } else if self.document.visual_selection() != Ok(selection) {
            self.document
                .select_visual(selection, Bias::After)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn source_selection_for_view(&self) -> Result<Selection, String> {
        if let Ok(selection) = self.document.source_selection() {
            return Ok(selection);
        }
        let visual = self
            .document
            .visual_selection()
            .map_err(|e| e.to_string())?;
        let edge = |offset| {
            self.document
                .projection()
                .source_at(offset, Bias::Before)
                .or_else(|error| {
                    if error != loom_markdown::Error::AmbiguousBoundary {
                        return Err(error);
                    }
                    self.document
                        .projection()
                        .spans()
                        .iter()
                        .find(|s| s.display.start < offset && offset < s.display.end)
                        .map(|s| {
                            if offset - s.display.start <= s.display.end - offset {
                                s.source.start
                            } else {
                                s.source.end
                            }
                        })
                        .ok_or(error)
                })
                .map_err(|e| e.to_string())
        };
        // Switching to source visibly places the caret at an entity edge. This
        // navigation policy is never used to authorize a completion splice.
        Ok(Selection {
            anchor: edge(visual.anchor)?,
            focus: edge(visual.focus)?,
        })
    }
    pub fn toggle_source(&mut self, system: &mut TextSystem) -> Result<(), String> {
        if self.verse() {
            return Ok(());
        }
        self.cancel_composition(system)?;
        self.remember_selection()?;
        self.source_mode = !self.source_mode;
        self.sync(system)
    }
    pub fn capture_formatting(&mut self) -> Result<FormattingCapture, String> {
        if self.source_mode || self.verse() || self.is_composing() {
            return Err("Formatting requires a Visual selection outside text composition".into());
        }
        self.remember_selection()?;
        Ok(FormattingCapture {
            view: self.active_view,
            selection: self
                .document
                .capture_formatting_selection(self.widget_selection())
                .map_err(|error| error.to_string())?,
        })
    }
    pub fn formatting_state(
        &self,
        capture: &FormattingCapture,
    ) -> Result<loom_markdown::FormattingState, String> {
        self.validate_formatting(capture)?;
        self.document
            .formatting_state(&capture.selection)
            .map_err(|error| error.to_string())
    }
    fn validate_formatting(&self, capture: &FormattingCapture) -> Result<(), String> {
        if capture.view != self.active_view || self.source_mode || self.is_composing() {
            return Err("The formatting selection no longer belongs to this Visual view".into());
        }
        self.document
            .validate_formatting_selection(&capture.selection)
            .map_err(|error| error.to_string())
    }
    pub fn refresh_formatting(
        &mut self,
        capture: &mut FormattingCapture,
        editor_focused: bool,
    ) -> Result<bool, String> {
        self.validate_formatting(capture)?;
        if editor_focused && self.widget_selection() != capture.selection.selection() {
            *capture = self.capture_formatting()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn format_captured(
        &mut self,
        system: &mut TextSystem,
        capture: &mut FormattingCapture,
        action: u32,
    ) -> Result<(), String> {
        use loom_markdown::FormattingCommand as Command;
        let command = match action {
            0 => Command::Paragraph(ParagraphFormat::Body),
            1..=3 => Command::Paragraph(ParagraphFormat::Heading(
                u8::try_from(action).map_err(|e| e.to_string())?,
            )),
            4 => Command::Inline(InlineFormat::Bold),
            5 => Command::Inline(InlineFormat::Italic),
            6 => Command::Structure(StructureFormat::Quote),
            7 => Command::Structure(StructureFormat::BulletList),
            8 => Command::Structure(StructureFormat::OrderedList),
            _ => return Err("Unknown formatting action".into()),
        };
        self.apply_formatting(system, capture, &command)
    }
    pub fn apply_formatting(
        &mut self,
        system: &mut TextSystem,
        capture: &mut FormattingCapture,
        command: &loom_markdown::FormattingCommand,
    ) -> Result<(), String> {
        self.validate_formatting(capture)?;
        self.document
            .apply_formatting(&mut capture.selection, command)
            .map_err(|error| error.to_string())?;
        self.sync(system)
    }
    pub fn format(&mut self, system: &mut TextSystem, action: u32) -> Result<(), String> {
        let mut capture = self.capture_formatting()?;
        self.format_captured(system, &mut capture, action)
    }
    pub fn indent(
        &mut self,
        system: &mut TextSystem,
        direction: ListIndent,
    ) -> Result<bool, String> {
        self.check_other_composition()?;
        if self.source_mode || self.verse() {
            if direction == ListIndent::Indent {
                return Ok(false);
            }
            self.cancel_composition(system)?;
            self.remember_selection()?;
            let handled = self
                .document
                .outdent_source(self.widget_selection())
                .map_err(|error| error.to_string())?;
            if handled {
                self.sync(system)?;
            }
            return Ok(handled);
        }
        self.cancel_composition(system)?;
        self.remember_selection()?;
        let handled = self
            .document
            .indent_list(self.widget_selection(), direction)
            .map_err(|e| e.to_string())?;
        if handled {
            self.sync(system)?;
        }
        Ok(handled)
    }
    pub fn select_accessible(
        &mut self,
        system: &mut TextSystem,
        selection: &accesskit::TextSelection,
    ) -> Result<(), String> {
        self.cancel_composition(system)?;
        self.editor.select_accessible(system, selection);
        self.remember_selection()
    }
    pub fn set_accessible_value(
        &mut self,
        system: &mut TextSystem,
        value: &str,
    ) -> Result<(), String> {
        self.check_other_composition()?;
        self.cancel_composition(system)?;
        self.remember_selection()?;
        if self.source_mode {
            let mut transaction = self.document.transaction();
            transaction
                .replace(0..self.document.source().len(), value)
                .map_err(|e| e.to_string())?;
            transaction.set_selection(Selection::caret(value.len()));
            self.document
                .apply(transaction)
                .map_err(|e| e.to_string())?;
        } else {
            let all = Selection {
                anchor: 0,
                focus: self.document.projection().text().len(),
            };
            self.document
                .replace_visual(all, value)
                .map_err(|e| e.to_string())?;
        }
        self.sync(system)
    }
    fn replacement_plan<'a>(
        &mut self,
        system: &mut TextSystem,
        command: &'a EditCommand,
    ) -> Result<Option<(EditPlan, &'a str)>, String> {
        let (action, text) = match command {
            EditCommand::Insert(text) | EditCommand::Paste(text) | EditCommand::Commit(text) => {
                (EditAction::Replace(text), text.as_str())
            }
            EditCommand::Backspace => (EditAction::Backspace, ""),
            EditCommand::BackspaceWord => (EditAction::BackspaceWord, ""),
            EditCommand::Delete => (EditAction::Delete, ""),
            EditCommand::DeleteWord => (EditAction::DeleteWord, ""),
            EditCommand::DeleteSelection => (EditAction::DeleteSelection, ""),
            _ => return Ok(None),
        };
        self.editing
            .plan_for(&mut self.editor, system, action)
            .map(|plan| Some((plan, text)))
            .map_err(|error| error.to_string())
    }

    fn navigate(
        &mut self,
        system: &mut TextSystem,
        movement: easl_native_text::Movement,
        extend: bool,
    ) -> Result<(), String> {
        if let Some(movement) = LineMovement::from_movement(movement) {
            self.navigation
                .apply(
                    &mut self.editor,
                    system,
                    &mut self.hit_testing,
                    movement,
                    extend,
                )
                .map_err(|error| error.to_string())?;
        } else {
            self.editor
                .command(system, EditCommand::Move(movement, extend))
                .map_err(|error| error.to_string())?;
        }
        self.remember_selection()
    }

    pub fn command(&mut self, system: &mut TextSystem, command: EditCommand) -> Result<(), String> {
        self.check_other_composition()?;
        if matches!(command, EditCommand::Preedit(_, _)) {
            if !self.composing {
                self.remember_selection()?;
                self.capture_view_selection()?;
                self.composition_selection = Some(self.widget_selection());
            }
            self.editor
                .command(system, command)
                .map_err(|e| e.to_string())?;
            self.composing = true;
            return Ok(());
        }
        self.cancel_composition(system)?;
        if matches!(command, EditCommand::CancelCompose) {
            return Ok(());
        }
        if matches!(&command, EditCommand::Insert(text) if text == "\t")
            && self.indent(system, ListIndent::Indent)?
        {
            return Ok(());
        }
        let selection = self.widget_selection();
        if matches!(command, EditCommand::Backspace | EditCommand::Delete) && !self.source_mode {
            let direction = if matches!(command, EditCommand::Backspace) {
                loom_markdown::DeleteDirection::Backward
            } else {
                loom_markdown::DeleteDirection::Forward
            };
            if self
                .document
                .delete_boundary(selection, direction)
                .map_err(|e| e.to_string())?
            {
                return self.sync(system);
            }
        }
        let replacement = self.replacement_plan(system, &command)?;
        if let Some((plan, text)) = replacement {
            let range = plan
                .replacement()
                .ok_or("EASL edit did not return a replacement")?;
            let result = if self.source_mode {
                self.document.select(selection).map_err(|e| e.to_string())?;
                let mut transaction = self.document.transaction();
                transaction
                    .replace(range.clone(), text)
                    .map_err(|e| e.to_string())?;
                let (anchor, focus) = plan.selection();
                transaction.set_selection(Selection { anchor, focus });
                self.document.apply(transaction)
            } else if matches!(command, EditCommand::Insert(_) | EditCommand::Commit(_)) {
                self.document.type_visual(selection, text)
            } else if matches!(command, EditCommand::Paste(_)) {
                self.document.replace_visual(selection, text)
            } else {
                self.document.delete_visual(selection, range)
            };
            result.map_err(|e| e.to_string())?;
            return self.sync(system);
        }
        match command {
            EditCommand::Move(movement, extend) => self.navigate(system, movement, extend),
            EditCommand::Click(x, y, _, _) | EditCommand::Drag(x, y) => {
                let (count, extend) = match command {
                    EditCommand::Click(_, _, count, extend) => (count.max(1), extend),
                    _ => (0, true),
                };
                let hit = self
                    .hit_testing
                    .hit_editor(&mut self.editor, system, [x, y])
                    .map_err(|error| error.to_string())?;
                self.editor
                    .select_pointer_target(system, hit.byte, hit.affinity, count, extend)
                    .map_err(|error| error.to_string())?;
                self.remember_selection()
            }
            EditCommand::SelectAll => {
                self.editing
                    .apply(&mut self.editor, system, EditAction::SelectAll)
                    .map_err(|error| error.to_string())?;
                self.remember_selection()
            }
            EditCommand::Undo => {
                self.document.undo().map_err(|e| e.to_string())?;
                self.sync(system)
            }
            EditCommand::Redo => {
                self.document.redo().map_err(|e| e.to_string())?;
                self.sync(system)
            }
            _ => {
                self.editor
                    .command(system, command)
                    .map_err(|e| e.to_string())?;
                self.remember_selection()
            }
        }
    }
    fn cancel_composition(&mut self, system: &mut TextSystem) -> Result<(), String> {
        if self.composing {
            self.editor
                .command(system, EditCommand::CancelCompose)
                .map_err(|e| e.to_string())?;
            self.composing = false;
            self.composition_selection = None;
            self.sync(system)?;
        }
        Ok(())
    }
    fn sync(&mut self, system: &mut TextSystem) -> Result<(), String> {
        self.refresh_styles().map_err(|error| error.to_string())?;
        let selection = if self.source_mode {
            self.source_selection_for_view()?
        } else {
            self.document
                .visual_selection()
                .map_err(|e| e.to_string())?
        };
        let text = if self.source_mode {
            self.document.source()
        } else {
            self.document.projection().text()
        };
        self.editor
            .set_document_projection(
                system,
                text,
                &self
                    .styles
                    .as_ref()
                    .ok_or("Missing resolved view styles")?
                    .spans,
                &self.paragraphs(),
                (selection.anchor, selection.focus),
            )
            .map_err(|e| e.to_string())?;
        self.view_revision = self.document.revision();
        self.captured_selection = self.widget_selection();
        let selection = self.source_selection_for_view()?;
        self.document
            .update_tracked_selection(self.selection, selection)
            .map_err(|e| e.to_string())
    }
    pub fn blocks(&self) -> &[loom_markdown::VisualBlock] {
        if self.source_mode || self.verse() {
            &[]
        } else {
            self.document.projection().blocks()
        }
    }
    pub fn paragraph_role(&self, id: u32) -> crate::interface::ParagraphStyle {
        self.paragraph_roles
            .iter()
            .find(|(key, _)| *key == id)
            .map_or(crate::interface::ParagraphStyle::default(), |(_, style)| {
                *style
            })
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "Markdown limits the combined nesting depth to 128"
    )]
    fn paragraphs(&self) -> Vec<easl_native_text::ParagraphLayout> {
        self.blocks()
            .iter()
            .enumerate()
            .map(|(index, block)| {
                let role = match block.style {
                    BlockStyle::Heading(level) => 9 + u32::from(level),
                    BlockStyle::Code(_) | BlockStyle::Raw => 20,
                    _ => 1,
                };
                let mut p = self.paragraph_role(role).at(block.display.start);
                let list = self.paragraph_role(30);
                let quote = self.paragraph_role(31);
                p.inset_left += block.list_depth as f32 * list.inset_left
                    + block.quote_depth as f32 * quote.inset_left;
                p.inset_right += block.quote_depth as f32 * quote.inset_right;
                if block.list_depth > 0 {
                    p.space_after = list.space_after;
                }
                if index == 0 {
                    p.space_before = 0.;
                }
                p
            })
            .collect()
    }
    /// Markdown supplies semantic ranges; shared EASL policy combines and
    /// coalesces only the view properties. Cache per view/revision and invalidate
    /// on theme/role changes, so resizing and caret repaint do not rerun the VM.
    fn refresh_styles(&mut self) -> Result<(), easl_text::Error> {
        let revision = self.document.revision();
        if self.styles.as_ref().is_some_and(|cache| {
            cache.revision == revision && cache.source_mode == self.source_mode
        }) {
            return Ok(());
        }
        let mut styles = vec![self.body.clone()];
        let mut ids = BTreeMap::new();
        for (id, style) in &self.roles {
            ids.entry(*id)
                .or_insert(u32::try_from(styles.len()).map_err(|_| easl_text::Error::Limit)?);
            styles.push(style.clone());
        }
        let role = |id| ids.get(&id).copied().unwrap_or(0);
        let mut invisible = self.body.clone();
        invisible.color[3] = 0;
        let invisible_id = u32::try_from(styles.len()).map_err(|_| easl_text::Error::Limit)?;
        styles.push(invisible);
        // Semantic flags and the current role table belong to this adapter.
        // Replacement order, property isolation and coalescing belong to EASL.
        let rules = [
            (1, StyleProperties::COLOR, 0),
            (2, StyleProperties::COLOR, invisible_id),
            (4, StyleProperties::FAMILY | StyleProperties::SIZE, role(20)),
            (8, StyleProperties::WEIGHT, role(22)),
            (16, StyleProperties::ITALIC, role(23)),
            (32, StyleProperties::UNDERLINE, role(21)),
        ]
        .map(|(flag, properties, style)| StyleRule {
            flag,
            properties,
            style,
        });
        let block_style = |style: &BlockStyle| match style {
            BlockStyle::Heading(level) => role(9 + u32::from(*level)),
            BlockStyle::Code(_) | BlockStyle::Raw => role(20),
            _ => 0,
        };
        let projection = self.document.projection();
        let inputs = if self.source_mode {
            vec![StyleInput {
                range: 0..self.document.source().len(),
                style: role(24),
                flags: 0,
            }]
        } else {
            let blocks = projection.blocks();
            let mut block_index = 0;
            let mut inputs = Vec::with_capacity(projection.spans().len() + 1);
            for span in projection.spans() {
                while blocks
                    .get(block_index + 1)
                    .is_some_and(|b| b.display.start <= span.display.start)
                {
                    block_index += 1;
                }
                let block = blocks.get(block_index);
                inputs.push(StyleInput {
                    range: span.display.clone(),
                    style: block.map_or(0, |b| block_style(&b.style)),
                    flags: 1
                        | (u32::from(block.is_some_and(|b| b.style == BlockStyle::Rule)) << 1)
                        | (u32::from(span.style.code) << 2)
                        | (u32::from(span.style.bold) << 3)
                        | (u32::from(span.style.italic) << 4)
                        | (u32::from(span.style.link.is_some()) << 5),
                });
            }
            if let Some(block) = blocks.last().filter(|block| block.display.is_empty()) {
                inputs.push(StyleInput {
                    range: block.display.clone(),
                    style: block_style(&block.style),
                    flags: 1,
                });
            }
            inputs
        };
        let source = if self.source_mode {
            self.document.source()
        } else {
            projection.text()
        };
        let spans = self.styling.resolve(source, &styles, &rules, &inputs)?;
        self.styles = Some(CachedStyles {
            revision,
            source_mode: self.source_mode,
            spans,
        });
        Ok(())
    }
    pub fn ensure_layout(
        &mut self,
        system: &mut TextSystem,
        body: &TextStyle,
        scene: &crate::interface::Scene,
        width: f32,
        height: f32,
    ) -> Result<(), easl_native_text::Error> {
        if self.body != *body || self.roles != scene.typography {
            self.body = body.clone();
            self.roles.clone_from(&scene.typography);
            self.styles = None;
        }
        self.paragraph_roles.clone_from(&scene.paragraphs);
        if !self.composing {
            self.refresh_styles().map_err(|error| match error {
                easl_text::Error::Native(error) => error,
                easl_text::Error::Limit => easl_native_text::Error::Limit,
                _ => easl_native_text::Error::InvalidStyle,
            })?;
            self.editor.set_spans(
                &self
                    .styles
                    .as_ref()
                    .ok_or(easl_native_text::Error::InvalidStyle)?
                    .spans,
            )?;
            self.editor.set_paragraphs(&self.paragraphs())?;
        }
        self.editor.ensure_layout(system, body, width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easl_native_text::Movement;

    #[test]
    fn loom_uses_shared_easl_edit_policy_with_its_own_markdown_history() {
        let mut field = TextField::with_kind("café\r\n", true, true).unwrap();
        let mut system = TextSystem::new();
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        field
            .command(&mut system, EditCommand::Insert("🌍".into()))
            .unwrap();
        field.command(&mut system, EditCommand::Backspace).unwrap();
        assert_eq!(field.editing.decisions, 3);
        assert_eq!(field.editor.text(), "");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.editor.text(), "🌍");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "café\r\n");
    }

    #[test]
    fn loom_pointer_hits_run_in_easl_and_edit_through_markdown_history() {
        let source = "**alpha** beta\r\n\r\nlast";
        let mut field = TextField::new(source, true).unwrap();
        let mut system = TextSystem::new();
        field.sync(&mut system).unwrap();
        let style = field.body.clone();
        field
            .editor
            .ensure_layout(&mut system, &style, 220., 100.)
            .unwrap();
        let layout = field.editor.inner().try_layout().unwrap();
        let rect = easl_native_text::parley::Cursor::from_byte_index(
            layout,
            6,
            easl_native_text::parley::Affinity::Downstream,
        )
        .geometry(layout, 1.);
        // Parley expands its f32 layout coordinates to f64 for rectangles.
        #[allow(clippy::cast_possible_truncation)]
        let point = [rect.x0 as f32 + 0.125, ((rect.y0 + rect.y1) * 0.5) as f32];
        field
            .command(
                &mut system,
                EditCommand::Click(point[0], point[1], 1, false),
            )
            .unwrap();
        assert_eq!(field.editor.selection_bytes(), (6, 6));
        assert_eq!(field.hit_testing.decisions, 1);
        assert_eq!(field.text(), source);
        field
            .command(&mut system, EditCommand::Insert("Z".into()))
            .unwrap();
        assert_eq!(field.text(), "**alpha** Zbeta\r\n\r\nlast");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);

        // Query state is shared, while each pane's selection remains independent.
        let first = field.editor.selection_bytes();
        field.activate_view(2, &mut system).unwrap();
        field
            .editor
            .ensure_layout(&mut system, &style, 110., 100.)
            .unwrap();
        field
            .command(&mut system, EditCommand::Click(-10., -10., 1, false))
            .unwrap();
        field
            .command(&mut system, EditCommand::Drag(500., 500.))
            .unwrap();
        let second = field.editor.selection_bytes();
        assert_eq!(second.0, 0);
        assert_eq!(second.1, field.editor.text().len());
        field.activate_view(1, &mut system).unwrap();
        assert_eq!(field.editor.selection_bytes(), first);
        field.activate_view(2, &mut system).unwrap();
        assert_eq!(field.editor.selection_bytes(), second);
        assert_eq!(field.hit_testing.decisions, 3);
        assert!(
            field
                .command(&mut system, EditCommand::Click(f32::NAN, 0., 1, false))
                .is_err()
        );
        assert_eq!(field.editor.selection_bytes(), second);
        assert_eq!(field.text(), source);
    }

    #[test]
    fn easl_navigation_keeps_preferred_columns_with_each_pane_and_preserves_markdown_history() {
        let source = "abcdefghij\nx\nabcdefghij";
        let mut field = TextField::with_kind(source, false, true).unwrap();
        let mut system = TextSystem::new();
        let style = TextStyle {
            family: "monospace".into(),
            ..TextStyle::default()
        };
        field
            .editor
            .ensure_layout(&mut system, &style, 400., 100.)
            .unwrap();
        field.editor.select_range(&mut system, (7, 7)).unwrap();
        field
            .command(&mut system, EditCommand::Move(Movement::Down, true))
            .unwrap();
        assert_eq!(field.editor.selection_bytes(), (7, 12));
        field.activate_view(2, &mut system).unwrap();
        field
            .editor
            .ensure_layout(&mut system, &style, 400., 100.)
            .unwrap();
        field
            .command(&mut system, EditCommand::Move(Movement::LineEnd, false))
            .unwrap();
        assert_eq!(field.editor.selection_bytes(), (10, 10));
        field.activate_view(1, &mut system).unwrap();
        field
            .command(&mut system, EditCommand::Move(Movement::Down, true))
            .unwrap();
        assert_eq!(field.editor.selection_bytes(), (7, 20));
        assert_eq!(field.navigation.decisions, 3);
        assert_eq!(field.text(), source);
        field
            .command(&mut system, EditCommand::Insert("Z".into()))
            .unwrap();
        assert_eq!(field.text(), "abcdefgZhij");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
    }

    #[test]
    fn pane_views_share_edits_and_undo_but_retain_their_own_carets_and_scroll() {
        let mut field = TextField::with_kind("café 🌍", true, true).unwrap();
        let mut system = TextSystem::new();
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field.editor.scroll = 12.;
        field.activate_view(2, &mut system).unwrap();
        assert_eq!(field.editor.selection_bytes(), (0, 0));
        field
            .command(&mut system, EditCommand::Insert("X".into()))
            .unwrap();
        field.activate_view(1, &mut system).unwrap();
        assert_eq!(field.editor.text(), "Xcafé 🌍");
        assert_eq!(field.editor.selection_bytes(), (11, 11));
        // Switching views must retain the user's explicit scroll positions.
        field.editor.scroll = 12.;
        field.activate_view(2, &mut system).unwrap();
        field.editor.scroll = 27.;
        field.activate_view(1, &mut system).unwrap();
        assert_eq!(field.editor.scroll.to_bits(), 12_f32.to_bits());
        field.command(&mut system, EditCommand::Undo).unwrap();
        field.activate_view(2, &mut system).unwrap();
        assert_eq!(field.editor.text(), "café 🌍");
        assert_eq!(field.editor.selection_bytes(), (0, 0));
        field.command(&mut system, EditCommand::Redo).unwrap();
        assert_eq!(field.text(), "Xcafé 🌍");
    }

    #[test]
    fn rendering_another_pane_does_not_commit_or_redirect_composition() {
        let mut field = TextField::with_kind("ab", true, true).unwrap();
        let mut system = TextSystem::new();
        field
            .command(&mut system, EditCommand::Move(Movement::Right, false))
            .unwrap();
        field
            .command(&mut system, EditCommand::Preedit("候補".into(), None))
            .unwrap();
        field.activate_view(2, &mut system).unwrap();
        assert!(field.is_composing());
        assert_eq!(field.editor.text(), "ab");
        assert!(
            field
                .command(&mut system, EditCommand::Insert("wrong".into()))
                .is_err()
        );
        assert_eq!(field.text(), "ab");
        field.activate_view(1, &mut system).unwrap();
        field
            .command(&mut system, EditCommand::Commit("確".into()))
            .unwrap();
        assert!(!field.is_composing());
        assert_eq!(field.text(), "a確b");
        field.activate_view(2, &mut system).unwrap();
        assert_eq!(field.editor.text(), "a確b");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "ab");
    }

    #[test]
    fn pane_source_mode_is_local_while_semantic_history_is_shared() {
        let (mut field, mut system) = fixture("Words");
        field.toggle_source(&mut system).unwrap();
        field.activate_view(2, &mut system).unwrap();
        assert!(!field.source_mode());
        field.format(&mut system, 1).unwrap();
        field.activate_view(1, &mut system).unwrap();
        assert!(field.source_mode());
        assert_eq!(field.editor.text(), "# Words");
        field.command(&mut system, EditCommand::Undo).unwrap();
        field.activate_view(2, &mut system).unwrap();
        assert!(!field.source_mode());
        assert_eq!(field.editor.text(), "Words");
    }

    #[test]
    fn source_outdent_is_one_undo_and_unindented_source_releases_shift_tab() {
        let source = "\tFirst α\r\n    Second β\n\tThird 🌍";
        let (mut field, mut system) = fixture(source);
        field.toggle_source(&mut system).unwrap();
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        assert!(field.indent(&mut system, ListIndent::Outdent).unwrap());
        assert_eq!(field.text(), "First α\r\nSecond β\nThird 🌍");
        assert!(!field.indent(&mut system, ListIndent::Outdent).unwrap());
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
        assert_eq!(field.editor.selected_text(), Some(source));
        field.command(&mut system, EditCommand::Redo).unwrap();
        assert_eq!(field.text(), "First α\r\nSecond β\nThird 🌍");
    }

    #[test]
    fn stored_typing_marks_belong_to_their_pane() {
        let (mut field, mut system) = fixture("ab");
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field.format(&mut system, 4).unwrap();
        field.activate_view(2, &mut system).unwrap();
        field
            .command(&mut system, EditCommand::Insert("X".into()))
            .unwrap();
        assert_eq!(field.text(), "Xab");
        field.activate_view(1, &mut system).unwrap();
        field
            .command(&mut system, EditCommand::Insert("Y".into()))
            .unwrap();
        assert_eq!(field.text(), "Xab**Y**");
    }

    fn style_scene() -> crate::interface::Scene {
        crate::interface::Interface::compile(crate::interface::SOURCE)
            .unwrap()
            .step({
                let mut input = [0.; crate::interface::INPUT_COUNT];
                input[..2].copy_from_slice(&[1280., 820.]);
                input[8..12].copy_from_slice(&[1., 1., 1., 20.]);
                input[12] = 1.;
                input
            })
            .unwrap()
    }
    fn fixture(source: &str) -> (TextField, TextSystem) {
        let mut field = TextField::new(source, true).unwrap();
        let mut system = TextSystem::new();
        field
            .ensure_layout(
                &mut system,
                &TextStyle::default(),
                &style_scene(),
                600.,
                400.,
            )
            .unwrap();
        (field, system)
    }

    fn style_for_word(field: &TextField, word: &str) -> TextStyle {
        let at = field.document.projection().text().find(word).unwrap();
        field
            .styles
            .as_ref()
            .unwrap()
            .spans
            .iter()
            .find(|s| s.range.contains(&at))
            .unwrap()
            .style
            .clone()
    }

    #[test]
    fn loom_rich_style_resolution_runs_in_easl_and_preserves_current_roles_and_source() {
        let source = "# Heading **strong** *emphasis* [link](https://example.org)\n\nBody **`code`** and **bold**.\n\n---\n";
        let mut field = TextField::new(source, true).unwrap();
        let mut system = TextSystem::new();
        let scene = style_scene();
        let body = TextStyle {
            features: "'liga' 0".into(),
            variations: "'wght' 625".into(),
            locale: Some("sr".into()),
            letter_spacing: 0.25,
            word_spacing: 1.5,
            color: [7, 8, 9, 255],
            ..TextStyle::default()
        };
        field
            .ensure_layout(&mut system, &body, &scene, 600., 400.)
            .unwrap();
        assert_eq!(field.styling.resolutions, 1);
        assert_eq!(field.text(), source);
        assert_eq!(field.editor.text(), field.document.projection().text());
        let role = |id| {
            scene
                .typography
                .iter()
                .find(|(key, _)| *key == id)
                .unwrap()
                .1
                .clone()
        };
        let mut heading = role(10);
        heading.color = body.color;
        assert_eq!(style_for_word(&field, "Heading"), heading);
        let mut strong = heading.clone();
        strong.weight = role(22).weight;
        assert_eq!(style_for_word(&field, "strong"), strong);
        let mut emphasis = heading.clone();
        emphasis.italic = role(23).italic;
        assert_eq!(style_for_word(&field, "emphasis"), emphasis);
        let mut link = heading;
        link.underline = role(21).underline;
        assert_eq!(style_for_word(&field, "link"), link);
        let mut code = body.clone();
        code.family = role(20).family;
        code.size = role(20).size;
        code.weight = role(22).weight;
        assert_eq!(style_for_word(&field, "code"), code);
        assert_eq!(style_for_word(&field, "Body"), body);
        let mut bold = body.clone();
        bold.weight = role(22).weight;
        assert_eq!(style_for_word(&field, "bold"), bold);
        assert!(field.blocks().iter().any(|b| b.style == BlockStyle::Rule));
        for block in field
            .blocks()
            .iter()
            .filter(|b| b.style == BlockStyle::Rule)
        {
            assert!(field.styles.as_ref().unwrap().spans.iter().any(|s| {
                s.range.start >= block.display.start && s.range.end <= block.display.end
            }));
            assert!(
                field
                    .styles
                    .as_ref()
                    .unwrap()
                    .spans
                    .iter()
                    .filter(|s| s.range.start >= block.display.start
                        && s.range.end <= block.display.end)
                    .all(|s| s.style.color[3] == 0)
            );
        }
        field.toggle_source(&mut system).unwrap();
        assert_eq!(field.editor.text(), source);
        assert_eq!(
            field.styles.as_ref().unwrap().spans,
            [StyledSpan {
                range: 0..source.len(),
                style: role(24)
            }]
        );
        field.toggle_source(&mut system).unwrap();
        assert_eq!(style_for_word(&field, "Body"), body);
        assert_eq!(field.text(), source);
    }

    #[test]
    fn rich_style_cache_survives_repaint_resize_and_independent_panes_but_tracks_changes() {
        let source = "**café** tail";
        let (mut field, mut system) = fixture(source);
        let scene = style_scene();
        let body = TextStyle::default();
        let initial = field.styling.resolutions;
        field
            .ensure_layout(&mut system, &body, &scene, 360., 200.)
            .unwrap();
        let shaping = field.editor.inner().shaping_generation();
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field
            .ensure_layout(&mut system, &body, &scene, 420., 220.)
            .unwrap();
        assert_eq!(field.styling.resolutions, initial);
        assert_eq!(field.editor.inner().shaping_generation(), shaping);
        field.activate_view(2, &mut system).unwrap();
        let second = TextStyle {
            size: 17.,
            color: [10, 20, 30, 255],
            ..body.clone()
        };
        field
            .ensure_layout(&mut system, &second, &scene, 260., 200.)
            .unwrap();
        let second_count = field.styling.resolutions;
        field.activate_view(1, &mut system).unwrap();
        field
            .ensure_layout(&mut system, &body, &scene, 420., 220.)
            .unwrap();
        assert_eq!(field.styling.resolutions, second_count);
        assert_eq!(
            style_for_word(&field, "tail").size.to_bits(),
            body.size.to_bits()
        );
        field
            .command(&mut system, EditCommand::Insert("!".into()))
            .unwrap();
        assert_eq!(field.text(), "**café** tail!");
        assert_eq!(field.styling.resolutions, second_count + 1);
        field.activate_view(2, &mut system).unwrap();
        assert_eq!(field.styling.resolutions, second_count + 2);
        assert_eq!(
            style_for_word(&field, "tail").size.to_bits(),
            second.size.to_bits()
        );
        assert_eq!(style_for_word(&field, "tail").color, second.color);
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
        assert_eq!(
            style_for_word(&field, "tail").size.to_bits(),
            second.size.to_bits()
        );
        let before = field.styling.resolutions;
        field
            .command(&mut system, EditCommand::Preedit("仮".into(), None))
            .unwrap();
        field
            .ensure_layout(&mut system, &second, &scene, 260., 200.)
            .unwrap();
        assert_eq!(field.styling.resolutions, before);
        assert_eq!(field.text(), source);
        field
            .command(&mut system, EditCommand::CancelCompose)
            .unwrap();
        assert_eq!(field.text(), source);
        let mut changed = scene.clone();
        changed
            .typography
            .iter_mut()
            .find(|(id, _)| *id == 22)
            .unwrap()
            .1
            .weight = 800.;
        field
            .ensure_layout(&mut system, &second, &changed, 260., 200.)
            .unwrap();
        assert_eq!(
            style_for_word(&field, "café").weight.to_bits(),
            800_f32.to_bits()
        );
        assert_eq!(field.styling.resolutions, before + 1);
    }

    #[test]
    fn invalid_rich_appearance_preserves_painted_projection_and_can_recover() {
        let source = "**author** text";
        let (mut field, mut system) = fixture(source);
        let before = field.editor.text();
        let scene = style_scene();
        let invalid = TextStyle {
            size: f32::NAN,
            ..TextStyle::default()
        };
        assert!(
            field
                .ensure_layout(&mut system, &invalid, &scene, 600., 400.)
                .is_err()
        );
        assert_eq!(field.text(), source);
        assert_eq!(field.editor.text(), before);
        assert!(field.editor.inner().try_layout().is_some());
        field
            .ensure_layout(&mut system, &TextStyle::default(), &scene, 600., 400.)
            .unwrap();
        assert_eq!(field.editor.text(), before);
        assert_eq!(
            style_for_word(&field, "author").weight.to_bits(),
            700_f32.to_bits()
        );
    }

    #[test]
    fn native_heading_style_caret_and_markdown_source_agree() {
        let (mut field, mut system) = fixture("Words");
        field.format(&mut system, 1).unwrap();
        assert_eq!(field.text(), "# Words");
        assert_eq!(field.editor.text(), "Words");
        let height = field.editor.inner().cursor_geometry(1.).unwrap().height();
        assert!(height > 30., "Heading caret was {height}");
        field.toggle_source(&mut system).unwrap();
        assert_eq!(field.editor.text(), "# Words");
        field.toggle_source(&mut system).unwrap();
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "Words");
        field.command(&mut system, EditCommand::Redo).unwrap();
        assert_eq!(field.text(), "# Words");
    }

    #[test]
    fn empty_heading_and_enter_to_body_use_their_own_caret_metrics() {
        let (mut field, mut system) = fixture("");
        field.format(&mut system, 1).unwrap();
        assert!(field.editor.inner().cursor_geometry(1.).unwrap().height() > 40.);
        field
            .command(&mut system, EditCommand::Insert("Title".into()))
            .unwrap();
        field
            .command(&mut system, EditCommand::Insert("\n".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "Title\n");
        let caret = field.editor.inner().cursor_geometry(1.).unwrap();
        assert!((caret.height() - 33.).abs() < 0.01, "{caret:?}");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert!(field.editor.inner().cursor_geometry(1.).unwrap().height() > 40.);
    }

    #[test]
    fn native_ime_preedit_is_never_saved_and_commit_is_one_semantic_undo() {
        let (mut field, mut system) = fixture("**café** tail");
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field
            .command(&mut system, EditCommand::Preedit("仮".into(), Some((3, 3))))
            .unwrap();
        assert_eq!(field.text(), "**café** tail");
        assert!(!field.dirty());
        field
            .command(
                &mut system,
                EditCommand::Preedit("仮名".into(), Some((6, 6))),
            )
            .unwrap();
        field
            .command(&mut system, EditCommand::Commit("確定".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "café tail確定");
        assert_eq!(field.text(), "**café** tail確定");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "**café** tail");
        field
            .command(&mut system, EditCommand::Preedit("仮".into(), None))
            .unwrap();
        field
            .command(&mut system, EditCommand::CancelCompose)
            .unwrap();
        assert_eq!(field.editor.text(), "café tail");
        assert!(!field.dirty());
    }

    #[test]
    fn native_input_rules_update_source_style_and_caret_but_paste_stays_literal() {
        let (mut field, mut system) = fixture("");
        field
            .command(&mut system, EditCommand::Insert("#".into()))
            .unwrap();
        let before = field.text();
        field
            .command(&mut system, EditCommand::Insert(" ".into()))
            .unwrap();
        assert_eq!(field.text(), "# ");
        assert_eq!(field.editor.text(), "");
        assert!(field.editor.inner().cursor_geometry(1.).unwrap().height() > 40.);
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), before);
        assert_eq!(field.editor.text(), "#");
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        field
            .command(&mut system, EditCommand::Paste("**literal**".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "**literal**");
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        field
            .command(&mut system, EditCommand::Preedit("**café**".into(), None))
            .unwrap();
        assert!(field.text().contains("literal"));
        field
            .command(&mut system, EditCommand::Commit("**café**".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "café");
        assert_eq!(field.text(), "**café**");
        assert_eq!(field.editor.selection_bytes(), (5, 5));
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.editor.text(), "**literal**");
    }

    #[test]
    fn native_backspace_joins_paragraphs_and_restores_caret_on_undo() {
        let (mut field, mut system) = fixture("**one**\n\n*two*");
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        for _ in 0..3 {
            field
                .command(&mut system, EditCommand::Move(Movement::Left, false))
                .unwrap();
        }
        assert_eq!(field.editor.selection_bytes(), (4, 4));
        field.command(&mut system, EditCommand::Backspace).unwrap();
        assert_eq!(field.editor.text(), "onetwo");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.editor.selection_bytes(), (4, 4));
        assert_eq!(field.text(), "**one**\n\n*two*");
    }

    #[test]
    fn native_source_mode_preserves_mixed_line_endings_and_literal_delimiters() {
        let source = "# Title\r\n\r\nText\rmore\n";
        let (mut field, mut system) = fixture(source);
        field.toggle_source(&mut system).unwrap();
        assert_eq!(field.editor.text(), source);
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field
            .command(&mut system, EditCommand::Insert("* literal\t".into()))
            .unwrap();
        assert_eq!(field.text(), format!("{source}* literal\t"));
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
        field.toggle_source(&mut system).unwrap();
        assert!(!field.dirty());
    }

    #[test]
    fn verse_edits_keep_literal_markdown_and_every_line_ending() {
        let source = "# a verse\r\n\t* literal\rfinal\n";
        let mut field = TextField::with_kind(source, true, true).unwrap();
        let mut system = TextSystem::new();
        assert_eq!(field.editor.text(), source);
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field
            .command(&mut system, EditCommand::Insert("\r\n\tmore".into()))
            .unwrap();
        assert_eq!(field.text(), format!("{source}\r\n\tmore"));
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
        assert!(field.format(&mut system, 4).is_err());
        field.toggle_source(&mut system).unwrap();
        assert!(!field.source_mode());
        assert_eq!(field.editor.text(), source);
    }

    #[test]
    fn accessible_replacement_updates_source_history_and_saved_state() {
        let (mut field, mut system) = fixture("# Old title");
        field
            .set_accessible_value(&mut system, "New *literal* title")
            .unwrap();
        assert_eq!(field.editor.text(), "New *literal* title");
        assert!(field.dirty());
        assert_eq!(field.document.projection().text(), field.editor.text());
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "# Old title");
        assert!(!field.dirty());
        field.toggle_source(&mut system).unwrap();
        field
            .set_accessible_value(&mut system, "# Source\r\n\r\n**bold**\n")
            .unwrap();
        assert_eq!(field.text(), "# Source\r\n\r\n**bold**\n");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert!(!field.dirty());
    }

    #[test]
    fn rejected_accessible_replacement_preserves_document_and_selection() {
        let (mut field, mut system) = fixture("**safe**");
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        let selection = field.editor.selection_bytes();
        assert!(
            field
                .set_accessible_value(&mut system, &"x".repeat(1_048_577))
                .is_err()
        );
        assert_eq!(field.text(), "**safe**");
        assert_eq!(field.editor.text(), "safe");
        assert_eq!(field.editor.selection_bytes(), selection);
        assert!(!field.dirty());
        assert!(
            field
                .set_accessible_value(&mut system, &"\u{ad}".repeat(3000))
                .is_err()
        );
        assert_eq!(field.text(), "**safe**");
        assert_eq!(field.editor.text(), "safe");
        assert_eq!(field.editor.selection_bytes(), selection);
        assert!(!field.document.can_undo());
    }

    #[test]
    fn typed_tab_indents_list_but_pasted_tab_is_literal() {
        let (mut field, mut system) = fixture("- one\n- two");
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field
            .command(&mut system, EditCommand::Insert("\t".into()))
            .unwrap();
        assert_eq!(field.text(), "- one\n  - two");
        assert_eq!(field.editor.text(), "one\ntwo");
        assert!(field.indent(&mut system, ListIndent::Outdent).unwrap());
        assert_eq!(field.text(), "- one\n- two");
        field
            .command(&mut system, EditCommand::Paste("\t".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "one\ntwo\t");
        assert_eq!(field.document.projection().blocks()[1].list_depth, 1);
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "- one\n- two");
    }

    #[test]
    fn code_inside_a_list_takes_literal_tab_newline_and_ime_commits() {
        let source = "- before\n- ```rust\r\n  A\r\n  ```";
        let (mut field, mut system) = fixture(source);
        assert_eq!(field.editor.text(), "before\nA");
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field
            .command(&mut system, EditCommand::Insert("\t".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "before\nA\t");
        assert_eq!(field.document.projection().blocks()[1].list_depth, 1);
        field
            .command(&mut system, EditCommand::Insert("\n".into()))
            .unwrap();
        let saved = field.text().clone();
        field
            .command(&mut system, EditCommand::Preedit("**café**".into(), None))
            .unwrap();
        assert_eq!(field.text(), saved);
        field
            .command(&mut system, EditCommand::Commit("**café**".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "before\nA\t\n**café**");
        assert!(matches!(
            field.document.projection().blocks()[1].style,
            BlockStyle::Code(_)
        ));
        field.toggle_source(&mut system).unwrap();
        assert_eq!(field.editor.text(), field.text());
        field.toggle_source(&mut system).unwrap();
        for _ in 0..3 {
            field.command(&mut system, EditCommand::Undo).unwrap();
        }
        assert_eq!(field.text(), source);
        assert_eq!(field.editor.text(), "before\nA");
        assert!(!field.dirty());
    }

    #[test]
    fn native_boundary_deletion_and_cross_container_ime_keep_semantic_history() {
        let source = "> one\n\nBody\n\nTail";
        let (mut field, mut system) = fixture(source);
        for _ in 0..4 {
            field
                .command(&mut system, EditCommand::Move(Movement::Right, false))
                .unwrap();
        }
        assert_eq!(field.editor.selection_bytes(), (4, 4));
        field.command(&mut system, EditCommand::Backspace).unwrap();
        assert_eq!(field.editor.text(), "one\nBody\nTail");
        assert_eq!(field.document.projection().blocks()[1].quote_depth, 1);
        assert_eq!(field.editor.selection_bytes(), (4, 4));
        field.command(&mut system, EditCommand::Backspace).unwrap();
        assert_eq!(field.editor.text(), "oneBody\nTail");
        assert_eq!(field.document.projection().blocks()[0].quote_depth, 1);
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.editor.text(), "one\nBody\nTail");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
        assert_eq!(field.editor.selection_bytes(), (4, 4));
        for _ in 0..2 {
            field
                .command(&mut system, EditCommand::Move(Movement::Left, false))
                .unwrap();
        }
        for _ in 0..4 {
            field
                .command(&mut system, EditCommand::Move(Movement::Right, true))
                .unwrap();
        }
        assert_eq!(field.editor.selection_bytes(), (2, 6));
        field
            .command(&mut system, EditCommand::Preedit("café".into(), None))
            .unwrap();
        assert_eq!(field.text(), source);
        field
            .command(&mut system, EditCommand::Commit("café".into()))
            .unwrap();
        assert_eq!(field.editor.text(), "oncafédy\nTail");
        assert_eq!(
            field.document.visual_selection().unwrap(),
            Selection::caret(7)
        );
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
        assert_eq!(field.editor.selection_bytes(), (2, 6));
        assert!(!field.dirty());
    }
}
