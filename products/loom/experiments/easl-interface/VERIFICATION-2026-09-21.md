# Loom / EASL continuation verification — September 21, 2026

This receipt covers the continuation patch applied to
`codex/loom-easl-main-integration` after reviewing
`loom-easl-handoff-2026-09-21` from Downloads.

## Source and change identity

- Base branch before this work: `38aede4988c19949cf33f031499897e6d36ed935`.
- Downloaded patch: `0001-repair-protected-original-tests.patch`, applied with
  `git apply --whitespace=error`.
- Changed files: the three protected-original test-bearing Rust files, the
  generation-profile replay key, its producer, and one Svelte test fixture.
- No UI reference hashes, production attachment storage code, dependencies or
  lockfiles changed.
- The new `material_plan` JSON key avoids colliding with the flattened
  retrieval evidence `materials` vector. Before this fix, one replay test failed
  during deserialization and four server-weave tests surfaced the same problem
  as misleading idempotency conflicts.

## Checks

Environment: macOS arm64, Rust 1.92.0, optimized `native-view` profile, shared
target directory, one Cargo process at a time.

| Check | Result |
| --- | --- |
| `cargo check --offline --locked --profile native-view -p loom-config -p loom-text-session -p tauri-plugin-loom --tests` | Passed |
| Protected-original focused export tests | 6 passed, 0 failed |
| Full `tauri-plugin-loom --lib` | 338 passed, 0 failed, 3 ignored |
| Server-weave regression filter | 4 passed, 0 failed |
| Frozen-profile replay regression | 1 passed, 0 failed |
| `loom-text-session --test function_configuration` | 3 passed, 0 failed |
| `pnpm --filter @delysis/loom check` | 0 errors, 0 warnings |
| `pnpm --filter @delysis/loom test` | 68 files, 491 tests passed |
| Strict Clippy for loom-config, loom-text-session, tauri-plugin-loom | Passed |
| `cargo run --offline --locked --profile native-view -p xtask -- policy` | Passed |
| `cargo fmt --all -- --check` | Passed |
| `git diff --check` | Passed |

The protected-original tests cover exact Unicode/CRLF export, authenticated
secured objects/manifests, tampering, ciphertext substitution, plaintext
downgrade, vault loss and no-clobber behavior. These are synthetic component
tests; they do not establish Keychain, signed-bundle, relaunch or OS acceptance.

## Remaining blocker

The Downloads handoff identified a separate production storage defect: retained
material evidence and bindings appear to be serialized as cleartext JSON under
`.loom/materials`, bypassing the private sidecar codec. This continuation does
not claim that defect fixed. A follow-up production patch must route private
material evidence/bindings through the existing vault boundary, preserve ordinary
project behavior and immutable identity, reject plaintext downgrade/tampering,
and add secured-project integration tests before private-project acceptance.

Current EASL interface parity, native OS interaction, IME/accessibility,
signed/relaunch lifecycle, full Pretext differential coverage and the remaining
handoff inventory remain open. No foreground window was launched here.
