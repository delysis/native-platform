use super::{EditorDocument, EditorSelection, Selection};
use crate::{Bias, Error, Projection};
use std::ops::Range;

enum VisualAnchor {
    Source(usize),
    Decoded {
        source: Range<usize>,
        text: String,
        within: usize,
    },
}

impl EditorDocument {
    /// Explicit storage normalization, with exact undo. Parsing/editing alone
    /// never calls this: a host whose prose format requires LF opts in at save.
    pub fn normalize_line_endings(&mut self) -> Result<bool, Error> {
        if !self.source().contains('\r') {
            return Ok(false);
        }
        let normalized = normalize(self.source());
        let selection = self.selection;
        let visual = match selection {
            EditorSelection::Source(_) => None,
            EditorSelection::Visual(s, bias) => Some((
                VisualAnchor::capture(self.source(), self.projection(), s.anchor, bias)?,
                VisualAnchor::capture(self.source(), self.projection(), s.focus, bias)?,
            )),
        };
        let mapped_source = match selection {
            EditorSelection::Source(s) => Some(Selection {
                anchor: normalize(&self.source()[..s.anchor]).len(),
                focus: normalize(&self.source()[..s.focus]).len(),
            }),
            EditorSelection::Visual(_, _) => None,
        };
        let mut transaction = self.transaction();
        transaction.replace(0..self.source().len(), normalized)?;
        transaction.set_selection(Selection::caret(0));
        self.apply_checked(transaction, |_, projection| {
            Ok(Some(match selection {
                EditorSelection::Source(_) => {
                    EditorSelection::Source(mapped_source.ok_or(Error::InvalidRange)?)
                }
                EditorSelection::Visual(_, bias) => {
                    let (anchor, focus) = visual.as_ref().ok_or(Error::InvalidRange)?;
                    EditorSelection::Visual(
                        Selection {
                            anchor: anchor.resolve(projection, bias)?,
                            focus: focus.resolve(projection, bias)?,
                        },
                        bias,
                    )
                }
            }))
        })
    }
}

impl VisualAnchor {
    fn capture(
        source: &str,
        projection: &Projection,
        offset: usize,
        bias: Bias,
    ) -> Result<Self, Error> {
        match projection.source_at(offset, bias) {
            Ok(at) => Ok(Self::Source(normalize(&source[..at]).len())),
            Err(Error::AmbiguousBoundary) => {
                let span = projection
                    .spans()
                    .iter()
                    .find(|s| s.display.start < offset && offset < s.display.end)
                    .ok_or(Error::AmbiguousBoundary)?;
                Ok(Self::Decoded {
                    source: normalize(&source[..span.source.start]).len()
                        ..normalize(&source[..span.source.end]).len(),
                    text: projection.text()[span.display.clone()].into(),
                    within: offset - span.display.start,
                })
            }
            Err(error) => Err(error),
        }
    }

    fn resolve(&self, projection: &Projection, bias: Bias) -> Result<usize, Error> {
        match self {
            Self::Source(at) => projection.display_at(*at, bias),
            Self::Decoded {
                source,
                text,
                within,
            } => {
                let span = projection
                    .spans()
                    .iter()
                    .find(|s| s.source == *source && projection.text()[s.display.clone()] == *text)
                    .ok_or(Error::AmbiguousBoundary)?;
                Ok(span.display.start + within)
            }
        }
    }
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
