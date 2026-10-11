# Loom / EASL material privacy verification — September 22, 2026

This receipt covers the reviewed integration of the Chat Pro follow-up packet
`loom-easl-followup-30d2a2c` into `codex/loom-easl-main-integration`.

## Source and patch identity

- Worktree base: `f7596597c440c1dfb16f8fd2f9ac7fea82c07fb8`.
- Packet checksums were verified with its `SHA256SUMS` manifest before review.
- `0002-protect-material-evidence.patch`:
  `f60adcf97976bb364d7ea31cd424758708a576aaea979d6c65c0d8af17439fa2`.
- `0003-protected-copy-materials.patch`:
  `cb76940ccf999b4fa2521c72d52a841bb63b11be541b7b02d646f66defdc36b0`.
- `0004-material-replay-schema-regression.patch`:
  `6091c04c9ad09f707d71f83b0243fb74d0fbaa05ff2d7dbed90e5a34bf76d60a`.
- All three patches passed `git apply --check --whitespace=error`. They were
  reviewed against the current source before application and formatted locally.
- The package added no dependency, lockfile, frontend or UI-reference change.

## Resulting behavior

Private-project material bindings and retained evidence now use the existing
authenticated private-sidecar codec. Readers authenticate before JSON parsing or
plaintext digest checks. Evidence IDs remain SHA-256 identities of the canonical
plaintext envelope; randomized ciphertext is only the storage representation.

Mutable bindings keep the existing atomic writer and refuse corrupt, tampered or
plaintext existing state instead of overwriting it. Immutable evidence is sealed
for its final project-relative namespace, staged as ciphertext and published by
the existing no-clobber hard-link boundary. A competing publisher is compared by
authenticated plaintext. Operation-owned staging is removed on normal errors and
unwinding without deleting the published destination.

Protected copy recognizes only `.loom/materials/bindings.json` and canonical
lowercase evidence-digest filenames. Unknown, nested and interrupted staging
names still fail closed. Copying authenticates secured input, validates evidence
plaintext against its filename digest, and reseals it for the destination vault.

The already-repaired replay schema now has an explicit regression: flattened
retrieval evidence owns the `materials` array, while the rich context plan owns
`material_plan`. The former collision shape and explicit null retrieval list are
rejected rather than guessed or migrated.

## Executed checks

Environment: macOS arm64 (`Darwin 24.6.0`), Rust 1.92.0
`ded5c06cf21d2b93bffd5d884aa6e96934ee4234`, Cargo 1.92.0, optimized
`native-view` profile, shared target directory, one Cargo process at a time.

| Check | Result |
| --- | --- |
| Affected-crate `cargo check --offline --locked --profile native-view --tests` | Passed |
| Material privacy regressions | 15 passed, 0 failed |
| Protected-copy regressions | 9 passed, 0 failed |
| Profile/replay regressions | 4 passed, 0 failed |
| Function-configuration integration | 3 passed, 0 failed |
| Full `tauri-plugin-loom --lib` | 354 passed, 0 failed, 3 ignored |
| Full `loom-store --lib` | 167 passed, 0 failed |
| Strict Clippy for the four affected Loom crates, all targets | Passed |
| Reusable `easl-text`, `easl-native-text`, `loom-markdown`, and `fsexp` tests | 369 passed, 0 failed |
| Strict Clippy for those four reusable crates, all targets, no dependencies | Passed |
| Svelte check | 0 errors, 0 warnings |
| Frontend unit tests | 68 files, 491 tests passed |
| `xtask policy` | Passed |
| `cargo fmt --all -- --check` and `git diff --check` | Passed |

The first reusable-crate attempt exhausted the host volume while building
`skrifa`. Cargo removed the unused 10.7 GiB development-profile cache; the
`native-view` cache and sources were preserved. The exact test command then
completed successfully.

## Honest native-interface boundary

The combined native component gate still stops at the intentional
`loom-easl-interface` build guard. Current `App.svelte` hashes to
`6667474039c166a5a33b1f4e2cd23e5cca79353962c73b1df4ac0e5c3552709a`, while
the reviewed native port is pinned to
`3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725`.
The guard was not changed because the current Add menu, pane states,
Ghost/Loompad control, theme, icons, focus and accessibility behavior have not
yet been ported and reviewed together.

No foreground application was launched. This receipt is component evidence,
not signed-bundle, OS input, IME, accessibility, save/relaunch or visual-parity
acceptance. Remaining storage work includes the broader private-writer and
ancestor-race audit described in the Chat Pro handoff; abrupt process termination
may also leave encrypted evidence staging that later inventory correctly rejects.
