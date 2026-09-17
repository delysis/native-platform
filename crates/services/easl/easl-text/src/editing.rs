//! Native binding for the reusable EASL editing-policy module. The VM receives
//! Unicode/selection facts and byte lengths, never a copy of the text buffer.
use crate::{Error, runtime::Runtime};
use easl_native_text::{EditContext, TextEditor, TextSystem};
use std::ops::Range;

pub const EDITING_SOURCE: &str = include_str!("../library/editing.easl");
const NATIVE_SOURCE: &str = include_str!("../library/editing-native.easl");

#[derive(Clone, Copy, Debug)]
pub enum EditAction<'a> {
    SelectAll,
    CollapseStart,
    CollapseEnd,
    MoveTo { byte: usize, extend: bool },
    Backspace,
    Delete,
    BackspaceWord,
    DeleteWord,
    DeleteSelection,
    Replace(&'a str),
}
impl EditAction<'_> {
    fn words(self) -> bool {
        matches!(self, Self::BackspaceWord | Self::DeleteWord)
    }
    fn encode(self) -> Result<[u32; 4], Error> {
        Ok(match self {
            Self::SelectAll => [0, 0, 0, 0],
            Self::CollapseStart => [1, 0, 0, 0],
            Self::CollapseEnd => [2, 0, 0, 0],
            Self::MoveTo { byte, extend } => [
                3,
                u32::try_from(byte).map_err(|_| Error::Invalid)?,
                0,
                u32::from(extend),
            ],
            Self::Backspace => [4, 0, 0, 0],
            Self::Delete => [5, 0, 0, 0],
            Self::BackspaceWord => [6, 0, 0, 0],
            Self::DeleteWord => [7, 0, 0, 0],
            Self::DeleteSelection => [8, 0, 0, 0],
            Self::Replace(text) => [
                9,
                0,
                u32::try_from(text.len()).map_err(|_| Error::Limit)?,
                0,
            ],
        })
    }
}

/// A synchronous view intent. A document-model consumer applies it to the same
/// source/selection snapshot, using that model's normal transaction and history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditPlan {
    replacement: Option<Range<usize>>,
    selection: (usize, usize),
}
impl EditPlan {
    pub fn replacement(&self) -> Option<Range<usize>> {
        self.replacement.clone()
    }
    pub fn selection(&self) -> (usize, usize) {
        self.selection
    }
}

pub struct Editing {
    runtime: Runtime,
    faulted: bool,
    pub decisions: u64,
}
impl std::fmt::Debug for Editing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editing")
            .field("faulted", &self.faulted)
            .field("decisions", &self.decisions)
            .finish_non_exhaustive()
    }
}
impl Editing {
    pub fn new() -> Result<Self, Error> {
        let runtime = std::panic::catch_unwind(|| {
            Runtime::compile(&[
                ("editing-native.easl", NATIVE_SOURCE),
                ("editing.easl", EDITING_SOURCE),
            ])
        })
        .map_err(|_| Error::Language("Editing compiler panicked".into()))??;
        Ok(Self {
            runtime,
            faulted: false,
            decisions: 0,
        })
    }

    pub fn plan_for(
        &mut self,
        editor: &mut TextEditor,
        system: &mut TextSystem,
        action: EditAction<'_>,
    ) -> Result<EditPlan, Error> {
        let context = editor.edit_context(system, action.words())?;
        self.plan(context, action)
    }

    pub fn plan(
        &mut self,
        context: EditContext,
        action: EditAction<'_>,
    ) -> Result<EditPlan, Error> {
        if self.faulted {
            return Err(Error::Faulted);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.plan_inner(context, action)
        }));
        match result {
            Ok(Ok(plan)) => {
                self.decisions = self.decisions.saturating_add(1);
                Ok(plan)
            }
            Ok(Err(error @ Error::Language(_))) => {
                self.faulted = true;
                Err(error)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                self.faulted = true;
                Err(Error::Language("Editing VM panicked".into()))
            }
        }
    }

    fn plan_inner(
        &mut self,
        context: EditContext,
        action: EditAction<'_>,
    ) -> Result<EditPlan, Error> {
        let facts = [
            context.text_len,
            context.anchor,
            context.focus,
            context.grapheme_before,
            context.grapheme_after,
            context.word_before,
            context.word_after,
        ]
        .map(|value| u32::try_from(value).map_err(|_| Error::Invalid));
        let facts = facts.into_iter().collect::<Result<Vec<_>, _>>()?;
        self.runtime.write("text-edit-context", &facts)?;
        self.runtime.write("text-edit-command", &action.encode()?)?;
        self.runtime
            .write("text-edit-plan", &[u32::MAX, 0, 0, 0, 0, 0])?;
        self.runtime.run("text-edit-native")?;
        let output = self.runtime.read("text-edit-plan")?;
        let [status, effect, start, end, anchor, focus] = output.as_slice() else {
            return Err(Error::Invalid);
        };
        match status {
            0 => {}
            2 => return Err(Error::Limit),
            _ => return Err(Error::Invalid),
        }
        let to_index = |value: u32| usize::try_from(value).map_err(|_| Error::Invalid);
        let range = to_index(*start)?..to_index(*end)?;
        if range.start > range.end || range.end > context.text_len {
            return Err(Error::Invalid);
        }
        let new_length = match action {
            EditAction::Replace(text) => context.text_len - range.len() + text.len(),
            _ if *effect == 1 => context.text_len - range.len(),
            _ => context.text_len,
        };
        let selection = (to_index(*anchor)?, to_index(*focus)?);
        if selection.0 > new_length || selection.1 > new_length {
            return Err(Error::Invalid);
        }
        let replacement = match effect {
            0 => None,
            1 => Some(range),
            _ => return Err(Error::Invalid),
        };
        Ok(EditPlan {
            replacement,
            selection,
        })
    }

    /// Standalone native widget consumer. Semantic document editors can instead
    /// use `plan_for` and apply the intent through their own document transaction.
    pub fn apply(
        &mut self,
        editor: &mut TextEditor,
        system: &mut TextSystem,
        action: EditAction<'_>,
    ) -> Result<(), Error> {
        let plan = self.plan_for(editor, system, action)?;
        if let Some(range) = plan.replacement {
            let text = match action {
                EditAction::Replace(text) => text,
                _ => "",
            };
            editor.replace_range(system, range, text, plan.selection)?;
        } else {
            editor.select_range(system, plan.selection)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_easl_import_composes_selection_and_deletion_behavior() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/editing_policy.easl");
        let documents = easl::parse::load_and_parse_easl_multidocument(&path)
            .unwrap()
            .unwrap()
            .unwrap();
        let mut runtime = Runtime::from_documents(documents).unwrap();
        runtime.run("main").unwrap();
        assert_eq!(runtime.read("erased").unwrap(), vec![0, 1, 1, 16, 1, 1]);
        assert_eq!(runtime.read("extended").unwrap(), vec![0, 0, 16, 16, 16, 0]);
    }
}
