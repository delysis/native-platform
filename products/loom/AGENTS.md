# Loom Native development rules

- All project-owned Rust is safe Rust and must retain `#![forbid(unsafe_code)]`.
- Prefer explicit state machines, bounded collections and typed failures.
- Manuscript files must remain ordinary readable UTF-8 files.
- Never mutate immutable artifacts, operations, revisions, or receipts in place.
- Never let automation modify the active manuscript without an explicit promotion.
- Keep local inference and editing paths free of subprocess and loopback dependencies.
