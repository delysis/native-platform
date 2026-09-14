# Loom Native development rules

- All project-owned Rust is safe Rust and must retain `#![forbid(unsafe_code)]`.
- Prefer explicit state machines, bounded collections and typed failures.
- Manuscript files must remain ordinary readable UTF-8 files.
- New macOS desktop projects encrypt private history, drafts, context and inference
  payloads. Ordinary manuscripts, authored config, public assets and manuscript
  recovery captures remain readable. Existing projects require explicit
  `loom protect-copy`; never silently rewrite or erase their plaintext source.
- Local builds intended for repeated use must be signed with
  `scripts/sign-mine-development.sh` (or `pnpm --filter @delysis/loom bundle:dev`).
  Its stable development signing identity preserves Keychain trust while review
  bundles may have distinct display/bundle IDs. Never substitute predictable keys
  or unrestricted Keychain access to suppress password prompts.
- Never mutate immutable artifacts, operations, revisions, or receipts in place.
- Never let automation modify the active manuscript without an explicit promotion.
- Keep local inference and editing paths free of subprocess and loopback dependencies.
