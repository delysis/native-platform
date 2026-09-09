# Mom Llama product contract

This subtree owns Mom Llama. Shared native, Speech, Information, and Attachment
crates live under the monorepo `crates/` tree. FTE remains a separate product.

## Product hierarchy

1. `docs/PRODUCT.md`
2. `contracts/upstream-parity.json`
3. `contracts/effects.json`
4. `contracts/commands.json`
5. `docs/MODULE_BOUNDARIES.md`
6. This file
7. Comments and historical receipts

## Non-negotiables

- Use safe, idiomatic Rust. Keep authority narrow and typed.
- Normal local inference is in-process through the pinned native-kit crates.
- Mom does not compose FTE, its Tauri plugin, hosted providers, or loopback.
- Rust owns state, validation, persistence, policy, inference and receipts.
- The webview owns transient presentation only.
- Product operations are CLI-exercisable before the GUI enables them.
- Fake/fixture evidence never promotes real readiness.
- Cache mismatch is an ordinary miss followed by generation.
- Only the explicit MCP adapter may spawn a process; normal product/runtime
  code has no network or process authority.
- Do not add native engine crates, attachment parsers, provider implementations,
  speech backends or loopback servers here. Consume their typed public
  boundaries.
- Keep user data and credentials intact. These products are unreleased; remove
  unused compatibility paths and reject incompatible stores instead of inventing migrations.

## Native UI discipline

- Use screen space purposefully: compact adaptive layouts, integrated titlebar
  controls and native edge resizing, with comfortable hit targets. Help and
  secondary status must not reserve permanent empty space around the work.
- Ordinary chat stays calm: no permanent dashboards, empty result-card grids,
  inline documentation rails or exposed settings on the conversation surface.
- Use established component classes and existing Lucide/upstream llama.cpp
  icons. No unstyled platform buttons or new visual treatments for existing
  action levels.
- Settings autosave. Success is a brief neutral glyph; actionable failure is
  persistent and retryable; user edits are never discarded.
- Message actions are contextual, not permanently visible.
- Every visible control has command, CLI, effect, blocker and test metadata.
- Visual changes require desktop and compact rendered review.

## Verification

Run the commands listed in `README.md`. Real-model, cache-restart and Tauri
bundle gates are explicit opt-in acceptance proofs.
