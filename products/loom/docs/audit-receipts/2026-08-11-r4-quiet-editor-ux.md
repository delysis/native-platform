# R4 quiet-editor UX correction receipt

Date: 2026-08-11
Evidence class: designated-machine product acceptance, not cross-platform certification

## Why this receipt exists

Direct author review rejected the prior UI: editor Tab could traverse focus and
expose a `Skip to manuscript` control, the quiet header advertised an unexplained
alternatives count, and the dialog's `Use this` path displaced caret-local
autocomplete. That observation withdraws the earlier build as current product
acceptance even though its storage and inference evidence remains historical.

## Immutable inputs

- Loom base: `79fb322c8c950cea8cc0659019cae660270369c8`
- Native dependency: `f7a69316c64d857b99bd847dd44cd852fc5b4ca4`
- UX source-diff SHA-256 before this receipt: `9dfcd2fe1cd81b21263fd4ea4551c52db33a58218ce957fe3cc38f605ac1fc55`
- Bundle identifier: `app.delysis.loom.r4ux3.f7a693.acceptance`
- Unsigned debug executable: 65,008,344 bytes, SHA-256 `14e8f3f37eb9128dbc7a3c9ef6cb47b7f0b72c6b598440186150138afd965455`
- Model: Gemma 4 E2B base Q8_0 GGUF, 4,954,576,032 bytes, SHA-256 `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`
- Observed host: Apple M4 Max Metal

The model path is omitted because it is local operational state, not artifact
identity. The native successor was resolved through a local Git URL rewrite and
is not remotely buildable until that immutable commit is published.

## Exercised sequence

1. Launched the uniquely identified bundle into a new app-local workspace. The
   full accessibility tree contained the title, one Writing options disclosure,
   and the manuscript. It contained neither a skip link nor a review/count control.
2. With an empty Visual manuscript and no candidate, pressed Tab once. Focus
   remained in the editor and the visible file became exactly one byte `09`.
3. Sent Cmd+Q, waited for the process to exit, and immediately relaunched the
   same exact bundle. The Visual editor reopened the preserved U+0009 manuscript.
4. Replaced the manuscript with the exact human prefix `At dawn, the locked
   observatory began to breathe.` and waited for three real local completions.
5. Observed muted continuation text beginning at the exact caret with no
   alternatives control in the header.
6. Pressed Tab once. The live keydown witness promoted the visible candidate;
   the accessibility status became `Suggestion accepted` rather than inserting
   a fallback tab or moving focus.
7. Queried the project SQLite store and ordinary UTF-8 manuscript. The selection
   event bound the candidate, source revision, resulting revision, and command;
   the visible file contained the accepted continuation.
8. Opened Writing options and selected Source editor. The menu closed, focus
   moved to the new textarea, and Tab appended a literal U+0009 byte there too.
9. Sent Cmd+Q and immediately relaunched the same bundle. The accepted text and
   trailing literal tab reopened in the Visual surface; the full tree still had
   no skip link or quiet-header review count.

## Durable identities

- Selection: `01KZSCPKBRJXV217GB3B6WS3HY`
- Candidate: `01KZSCP467QSQ0Q610THVG3AC3`
- Source revision: `01KZSCP1D9V70KX5TBYBW8P9M0`
- Resulting revision: `01KZSCPKBR4W0NESD6RZ8KYP7Y`
- Promotion command: `01KZSCPKB9AFFA3C6MWB1W12EG`

## Automated checks and claim boundary

The focused UX suite passed 53 tests across ghost, source-ghost, Markdown-safety,
and App-reactivity files; the full frontend gate is recorded as 29 files and 173
tests. Svelte diagnostics reported zero errors and warnings, and the production
frontend build passed with the existing chunk-size advisory.

This proves the named bundle's current macOS interaction only. It does not
certify screen readers, other operating systems, signed packaging, model quality,
latency, or every cursor/IME/layout combination. Reviewable candidates remain
durable; only candidates presentable by the active editor are exposed under the
progressive Writing options control. Visual mode's literal-tab behavior is a
documented Loom Markdown dialect: U+0009 means manuscript indentation, not
CommonMark tab-indented code. That external syntax remains a Source-mode concern.
