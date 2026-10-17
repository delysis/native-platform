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
- Settings occupy a compact right sidebar while the conversation remains usable.
  Hidden message actions reserve no layout space; avoid repeated headings,
  decorative cards and fixed reading gutters that crowd the work.
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

For voice changes, follow `docs/VOICE_RELIABILITY.md`. Keep the installed app and
user state stable while developing in an isolated checkout. One integrator owns
the source and serialized model/build slot. Preserve failing receipts; a short
CPU test cannot qualify long Metal cache reuse. Require long-prefix native
import/edit parity, repeated cancellation, and persisted-cache restart evidence
for affected cache changes. Bind every measurement to its source, model, device
and executable; never infer the running binary from the latest build directory.
Build review candidates with `scripts/build-voice-candidate.mjs` after the exact
clean-tree component gate. Candidate app identity and state stay separate from
the installed app. Component success does not authorize promotion as a tested
voice release: packaged call, physical echo/double-talk, sustained-call soak and
target-device results remain distinct acceptance records. Never replace the
user's working app based only on unit tests or a successful launch.
