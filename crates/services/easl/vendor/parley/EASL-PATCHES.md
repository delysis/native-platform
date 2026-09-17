# Parley 0.11.1 for native EASL text

Source: crates.io `parley` 0.11.1, tag `v0.11.1`, commit
`eea3503dd6cf17130cbb07348e0ff2c918300e94` at
https://github.com/linebender/parley. Apache-2.0 OR MIT; licenses retained.
Original file hashes and import authorization are in
`migration/easl-native-text-import.json` at the workspace root.

Local corrections exercised through the public `easl-native-text` contracts:

- Record full-source ICU grapheme boundaries after shaping, using final source
  offsets, including RTL runs. Natural/emergency wrapping and overflowing spaces
  respect those boundaries across fallback runs. CRLF remains intact.
- Eight-space tab stops use the current line advance. Candidate tab widths follow
  line-break checkpoints and reset for reflow.
- Soft hyphens retain invisible discretionary glyphs; a chosen break admits and
  activates the hyphen's width. Preceding word context supplies kerning, subject to
  a bounded shaping-work budget. Complex-script contextual substitution of the
  preceding word is not a complete discretionary reshaping implementation.
- Widen cluster map lengths, glyph counts and source offsets. Large graphemes no
  longer overflow u8 or silently drop glyphs after 32 entries; long font runs do
  not wrap source offsets at 64 KiB. Ordinary small clusters retain stack storage.
- Force paragraph separators into separate shaping runs so newline + combining
  mark does not merge into an asserted-invalid newline cluster.
- Empty line data has an empty source range. Forced breaks preserve a trailing
  explicit newline and account for activated discretionary hyphens.
- Expose selected-line hit testing for column layouts, and bounded paragraph
  optimization metadata (`can_break_before`, `discretionary_advance`, wrapping).
- Accept validated ranged style sets in the native editor. Keep their ranges
  aligned through replacement and temporary IME composition so rich glyphs,
  caret/selection geometry and the composition area share one shaped layout.
- Editor paragraph boxes supply left/right insets, first-line indentation and
  before/after spacing to the line breaker. Line origins also contribute to
  cluster/caret offsets. Empty final paragraphs use an independently shaped font
  strut, including heading-to-body transitions, without adding source characters.
  Empty ranged styles supply that strut instead of entering character analysis.
  `paragraph_editor` and native field contracts exercise wrapping, clipping,
  caret/click agreement, composition, trailing empties and admission failures.
- Expose physical-line lookup and multi-line editor movement for viewport-sized
  Page Up/Down navigation. Retain the horizontal position and selection anchor
  through short lines. An EOF caret on an empty final line uses that line's
  geometry even after an RTL paragraph break.
- Accept externally hit-tested byte/affinity targets without repeating pointer
  hit testing. Preserve word/line drag anchors and shift-click semantics. Word
  ranges expand to whole graphemes before storing those anchors, including the
  split native clusters of a CRLF; empty final lines retain their own caret.
  The shared `easl-text` pointer adapter exercises this API against shaped geometry.
- Expose the retained horizontal position and accept external keyboard targets
  with explicit anchor extension and preferred-column state. The EASL navigator
  owns line/page/document policy; Parley retains shaped geometry and view state.

Regression evidence includes grapheme/fallback, tabs, soft hyphens, empty text,
72 KiB font runs, 300-mark graphemes, multi-column hit testing, optimized paragraphs,
and the full Pretext retained corpus replay (177,065 layouts). This replay checks
native invariants; it does not prove browser geometry equivalence.

The dependency is excluded from primary package groups. Upstream source tests are
retained; workspace tests exercise the native text service that consumes it.
