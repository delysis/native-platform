# PR #77 native continuation

## Published integration head

- Branches: `codex/pr75-astra-handoff-20260922` and
  `codex/pr77-native-focus-stabilization-20260924`
- Qualified merge before this handoff: `c12501241c08e05c154c66023a124dd712536456`
  (`1e5d3a783cbece179dfc281f16a48fd4c5db0f4b` tree)
- Current head before this handoff document: `8ce4e500076edffb1cb23fa38d5054a62ed84281`
  (`49188b111ff0ddfc7cbd61e8d4d8376ba660fdfd` tree)

The merge-tree local command plan passed workspace tests and doctests, strict
Clippy, ignored-test reconciliation, all frontend checks, 553 Loom unit tests,
165 real WebKit tests, and compilation/self-tests for 19 macOS smoke helpers.
The later exact-control helper change passed the 50-test workflow contract and
compiled independently. The candidate release builder also passed its exact
Rust controls and 553 Loom tests.

## Repairs made after the AppKit observer merge

1. `e2ebc645` makes the native input helper wait until the AX value and collapsed
   caret are jointly stable before posting the required real Space key event.
   This prevents the event from accepting a stale platform prediction after an
   asynchronous WebKit AX replacement. The key event remains mandatory.
2. `8ce4e500` replaces substring matching in the new-document helper with a
   unique exact accessible-name match. Only the documented parenthesized
   shortcut suffix is accepted for `New document`; unrelated `Add ...` controls
   cannot satisfy the toolbar selector.

## Exact candidate and native observations

Candidate:

`dist/macos/loom-v0.1.0-8ce4e500076e-20260924T215046Z/Loom.app.zip`

Its adjacent `release-receipt.json` binds the archive to `8ce4e500`. The previous
`c1250124` candidate is retained separately under its own timestamped directory.

The `c1250124` native run used a real local Gemma 4 E2B base model and produced
four completed, correlated candidates. It passed the terminal-space boundary,
real visual presentation, formatting journey, and the 75-second hide/resume
observation. It then failed because the old new-document helper selected controls
by substring. Raw root (preserve byte-for-byte, including SQLite/WAL/SHM):

`/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-loom-smoke.XXXXXX.rFoHrV9kPw`

The `8ce4e500` native run encountered a distinct earlier failure. Its real
four-run family completed, but the selected first candidate began with HTML and
the live observer ended `no_correlated_inline_render`. Do not convert this run
to acceptance and do not repeatedly redispatch it unchanged. Raw root (also
preserve byte-for-byte):

`/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-loom-smoke.XXXXXX.qe6wE9AeXc`

No SQL, WAL, or SHM file was deleted or modified during diagnosis.

## Remaining source question

Ordinary ChatGPT 6 Pro reviewed the controller sources and the second failure.
Its delivered response is retained locally at:

`/Users/george/.codex/evidence/chat-pro-unpresentable-family-8ce4e500-run1/response.md`

The review correctly distinguishes what the capture proves from what it does
not. The capture does not prove that the visual rejection callback fired,
because the final observation also lost foreground/focus. Independently, source
inspection establishes a rejection-to-selection gap:

- `rejectVisualPresentation` records the presentation key but does not advance
  the retained session selection or `activeRunId`.
- `completionActiveFamily` returns the cached unconsumed session candidates
  without excluding acknowledged visual rejections.
- `completionControllerView` can therefore continue selecting the rejected key.

Start with a failing direct controller regression. If repaired, selection,
active run, and witness selection must agree on the next eligible member from
the existing family. Preserve family order, bodies, hashes, and run IDs; do not
regenerate, salvage prose from inside HTML, cap all previews to one word, inflate
deadlines/guards, or use hosted fallback.

Automatic fallback is allowed only for an untouched default visual suggestion.
It must not override explicit writer cycling/pinning, accepted chunks, frozen
authority, rollback identity, Source-mode presentation, or a temporary loss of
focus/visibility. If no family member is presentable, settle with no visual
selection and no replacement generation.

## Still open before promotion

- Prove the rejection/selection behavior with direct controller controls and a
  mounted real-WebKit correlation test, then rerun the exact packaged journey.
- Complete the separate persistent-root A/B/A sequence on the final exact
  archive, including warm-writer re-enabling and save/reopen/shutdown.
- Run one final consolidated gate on the exact resulting integration tree.
- Keep EASL outside promotion until its independent work order qualifies it.
- Hosted CI run `35807244934` remains a zero-planner-step infrastructure
  failure of unestablished cause; do not hunt for an imaginary compiler error
  or repeatedly redispatch unchanged CI.

Separate green branch runs and the partial native observations above do not
qualify promotion. After authorized promotion, verify main's tree equals the
tested integration tree.
