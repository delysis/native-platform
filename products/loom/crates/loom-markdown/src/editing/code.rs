//! Literal code editing: only the affected block is replaced. Code delimiters
//! never enter the view, and inserted fence-like text cannot escape its block.
use super::{escape_link_attribute, preferred_newline};
use crate::transaction::EditorSelection;
use crate::{Bias, BlockStyle, EditorDocument, Error, NodeKind, Selection};

impl EditorDocument {
    pub(super) fn replace_code(
        &mut self,
        before: Selection,
        selection: Selection,
        inserted: &str,
        index: usize,
    ) -> Result<bool, Error> {
        let block = &self.projection().blocks()[index];
        let BlockStyle::Code(language) = &block.style else {
            return Err(Error::UnsupportedEdit);
        };
        let range = selection.range();
        let mut content = self.projection().text()[block.display.clone()].to_owned();
        content.replace_range(
            range.start - block.display.start..range.end - block.display.start,
            inserted,
        );
        let mut source_range = block.source.clone();
        if matches!(
            self.markdown().nodes()[block.node].kind,
            NodeKind::CodeBlock { fenced: false, .. }
        ) {
            // The parser starts an indented block after its four structural
            // spaces. Replace those too when converting the affected block to
            // a fence; otherwise the new fence itself would be literal code.
            let start = source_range
                .start
                .checked_sub(4)
                .ok_or(Error::UnsupportedEdit)?;
            if self.source().get(start..source_range.start) != Some("    ") {
                return Err(Error::UnsupportedEdit);
            }
            source_range.start = start;
        }
        let original = &self.source()[source_range.clone()];
        let marker = original
            .trim_start_matches(' ')
            .chars()
            .next()
            .filter(|c| matches!(c, '`' | '~'))
            .unwrap_or('`');
        let newline = preferred_newline(original);
        let bare = render(language, &content, newline, marker)?;
        let line_start = self.source()[..source_range.start]
            .rfind(['\r', '\n'])
            .map_or(0, |i| i + 1);
        let prefix: String = self.source()[line_start..source_range.start]
            .chars()
            .map(|c| {
                if c == '>' || c.is_whitespace() {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        let mut replacement = prefix_lines(&bare, "", &prefix, true)?;
        // Retain the line ending that separates this source block from whatever
        // follows. The new fence owns its own terminating code-content break.
        if original.ends_with("\r\n") {
            replacement.push_str("\r\n");
        } else if original.ends_with(['\r', '\n']) {
            replacement.push_str(&original[original.len() - 1..]);
        }
        let mut transaction = self.transaction();
        transaction.replace(source_range, replacement)?;
        transaction.before_visual(before);
        transaction.set_selection(Selection::caret(0));
        let mut expected = self.projection().text().to_owned();
        expected.replace_range(range.clone(), inserted);
        let geometry: Vec<_> = self
            .projection()
            .blocks()
            .iter()
            .map(|b| {
                (
                    b.style.clone(),
                    b.quote_depth,
                    b.list_depth,
                    b.marker.clone(),
                )
            })
            .collect();
        self.apply_checked(transaction, |_, projection| {
            if projection.text() != expected
                || projection.blocks().len() != geometry.len()
                || projection.blocks().iter().zip(&geometry).any(|(b, g)| {
                    b.style != g.0 || b.quote_depth != g.1 || b.list_depth != g.2 || b.marker != g.3
                })
            {
                return Err(Error::SerializationMismatch);
            }
            Ok(Some(EditorSelection::Visual(
                Selection::caret(range.start + inserted.len()),
                Bias::After,
            )))
        })
    }
}

pub(super) fn render(
    language: &str,
    content: &str,
    newline: &str,
    marker: char,
) -> Result<String, Error> {
    let marker = if marker == '`' && language.contains('`') {
        '~'
    } else {
        marker
    };
    let longest = content
        .split(|c| c != marker)
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = marker.to_string().repeat(3.max(longest + 1));
    let info = escape_link_attribute(language);
    // Adjacent CR + LF is a single physical ending. Keep an authored edge byte
    // from merging with, and disappearing into, a syntax-only fence ending.
    let opening_end = if newline == "\r" && content.starts_with('\n') {
        "\r\n"
    } else {
        newline
    };
    let body_end = if content.is_empty() {
        ""
    } else if content.ends_with('\r') && newline == "\n" {
        "\r\n"
    } else {
        newline
    };
    let size = fence.len() * 2 + info.len() + opening_end.len() + content.len() + body_end.len();
    if size > crate::MAX_SOURCE_BYTES {
        return Err(Error::Limit);
    }
    // Always give nonempty content one syntax-only ending. An author-entered
    // final newline remains a visible blank code line after projection.
    Ok(format!(
        "{fence}{info}{opening_end}{content}{body_end}{fence}"
    ))
}

/// Apply container prefixes without normalizing any literal code line endings.
pub(super) fn prefix_lines(
    text: &str,
    first: &str,
    later: &str,
    indent_empty: bool,
) -> Result<String, Error> {
    let mut out = String::new();
    let mut rest = text;
    let mut prefix = first;
    loop {
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        let (line, tail) = rest.split_at(end);
        let ending = if tail.is_empty() {
            0
        } else if tail.starts_with("\r\n") {
            2
        } else {
            1
        };
        let indent = if out.is_empty() || indent_empty || !line.is_empty() {
            prefix
        } else {
            ""
        };
        if out
            .len()
            .saturating_add(indent.len())
            .saturating_add(line.len())
            .saturating_add(ending)
            > crate::MAX_SOURCE_BYTES
        {
            return Err(Error::Limit);
        }
        out.push_str(indent);
        out.push_str(line);
        if tail.is_empty() {
            break;
        }
        out.push_str(&tail[..ending]);
        rest = &tail[ending..];
        if rest.is_empty() {
            break;
        }
        prefix = later;
    }
    Ok(out)
}
