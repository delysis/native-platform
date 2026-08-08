# Mom Llama product contract

This repository owns one product: Mom Llama. Reusable llama.cpp internals live
in `delysis/llama-native-kit`; routing/protocol/provider modules live in
`delysis/free-token-energy`; local STT/TTS lives in
`delysis/speech-native-kit`.

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
- Rust owns state, validation, persistence, policy, inference and receipts.
- The webview owns transient presentation only.
- Product operations are CLI-exercisable before the GUI enables them.
- Fake/fixture evidence never promotes real readiness.
- Cache mismatch is an ordinary miss followed by generation.
- Only the explicit MCP adapter may spawn a process; normal product/runtime
  code has no network or process authority.
- Do not add native engine crates, provider implementations, speech backends or
  loopback servers here. Consume their typed public boundaries.
- Preserve current data/Keychain/Tauri identifiers until a tested additive
  migration verifies read-back and rollback.

## Native UI discipline

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
