# Loom Native development rules

- All project-owned Rust is safe Rust and must retain `#![forbid(unsafe_code)]`.
- Prefer explicit state machines, bounded collections and typed failures.
- Manuscript files must remain ordinary readable UTF-8 files.
- Never mutate immutable artifacts, operations, revisions, or receipts in place.
- Never let automation modify the active manuscript without an explicit promotion.
- Keep local inference and editing paths free of subprocess and loopback dependencies.

## Completion changes

Read `docs/streaming-completion.md` before changing rendering, inference delivery,
family selection, editor input, or acceptance automation. Full streaming previews
and word-at-a-time insertion are different contracts. Keep a failing behavioral
regression before repairing either; assert actual editor DOM bytes, not hidden
candidate state. The normal unit and WebKit suites must retain these regressions.
A failed or unavailable exact-artifact native writing journey blocks product
acceptance and promotion, even when every component/build gate is green.
