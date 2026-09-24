# Published supervisor continuation: passive observation before Gemma admission

Supervisor: `990977acce6c5a00b1d2b995b8f403cc64af4479`, tree
`91b02ac6cc0ce20afbaf8d955d34df9d17bd6b1b`.
Non-EASL PR #77: `f1ff766612f7d6d1b64264fd9e7708ed2328a095`, tree
`e47084197c1bcbfefdec2373acd01e3cf6c33fd4`.
Shared ancestor: `050672127b735df0fba6c0a55dd09de23dad79e8`.

## Received evidence, not rerun native acceptance

The complete committed `CHATGPT-PRO-HANDOFF.md` is now accessible. Its later
coordinator section supersedes its older 072027a5/78a6fa37 status paragraphs.
The completion artifact belongs to `d98798301c0b1803504eb179bc69903a1944ba1a`:
archive `92169e3ccc8dc143b606a8af8db67aec18c8e29598df383f0ee6cfbec0dbe227`,
executable `ba0c023e03d6db5798c371770ed15902790f3a6a290c212dc158ebc88d9f3202`.
The handoff reports persistent-store A/B/A success, 3 browser cases, 13 Rust
tests, Clippy/helper compilation, and 540 frontend tests. These are the worker's
recorded results; this continuation did not inspect those local binaries or raw
Mac receipt files. The audit worker's Rust/native qualification stopped on ENOSPC.
EASL also stopped on ENOSPC and remains stashed and unqualified.

The new Gemma incident reports zero admitted runs, `caret_at_start`, and no
scheduled request. That is different evidence from the older eight-run incident.
The scheduler must not be changed to accept a zero-byte caret just to pass.

## Source-confirmed observer defect and narrow correction

At the exact supervisor source, the live monitor activates the application,
writes AXFrontmost, and writes AXFocused on every polling iteration, including
iterations with no admitted family. It reads the App witness and then writes
editor focus before evaluating the raw AX caret. The observer therefore changes
the state it purports to measure. Its 38 pure projection tests all pass despite
those platform-side effects.

The patch removes those three UI-mutating calls. Foreground ownership, focused
editor, exact caret, matching full rendered bytes, native event prefix, family
count, and post-observation terminal checks remain mandatory. The pure projection
code, all 38 self-test assertions, and the family/acceptance tail are byte-identical
to the authenticated original. No generation budget or deadline is increased.

Two bounded diagnostic snapshots record the first and last *observed* states:
raw AX caret/selection in UTF-16, availability/focus, exact-manuscript comparison,
App selection and scheduler caret in UTF-8 bytes, lifecycle reason and scheduled
kind. Missing measurements are distinguished from zero. App publishes scheduled
work as a string kind or null, not an object. Snapshots are diagnostic, not atomic
cross-process observations or evidence of actual generated/rendered output.

**Causal limit:** the focus writes are established by source, but this does not
prove that they caused the particular YDjxEEXg1U failure. If the passive observer
still reports a zero caret, use its first/last snapshots and the preserved App
witness to locate the transition. Do not relabel this as a proven Gemma fix.

The driver already verifies persisted text and internal/AX end-caret state before
starting the monitor (`smoke-macos-app.sh`, source 1190–1320). It also calls
`foreground_loom_process` once after starting the monitor and before enabling.
That helper actively requests foreground. If the first observed caret is correct
but the later caret changes, distinguish that driver call, the exact Cmd+Shift+G
activation, and subsequent App/editor transitions. Do not reintroduce focus or
caret repair into the observer. A lost prerequisite must remain observable.

## Executed in the browser-side working container

- New source-policy regression: before correction, 1 passed / 2 failed; after,
  3 passed / 0 failed. These check source structure, not native focus behavior.
- Actual compiled portable monitor self-test: 38 assertions before and after.
- Actual diagnostic dictionary construction: 19 explicit assertions with
  controlled AX inputs; these do not exercise Apple frameworks.
- Four deliberate structural regressions rejected: activation, AX focus write,
  lost first observation, and disconnected ordinary-CI import.
- Exact preimage and retained projection/terminal/guard/deadline blocks checked.
- macOS-target Swift parsing passed; native typechecking/linking did not run.
- Integration helper tested on temporary Git repositories, preserving both
  parents, file modes, ignored-but-tracked binary fixture and an existing stash.
  These are helper tests, not an executed merge of the real product repository.

The ordinary CI entrypoint `scripts/ci/test-ci-required.mjs` imports the three
new policy cases. The existing macOS helper build continues to compile this
monitor and execute its self-test. No new CI workflow or model service is needed.

## Integration and acceptance boundary

Merge PR #77 into a new descendant worktree of the supervisor source. The incoming
16 audit paths and the supervisor's 15 changes since their common ancestor are
disjoint. Preserve all histories, the committed supervisor handoff, and existing
worker fixes. Do not reset its original worktree or reapply old packets.

The accompanying helper checks exact commit/tree/ref and preimage identities,
compares the entire merged Git index against the expected union, applies this
patch, and leaves an uncommitted two-parent merge for review and focused tests.
It never pushes, updates main, operates on a stash or deletes original data.

Resolve build-space exhaustion using only identified reproducible build/cache
outputs, with one Rust build owner per target directory. Then finish the audit
Rust/libtest/Keychain/FTE gates and the new observer's actual macOS compilation.
Use a fresh exact combined artifact for final native qualification. Historical
A/B/A evidence is not acceptance of a newly integrated binary.

PR #77 remains the only non-EASL landing target. No additional PR is needed.
Push a normal descendant there after review/focused checks. Main promotion still
requires the settled integrated tree and exact Mom/FTE and Visual/Source
Ghost/Loompad journeys. EASL PR #73 and its stash stay out. The wider 360-degree
source audit is not closed by this continuation.
