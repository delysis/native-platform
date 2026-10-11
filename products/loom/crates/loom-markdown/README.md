# Native Loom Markdown

A safe Rust document model with no browser, EASL, window, font or storage dependency.
It replaces the source/semantic portion of Loom's ProseMirror integration. Native
text geometry belongs to `easl-native-text`; application authority stays in Loom's
shared Rust services. The view adapter must never save a widget's temporary IME
buffer as manuscript source.

Original UTF-8 is authoritative. Parsing and projection retain exact source bytes,
including reference links, inert HTML, unsupported extensions and mixed line
endings. Loom's dialect treats literal tabs as text, and retains author-entered
trailing spaces and empty paragraphs. The plain-text dialect supports verse
without Markdown interpretation. Fenced code retains literal tabs and original
line endings, including CRLF inside quotes and lists, with exact source anchors.
The final code ending belongs to the fence syntax and is hidden; preceding blank
code lines remain visible. Literal code edits preserve tabs, CR/LF/CRLF, language
metadata and container depth, and choose a fence that cannot collide with the
inserted text. Editing indented code converts only that block to a fenced block.

Transactions bind to a unique editor and revision. Bounds cover source size,
structure, parser-owned strings, edit count, and history memory. Each candidate
is parsed/projected before it can replace live state. Visual edits additionally
verify the resulting visible text and inline marks. Only affected text blocks
are serialized for inline edits. Structural edits serialize affected root
containers and validate their resulting depth and content. Exact inverse deltas
restore original bytes on undo.
Structural validation also checks list marker placement and ordered-list starts.
Block ownership is indexed once per structural transaction; leaf fragment lookup
visits its own spans rather than scanning every span for every block.
Adapters can install an admission check for their own bounded text engine. It
runs before source, selection and history change, including undo/redo. Hidden
reference definitions retain an editable body without making typing overwrite
or join the definition bytes. Definitions inside rewritten containers or paragraph
gaps are retained, including definitions used by untouched reference links.
Rewrites containing unrepresented duplicate definitions currently fail atomically.

Source and visual selections are distinct. A visual caret within a decoded entity
can be edited, but `source_selection` returns `AmbiguousBoundary` when it cannot
prove an exact byte boundary. Completion or persistence adapters must not replace
that error with a guessed source offset.

Native panes can register bounded source selections with `track_selection`.
Successful transactions, undo and redo move those endpoints atomically with the
document; failed edits leave them unchanged. Insertions have right affinity and
deleted endpoints collapse to the replacement end, snapping to whole graphemes.
Handles belong to one editor and are never reused after release. They describe
live view state and cannot authorize a persistence or generated-text splice.

Formatting palettes can capture a revision-bound visual selection with
`capture_formatting_selection`. Commands apply only to that editor and revision,
renew the capture after success, and preserve live selection/typing marks on
failure. Undo or another edit invalidates an old capture even if the source
returns to identical bytes. Views additionally retain their own pane identity
and reject capture during IME composition or literal source editing.
`formatting_state` resolves the paragraph at the ordered selection start, any
selected emphasis, the first selected link destination, and selected ancestor
quote/list containers. Link removal includes selected edge whitespace; applying
a link trims the selected inline text as in the reviewed editor. At an empty
caret, explicit typing marks win; otherwise marks follow the reviewed editor's
preceding-inline-span policy, with paragraph boundaries and noninclusive links.
Selected-range replacement inherits the first selected span instead.

Hosts whose prose storage format requires LF may explicitly call
`normalize_line_endings` at save. Source/visual selection direction is retained,
including a caret inside a decoded entity; undo restores the exact original
endings. Parsing and ordinary editing never apply this storage policy implicitly.

Implemented commands include body/headings, bold/italic, links/unlink, stored marks,
inline replacement, root paragraph splitting/joining, multiline paste, clearing,
quote/list toggles, item splitting/exit, list indentation/outdent, and bounded
undo/redo with selection direction. `type_visual` recognizes heading, quote, list,
emphasis and link input rules in bounded paragraph context. A rule and its trigger
form one undoable transaction; subsequent typing retains the inherited marks.
`replace_visual` remains literal for paste and accessibility replacement. Separate
ordered lists retain their starts using distinct CommonMark delimiter punctuation;
link destinations and titles retain literal entities and backslashes after edits.

`outdent_source` removes one leading tab or up to four spaces from the selected
source lines, matching the source editor's Shift-Tab command. It preserves other
bytes, selection direction and tracked pane selections as one transaction. A
selection ending at the next line's start excludes that line. Unindented source
returns false without adding history, so the host can continue focus traversal.

Selected-range replacement joins compatible open quote/list edges while retaining
unselected descendants. Prose/code joins retain literal code bytes and the starting
block context. Keyboard Backspace/Delete uses a separate structural command:
lifting or joining a container can retain the paragraph boundary, which the next
keypress deletes. The corpus records the original ProseMirror selection and
keyboard results; undo restores exact source and selection direction.

Code-format controls, object editing, ambiguous duplicate-reference rewrites and
complete original-editor parity remain under development. See the experiment's
`PARITY.md`; these contracts alone do not establish native frontend acceptance.

Nested toggles target the selected wrapper. Partial quote lifts retain unselected
descendant depth and ordered-item numbers. An edit that would split an existing
multi-paragraph list item into additional items is currently refused atomically.

Pinned parser: pulldown-cmark 0.13.4; serializer: pulldown-cmark-to-cmark 22.0.1.

```sh
rustup run 1.92.0 cargo test -p loom-markdown
```
