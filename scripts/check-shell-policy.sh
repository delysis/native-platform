#!/bin/sh
set -eu

rustup run 1.92.0 rustc --version --verbose
rustup run 1.92.0 cargo --version
pnpm --version
rustup run 1.92.0 cargo fmt --all -- --check
rustup run 1.92.0 cargo test --locked --workspace --all-targets
rustup run 1.92.0 cargo test --locked -p llama-native-engine --features unstable-w1-contract-tests,unstable-w1-vertical-tests --all-targets
rustup run 1.92.0 cargo clippy --locked --workspace --all-targets -- -D warnings
rustup run 1.92.0 cargo run --locked -p xtask -- policy
./crates/native/scripts/check-architecture.sh
./crates/native/scripts/check-workflow-policy.sh .
./crates/native/tests/workflow_policy.sh
pnpm install --lockfile-only --frozen-lockfile --offline
