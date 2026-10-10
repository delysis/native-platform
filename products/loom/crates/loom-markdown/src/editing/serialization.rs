//! Resolve adjacent emphasis delimiters using the real Markdown parser. Most
//! paragraphs need only the ordinary serializer; ambiguous mark transitions can
//! choose the other standard delimiter without inserting invisible characters.
use super::{Fragment, FragmentKind, fragments, same_fragments};
use crate::{BlockStyle, Dialect, Error, Markdown};
use pulldown_cmark::Event;
use pulldown_cmark_to_cmark::{Options, cmark_with_options};

pub(super) fn markup(
    events: &[Event<'_>],
    expected: &[Fragment],
    style: &BlockStyle,
    adjacent_marks: bool,
) -> Result<String, Error> {
    let validate = adjacent_marks
        && !expected
            .iter()
            .any(|f| matches!(f.kind, FragmentKind::Raw(_)));
    for (strong_token, emphasis_token) in [("**", '*'), ("**", '_'), ("__", '*'), ("__", '_')] {
        let mut output = String::new();
        cmark_with_options(
            events.iter(),
            &mut output,
            Options {
                strong_token,
                emphasis_token,
                ..Options::default()
            },
        )
        .map_err(|_| Error::SerializationMismatch)?;
        if !validate || preserves_markup(&output, expected, style)? {
            return Ok(output);
        }
    }
    Err(Error::SerializationMismatch)
}

fn preserves_markup(
    source: &str,
    expected: &[Fragment],
    style: &BlockStyle,
) -> Result<bool, Error> {
    let parsed = Markdown::parse(source, Dialect::Loom)?;
    let projection = parsed.project()?;
    let [block] = projection.blocks() else {
        return Ok(false);
    };
    Ok(&block.style == style && same_fragments(expected, &fragments(&parsed, &projection, block)?))
}
