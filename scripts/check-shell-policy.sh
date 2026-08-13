#!/bin/sh
set -eu

rustup run 1.92.0 rustc --version --verbose
rustup run 1.92.0 cargo --version
pnpm --version
rustup run 1.92.0 cargo fmt --all -- --check
rustup run 1.92.0 cargo test --locked --workspace --all-targets
rustup run 1.92.0 cargo test --locked -p llama-native-engine --features unstable-w1-contract-tests,unstable-w1-vertical-tests --all-targets
rustup run 1.92.0 cargo test --locked -p information-native-acquire --features unstable-w1-contracts
rustup run 1.92.0 cargo test --locked -p information-native-host --features unstable-w1-vertical-tests --test w1_vertical
rustup run 1.92.0 cargo test --locked -p speech-native-host --all-targets --features unstable-w1-contract-tests,unstable-w1-vertical-tests
rustup run 1.92.0 cargo clippy --locked -p speech-native-host --all-targets --features unstable-w1-contract-tests,unstable-w1-vertical-tests -- -D warnings
rustup run 1.92.0 cargo clippy --locked --workspace --all-targets -- -D warnings
rustup run 1.92.0 cargo run --locked -p xtask -- policy
node --test scripts/ci/test-ci-plan.mjs scripts/ci/test-ci-required.mjs scripts/ci/test-workflows.mjs
./crates/native/scripts/check-architecture.sh
./crates/services/attachment/scripts/check-boundaries.sh
./crates/services/information/scripts/check-boundaries.sh
./crates/services/speech/scripts/check-boundaries.sh
pnpm install --lockfile-only --frozen-lockfile --offline
