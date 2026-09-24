# Loom Native development rules

- All project-owned Rust is safe Rust and must retain `#![forbid(unsafe_code)]`.
- Prefer explicit state machines, bounded collections and typed failures.
- Manuscript files must remain ordinary readable UTF-8 files.
- Never mutate immutable artifacts, operations, revisions, or receipts in place.
- Never let automation modify the active manuscript without an explicit promotion.
- Keep local inference and editing paths free of subprocess and loopback dependencies.

## Completion is a product invariant

Read [the streaming contract](docs/streaming-completion.md) and
[the hydration/admission continuation](../../docs/reviews/2026-09-22-hydration-admission.md)
before changing completion, rendering, model activation, panes or acceptance automation.
Full authorized previews must stream; word-sized acceptance is a separate operation.
A ready terminal row is not a verified candidate body. Do not spend the retry budget
on an unusable partial while terminal SHA hydration is outstanding.

Run the real-App delayed-body regression and focused unit/browser tests. Assert actual
DOM text and exact insertion bytes, not just keys, labels or controller state. Changes
to an observer need a negative control for its old wrong decision. Fixtures and
component counts never establish native inference or visible product acceptance.
Do not promote a completion-affecting tree until its exact artifact has passed the
approved Visual/Source/Ghost/Loompad journey, including owned-worker quit and relaunch.
