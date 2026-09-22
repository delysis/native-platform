//! Source admission without a font system, native widget or EASL VM.
//!
//! The storage worker and the editor constructor use the same document factory.
//! Validation never constructs an editor merely to discard it. The installed
//! check also applies to subsequent transactions, undo and redo.
use loom_markdown::{Dialect, EditorDocument, Projection};

pub fn create(source: &str, verse: bool) -> Result<EditorDocument, String> {
    // Bound source work before Markdown allocation, not after projection.
    easl_native_text::validate_text(source).map_err(|_| loom_markdown::Error::Limit.to_string())?;
    let dialect = if verse {
        Dialect::PlainText
    } else {
        Dialect::Loom
    };
    let mut document = EditorDocument::new(source, dialect).map_err(|error| error.to_string())?;
    document
        .set_admission_check(admit_projection)
        .map_err(|error| error.to_string())?;
    Ok(document)
}

fn admit_projection(source: &str, projection: &Projection) -> Result<(), loom_markdown::Error> {
    // One slot remains reserved for the default style. This form also avoids
    // overflowing a len + 1 calculation at an untrusted capacity boundary.
    if projection.spans().len() >= easl_native_text::MAX_EDITOR_SPANS {
        return Err(loom_markdown::Error::Limit);
    }
    easl_native_text::validate_text(source)
        .and_then(|()| easl_native_text::validate_text(projection.text()))
        .map_err(|_| loom_markdown::Error::Limit)
}

#[cfg(test)]
#[path = "document_model_tests.rs"]
mod tests;
