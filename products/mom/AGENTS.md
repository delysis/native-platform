# Llama Native Kit implementation contract

This repository owns one reusable, process-free llama.cpp runtime and one
reference product: Mom Llama.

## Product hierarchy

1. `docs/PRODUCT.md`
2. `contracts/upstream-parity.json`
3. `contracts/effects.json`
4. `contracts/commands.json`
5. This file
6. Comments and historical receipts

## Non-negotiables

- Normal inference is in-process. Inference crates may not use networking,
  subprocesses, shell commands, or executable discovery.
- All llama.cpp objects stay on their owning worker thread. Binding objects and
  raw pointers never cross the public Rust API.
- Project-owned unsafe code is forbidden except in the documented native state
  buffer module, where every block must state its safety argument.
- Backend operations are CLI-exercisable before the GUI enables them.
- Fixture engines are permanently labeled fixtures and never unlock runtime
  readiness.
- Persistent sensitive content is authenticated and encrypted at rest.
- Cache reuse is an optimization. A fingerprint mismatch invalidates the cache
  and falls back to ordinary generation.
- Rust owns authoritative state, validation, persistence, policy, inference and
  receipts. The webview owns only transient presentation behavior.
- The upstream UI is pinned by commit. Parity claims require command tests and
  same-state visual evidence.
- Do not claim clinical validity, impersonation, endorsement, HIPAA compliance,
  user acceptance or release without the corresponding evidence.

## Verification

Run, at minimum:

```sh
cargo fmt --all --check
cargo test --workspace --exclude mom-llama-app
cargo clippy --workspace --all-targets --exclude mom-llama-app -- -D warnings
node --check apps/mom-llama/ui/coop-hx.js
./scripts/check-architecture.sh
./scripts/check-contracts.sh
```

Real-model and Tauri bundle checks are separate opt-in acceptance gates.

## Native UI visual discipline

- Keep the ordinary chat surface calm. Conversation configuration belongs in
  Settings or a transient contextual surface, never in a permanent dashboard,
  card rail, or instructional banner.
- Every visible button must use an established component class from
  `apps/mom-llama/ui/style.css`. Do not ship an unstyled platform-default
  button, a one-off class without CSS, or a new visual treatment for an
  existing action hierarchy.
- Use `primary-button` only for the single principal action in a local task.
  Use `secondary-button` or `small-button` for ordinary actions,
  `icon-button` for compact icon-only actions with an accessible label, and
  `danger` only for destructive actions.
- Settings autosave. Do not add a persistent Save button. Use a small Lucide
  progress glyph while saving, show success only as a brief neutral check that
  disappears, and reserve persistent text plus an explicit Retry action for a
  failure that needs attention. Never discard the user's edits.
- Prefer existing Lucide/upstream llama.cpp icons and design tokens. New icons,
  colors, radii, shadows, or button variants require a demonstrated semantic
  need and a rendered desktop/compact regression check.
- The deterministic view test must reject buttons that do not use an approved
  component class. Visual review is required after changing navigation,
  settings, composer, dialogs, or message presentation.
