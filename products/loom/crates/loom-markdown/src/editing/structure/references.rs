//! Preserve nonvisual reference data when rewriting complete root containers.
use super::{EditorDocument, Error, preferred_newline};
use crate::{BlockStyle, Mapping};
use std::ops::Range;

pub(in crate::editing) fn retained(
    doc: &EditorDocument,
    range: Range<usize>,
) -> Result<String, Error> {
    let references = doc.markdown().references();
    let start = references.partition_point(|r| r.source.end <= range.start);
    let end = references.partition_point(|r| r.source.start < range.end);
    if references[start..end]
        .iter()
        .any(|r| r.source.start < range.start || r.source.end > range.end)
    {
        return Err(Error::UnsupportedEdit);
    }
    reject_unrepresented_definitions(doc, range.clone())?;
    if start == end {
        return Ok(String::new());
    }
    let newline = preferred_newline(&doc.source()[range.clone()]);
    let mut out = crate::editing::empty_body_prefix(&doc.source()[..range.start]);
    for reference in &references[start..end] {
        out.push('[');
        out.push_str(&reference.label);
        out.push_str("]: <");
        out.push_str(&crate::editing::escape_link_attribute(
            &reference.destination,
        ));
        out.push('>');
        if let Some(title) = &reference.title {
            out.push_str(" \"");
            out.push_str(&crate::editing::escape_link_attribute(title));
            out.push('"');
        }
        out.push_str(newline);
    }
    out.push_str(newline);
    if out.len() > crate::MAX_SOURCE_BYTES {
        return Err(Error::Limit);
    }
    Ok(out)
}

fn reject_unrepresented_definitions(
    doc: &EditorDocument,
    range: Range<usize>,
) -> Result<(), Error> {
    let references = doc.markdown().references();
    let projection = doc.projection();
    let mut offset = range.start;
    for line in doc.source()[range].split_inclusive(['\r', '\n']) {
        if let Some(at) = line.find('[')
            && line[at..].contains("]:")
            && line[..at].chars().all(|c| {
                c.is_whitespace()
                    || c.is_ascii_digit()
                    || matches!(c, '>' | '-' | '+' | '*' | '.' | ')')
            })
        {
            let position = offset + at;
            let known = references
                .partition_point(|r| r.source.start <= position)
                .checked_sub(1)
                .is_some_and(|i| references[i].source.contains(&position));
            let span = projection
                .spans()
                .partition_point(|s| s.source.end <= position);
            let visible = projection
                .spans()
                .get(span)
                .is_some_and(|s| s.mapping != Mapping::Separator && s.source.contains(&position));
            let block = projection
                .blocks()
                .partition_point(|b| b.source.start <= position)
                .checked_sub(1);
            let literal = block.is_some_and(|i| {
                let b = &projection.blocks()[i];
                b.source.contains(&position)
                    && matches!(b.style, BlockStyle::Code(_) | BlockStyle::Raw)
            });
            if !known && !visible && !literal {
                // The parser retains only the first definition of a label.
                // Until duplicate spans can be represented losslessly, decline
                // this edit instead of dropping a later author's definition.
                return Err(Error::UnsupportedEdit);
            }
        }
        offset += line.len();
    }
    Ok(())
}
