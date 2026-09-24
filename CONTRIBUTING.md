# Contributing

Use safe, idiomatic Rust, explicit ownership, workspace dependencies and lints.
These products are unreleased: prefer one current format and explicit rejection
of incompatible data to unused migration machinery. Preserve user prose,
credentials and archived evidence. Test observable effects, failure and
cancellation through the actual adapter, store or worker; do not weaken tests.

## Tools and focused work

Run from a complete native-platform checkout, not a selective source archive.
Required locally: Git with the history used by the documentation policy; Rust
1.92.0 with rustfmt and Clippy; Node 22 with npm/npx; pnpm 11.16.0; CMake; and
macOS developer tools providing `xcrun`, Swift, Clang and an SDK. Install missing
prerequisites deliberately, outside the gate. The runner installs no toolchain,
global package manager, browser or system package.

Pin **actual binaries**, not just rustup's override: Homebrew Cargo may precede
rustup on PATH. In the coordinator's shell, from the repository root:

```sh
export RUSTUP_TOOLCHAIN=1.92.0
export CARGO="$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" cargo)"
export RUSTC="$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" rustc)"
export RUSTDOC="$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" rustdoc)"
export RUSTFMT="$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" rustfmt)"
export PATH="$(dirname "$CARGO"):$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
"$CARGO" --version
"$RUSTC" --version
"$RUSTDOC" --version
```

Prime prerequisites explicitly when needed; these commands can fetch dependencies
but do not replace global pnpm (which may be 11.19.0):

```sh
npx --yes pnpm@11.16.0 --version
"$CARGO" fetch --locked
npx --yes pnpm@11.16.0 install --frozen-lockfile
npx --yes pnpm@11.16.0 --filter @delysis/loom exec playwright install webkit
```

The runner requires the pinned pnpm to be available to
`npx --offline --yes=false pnpm@11.16.0` and uses frozen/offline frontend
installation. Cargo uses locked dependencies with its normal native build setup.
Leave `CARGO_NET_OFFLINE`, `ORT_OFFLINE` and `ORT_SKIP_DOWNLOAD` unset: the pinned
ONNX build script treats offline mode as disabling its binary resolution entirely,
even when the verified runtime is already cached. Missing native inputs may be
downloaded by the existing build scripts; failures never authorize substituting
tools or skipping checks. This is not a network sandbox. No environment/credential
dump is collected.

Use focused checks while editing, including the runner regressions when relevant:

```sh
"$CARGO" test --locked -p xtask local_ci::tests
"$CARGO" run --locked -p xtask -- policy
node scripts/ci/cargo-group.mjs test <group>
node scripts/ci/cargo-group.mjs clippy <group>
"$CARGO" fmt --all -- --check
```

Keep package groups complete; the existing selectors own affected-consumer
selection. Before running guarded Node checks manually, follow CI-POLICY.md's
compiler/target environment rule rather than weakening the guard.

## One clean macOS component gate

Review and commit locally first. Dirty tracked, staged or untracked files refuse
qualification, as do sparse/assume-unchanged index entries or Git worktree/index
environment overrides. The gate never commits, resets, cleans, merges or pushes. Stop other Cargo work
sharing the target first.

```sh
mkdir -p "$HOME/.codex/evidence"
# Choose a new leaf for EVERY attempt; do not create that leaf yourself.
"$CARGO" run --locked -p xtask -- local-ci \
  "$HOME/.codex/evidence/local-ci-$(git rev-parse HEAD)-attempt-01"
```

Reuse one target, not per-step/per-receipt targets. The unchanged ignored-test
guard forbids `CARGO_TARGET_DIR`: the runner verifies that the configured cache
and the checkout's `target` resolve to the **same physical directory**, then
removes that override only in child environments that use the default path.
A pre-existing integrator-managed `target` symlink to the same cache is supported;
the runner creates or replaces no link. A symlink may not redirect builds into
another source directory. A different cache is rejected, not silently abandoned
for a second build. Unset `CARGO_BUILD_TARGET_DIR`.

Cargo's macOS launcher adds `DYLD_FALLBACK_LIBRARY_PATH`. The runner permits only
paths in the shared target, pinned toolchain libraries and Cargo's standard
macOS fallback locations, then records/removes that launcher variable for child
commands. Custom loader paths must be unset before launch; other prohibited
compiler/loader/flag overrides remain errors in the unchanged guard.

A cooperative `target/.local-ci.lock` excludes other local-ci instances, not
arbitrary Cargo commands. On interruption, inspect the referenced receipt and
ensure the runner and its descendants have exited before manually removing a
stale lock. Do not steal a live lock. Never delete failed receipts or user state.
A launcher compilation failure occurs before the runner can create evidence;
retain its command, output and exit status separately. See
[CI policy](docs/CI-POLICY.md) for receipt interpretation and acceptance limits.

Keep opt-in tests in `ci/ignored-tests.json` with exact identities and real
prerequisites. Unsupported platforms must fail before state mutation; retain
portable tests and tests of that unsupported boundary. Historical ledgers are
provenance, not current implementation instructions.
