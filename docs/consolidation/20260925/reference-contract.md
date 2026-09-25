# Shared reference grammar

A name is syntax, not a filesystem capability or execution permission.

The existing bounded Loom expression/Markdown grammar is moved, unchanged, into
`workspace-document`. Loom's public module remains a compatibility re-export.
The existing document-projection implementation is also retained byte-for-byte
under `projection.rs`; this change does not install a new persistence format.

## Meaning by use

- Autocomplete/context: `@Name`, `@"A name"`, registered relative paths and exact
  retained `loom-material:` / `loom-evidence:` links are values. Resolution stays
  in the owning store, rejects ambiguous aliases and never recursively executes
  references found inside source text.
- Explicit expressions: only a leading `=` selects expression execution.
  `=@Expert(@Document)` is a call; `=@Document` is a read-only value. These remain
  subject to the existing bounded evaluator and native command admission.
- Mom consults: only start-of-input or whitespace-delimited `@Name` addresses
  invite participants. Parenthesized mentions remain passive. A retained link's
  label cannot invoke an expert. Names are parsed in full: `@Expert/notes` must
  never be shortened into an invocation of `@Expert`.

Consult parsing failures must propagate before dispatch. Address removal removes
only parsed address spans, including the group handle rather than its expanded
members. It preserves punctuation, leading/trailing spaces, CRLF, indentation,
Unicode and inert code/quote text exactly. Rendering a function prompt counts
framing bytes against the same maximum as source/input bytes.

The scanner deliberately is not a complete CommonMark or HTML interpreter.
Native resolution, immutable source identity, attachment validation, model/media
capabilities, permission grants and author-controlled promotion remain separate
requirements. No codec or credential/storage authority is granted by this move.
