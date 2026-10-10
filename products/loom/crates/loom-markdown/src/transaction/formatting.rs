//! A formatting palette borrows one document's visual selection. Its commands
//! must not follow focus into another document or silently rebase across edits.
use super::{EditorDocument, EditorSelection, Selection, validate_selection};
use crate::{
    Bias, BlockStyle, Dialect, Error, InlineFormat, InlineStyle, NodeKind, ParagraphFormat,
    StructureFormat,
};

#[derive(Debug)]
pub struct FormattingSelection {
    editor: u64,
    revision: u64,
    selection: Selection,
    typing_style: Option<InlineStyle>,
}
impl FormattingSelection {
    pub fn selection(&self) -> Selection {
        self.selection
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormattingCommand {
    Paragraph(ParagraphFormat),
    Inline(InlineFormat),
    Structure(StructureFormat),
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent marks and nested containers may all be active"
)]
pub struct FormattingState {
    pub paragraph: ParagraphFormat,
    pub bold: bool,
    pub italic: bool,
    pub quote: bool,
    pub bullet_list: bool,
    pub ordered_list: bool,
    pub link: Option<std::sync::Arc<str>>,
    pub selection_empty: bool,
}

impl EditorDocument {
    /// Capture exact visual grapheme endpoints without editing source, selection
    /// or history. A view must separately retain its own focus/lifetime identity.
    pub fn capture_formatting_selection(
        &self,
        selection: Selection,
    ) -> Result<FormattingSelection, Error> {
        if self.markdown().dialect() == Dialect::PlainText {
            return Err(Error::UnsupportedEdit);
        }
        validate_selection(self.projection().text(), selection)?;
        Ok(FormattingSelection {
            editor: self.identity,
            revision: self.revision,
            selection,
            typing_style: if self.visual_selection() == Ok(selection) {
                self.typing_style.clone()
            } else {
                None
            },
        })
    }

    /// Check ownership/revision without scanning source or formatting ranges.
    pub fn validate_formatting_selection(
        &self,
        capture: &FormattingSelection,
    ) -> Result<(), Error> {
        if capture.editor != self.identity || capture.revision != self.revision {
            return Err(Error::StaleTransaction);
        }
        Ok(())
    }

    /// Match Loom's palette: the paragraph at the ordered selection start, any
    /// selected inline mark, and every selected ancestor structure. Mixed nested
    /// ordered/bullet selections may activate both list controls.
    pub fn formatting_state(
        &self,
        capture: &FormattingSelection,
    ) -> Result<FormattingState, Error> {
        self.validate_formatting_selection(capture)?;
        let range = capture.selection.range();
        let projection = self.projection();
        let mut state = FormattingState {
            paragraph: ParagraphFormat::Body,
            bold: false,
            italic: false,
            quote: false,
            bullet_list: false,
            ordered_list: false,
            link: None,
            selection_empty: range.is_empty(),
        };
        if range.is_empty() {
            let style = capture
                .typing_style
                .clone()
                .unwrap_or_else(|| crate::editing::caret_style_at(projection, range.start));
            state.bold = style.bold;
            state.italic = style.italic;
            state.link = style.link;
        } else {
            for span in projection
                .spans()
                .iter()
                .filter(|s| s.display.start < range.end && range.start < s.display.end)
            {
                state.bold |= span.style.bold;
                state.italic |= span.style.italic;
                if state.link.is_none() {
                    state.link.clone_from(&span.style.link);
                }
            }
        }
        let nodes = self.markdown().nodes();
        let mut selected = vec![false; nodes.len()];
        let mut first = true;
        for block in projection.blocks().iter().filter(|block| {
            if range.is_empty() {
                block.display.start <= range.start && range.start <= block.display.end
            } else {
                block.display.start < range.end && range.start < block.display.end
            }
        }) {
            selected[block.node] = true;
            if first {
                if let BlockStyle::Heading(level @ 1..=3) = block.style {
                    state.paragraph = ParagraphFormat::Heading(level);
                }
                first = false;
            }
        }
        let mut pending = vec![(0, false, false, false)];
        while let Some((id, quote, bullet, ordered)) = pending.pop() {
            let node = &nodes[id];
            let quote = quote || matches!(node.kind, NodeKind::Quote);
            let bullet = bullet || matches!(node.kind, NodeKind::List(None));
            let ordered = ordered || matches!(node.kind, NodeKind::List(Some(_)));
            if selected[id] {
                state.quote |= quote;
                state.bullet_list |= bullet;
                state.ordered_list |= ordered;
            }
            pending.extend(node.children.iter().map(|&id| (id, quote, bullet, ordered)));
        }
        Ok(state)
    }

    /// Apply to the captured document/selection and renew that capture only on
    /// success. Rejected commands preserve the current selection and typing
    /// marks as well as the existing transactional source/history guarantees.
    pub fn apply_formatting(
        &mut self,
        capture: &mut FormattingSelection,
        command: &FormattingCommand,
    ) -> Result<bool, Error> {
        self.validate_formatting_selection(capture)?;
        let before_selection = self.selection;
        let before_style = self.typing_style.clone();
        self.selection = EditorSelection::Visual(capture.selection, Bias::After);
        self.typing_style.clone_from(&capture.typing_style);
        let result = match command {
            FormattingCommand::Paragraph(format) => {
                self.format_paragraph(capture.selection, *format)
            }
            FormattingCommand::Inline(format) => self.format_inline(capture.selection, format),
            FormattingCommand::Structure(format) => {
                self.format_structure(capture.selection, *format)
            }
        };
        match result {
            Ok(changed) => {
                capture.revision = self.revision;
                capture.selection = self.visual_selection()?;
                capture.typing_style.clone_from(&self.typing_style);
                Ok(changed)
            }
            Err(error) => {
                self.selection = before_selection;
                self.typing_style = before_style;
                Err(error)
            }
        }
    }
}
