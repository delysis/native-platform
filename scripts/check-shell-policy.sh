#!/bin/sh
set -eu

# Cargo, rustc, rustdoc, and Clippy must resolve from the same pinned toolchain.
PINNED_CARGO=$(rustup which --toolchain 1.95.0 cargo)
export PATH="$(dirname "$PINNED_CARGO"):$PATH"

rustc --version --verbose
cargo --version
pnpm --version
node scripts/build-loom-signal.mjs
cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run --locked -p xtask -- policy
node --test scripts/ci/test-ci-metadata-selection.mjs scripts/ci/test-ci-plan.mjs scripts/ci/test-ci-required.mjs scripts/ci/test-ignored-tests.mjs scripts/ci/test-product-state-backup.mjs scripts/ci/test-workflows.mjs
node --test scripts/ci/test-current-docs.mjs
node scripts/ci/validate-current-docs.mjs
./crates/native/scripts/check-architecture.sh
./crates/services/attachment/scripts/check-boundaries.sh
./crates/services/information/scripts/check-boundaries.sh
./crates/services/speech/scripts/check-boundaries.sh
pnpm install --lockfile-only --frozen-lockfile --offline
