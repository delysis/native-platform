use crate::{EditorDocument, Error, Selection};

impl EditorDocument {
    /// Remove one literal tab or up to four leading spaces from selected source
    /// lines, preserving all other bytes. False leaves Shift-Tab to focus traversal.
    pub fn outdent_source(&mut self, selection: Selection) -> Result<bool, Error> {
        let range = super::checked_range(self.source(), selection)?;
        let bytes = self.source().as_bytes();
        let start = bytes[..range.start]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let end = if range.end > range.start && bytes[range.end - 1] == b'\n' {
            range.end - 1
        } else {
            range.end
        };
        let mut transaction = self.transaction();
        let mut line = start;
        loop {
            let length = if bytes.get(line) == Some(&b'\t') {
                1
            } else {
                bytes[line..]
                    .iter()
                    .take(4)
                    .take_while(|byte| **byte == b' ')
                    .count()
            };
            if length != 0 {
                transaction.replace(line..line + length, "")?;
            }
            let Some(offset) = bytes[line..end].iter().position(|byte| *byte == b'\n') else {
                break;
            };
            line += offset + 1;
        }
        if transaction.edits().is_empty() {
            return Ok(false);
        }
        let adjusted = |position: usize| {
            position
                - transaction
                    .edits()
                    .iter()
                    .map(|edit| {
                        position
                            .saturating_sub(edit.range.start)
                            .min(edit.range.len())
                    })
                    .sum::<usize>()
        };
        let after = Selection {
            anchor: adjusted(selection.anchor),
            focus: adjusted(selection.focus),
        };
        transaction.before_source(selection);
        transaction.set_selection(after);
        self.apply(transaction)
    }
}
