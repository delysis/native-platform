#!/bin/sh
set -eu

rustup run 1.92.0 rustc --version --verbose
rustup run 1.92.0 cargo --version
pnpm --version
rustup run 1.92.0 cargo fmt --all -- --check
rustup run 1.92.0 cargo test --locked --workspace --all-targets
rustup run 1.92.0 cargo clippy --locked --workspace --all-targets -- -D warnings
rustup run 1.92.0 cargo run --locked -p xtask -- policy
node --test scripts/ci/test-ci-plan.mjs scripts/ci/test-ci-required.mjs scripts/ci/test-workflows.mjs
./crates/native/scripts/check-architecture.sh
./crates/services/attachment/scripts/check-boundaries.sh
./crates/services/information/scripts/check-boundaries.sh
./crates/services/speech/scripts/check-boundaries.sh
pnpm install --lockfile-only --frozen-lockfile --offline
