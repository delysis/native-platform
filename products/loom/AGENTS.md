# Loom Native development rules

- All project-owned Rust is safe Rust and must retain `#![forbid(unsafe_code)]`.
- Prefer explicit state machines, bounded collections and typed failures.
- Manuscript files must remain ordinary readable UTF-8 files.
- Never mutate immutable artifacts, operations, revisions, or receipts in place.
- Never let automation modify the active manuscript without an explicit promotion.
- Keep local inference and editing paths free of subprocess and loopback dependencies.

## Completion is a product invariant

Read [the completion boundary record](../../docs/reviews/2026-09-22-ghost-observation.md)
before changing rendering, completion, model activation, workspace/panes or
native acceptance automation. Run the real-App browser regression as well as
focused unit tests. Keep display text, immutable candidate bytes and editor
insertion bytes distinct. A key, label or controller state is not a visible
glyph. Observer changes need a negative control for the old wrong decision.
Model fixtures and browser tests do not authorize native product acceptance.
Do not promote a completion-affecting tree until its exact artifact has passed
the approved native Visual/Source/Ghost/Loompad journey and restart/quit checks.
