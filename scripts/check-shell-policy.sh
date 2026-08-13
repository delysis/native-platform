#!/bin/sh
set -eu

rustup run 1.92.0 rustc --version --verbose
rustup run 1.92.0 cargo --version
pnpm --version
rustup run 1.92.0 cargo fmt --all -- --check
rustup run 1.92.0 cargo test --locked --workspace --all-targets
rustup run 1.92.0 cargo test --locked -p llama-native-engine --features unstable-w1-contract-tests,unstable-w1-vertical-tests --all-targets
rustup run 1.92.0 cargo fmt --manifest-path crates/services/attachment/Cargo.toml --all -- --check
rustup run 1.92.0 cargo test --locked --manifest-path crates/services/attachment/Cargo.toml --workspace --all-targets
rustup run 1.92.0 cargo clippy --locked --manifest-path crates/services/attachment/Cargo.toml --workspace --all-targets --all-features -- -D warnings
(
    cd crates/services/information
    ./scripts/check-portable-packages.sh
    RUSTUP_TOOLCHAIN=1.92.0 ./scripts/run-portable-cargo.sh test
    RUSTUP_TOOLCHAIN=1.92.0 ./scripts/run-portable-cargo.sh clippy
)
rustup run 1.92.0 cargo test --locked --manifest-path crates/services/information/Cargo.toml -p information-native-acquire --features unstable-w1-contracts
rustup run 1.92.0 cargo test --locked --manifest-path crates/services/information/Cargo.toml -p information-native-host --features unstable-w1-vertical-tests --test w1_vertical
rustup run 1.92.0 cargo test --locked --manifest-path crates/services/information/Cargo.toml -p tauri-plugin-information-native --all-targets
rustup run 1.92.0 cargo clippy --locked --manifest-path crates/services/information/Cargo.toml -p tauri-plugin-information-native --all-targets --all-features -- -D warnings
rustup run 1.92.0 cargo fmt --manifest-path crates/services/speech/Cargo.toml --all -- --check
rustup run 1.92.0 cargo test --locked --manifest-path crates/services/speech/Cargo.toml --workspace --all-targets
rustup run 1.92.0 cargo test --locked --manifest-path crates/services/speech/Cargo.toml -p speech-native-host --all-targets --features unstable-w1-contract-tests,unstable-w1-vertical-tests
rustup run 1.92.0 cargo clippy --locked --manifest-path crates/services/speech/Cargo.toml --workspace --all-targets -- -D warnings -A clippy::collapsible-if
rustup run 1.92.0 cargo clippy --locked --manifest-path crates/services/speech/Cargo.toml -p speech-native-host --all-targets --features unstable-w1-contract-tests,unstable-w1-vertical-tests -- -D warnings
rustup run 1.92.0 cargo clippy --locked --workspace --all-targets -- -D warnings
rustup run 1.92.0 cargo run --locked -p xtask -- policy
./crates/native/scripts/check-architecture.sh
./crates/native/scripts/check-workflow-policy.sh .
./crates/native/tests/workflow_policy.sh
./crates/services/attachment/scripts/check-workflow-policy.sh crates/services/attachment
./crates/services/attachment/tests/workflow_policy.sh
./crates/services/information/scripts/check-workflow-policy.sh crates/services/information
./crates/services/information/tests/workflow_policy.sh
./crates/services/speech/scripts/check-workflow-policy.sh crates/services/speech/.github/workflows
./crates/services/attachment/scripts/check-boundaries.sh
./crates/services/information/scripts/check-boundaries.sh
./crates/services/information/scripts/check-w1-contract-pins.sh
./crates/services/information/tests/w1_contract_pin_policy.sh
./crates/services/speech/scripts/check-boundaries.sh
./crates/services/speech/scripts/check-w1-contract-pin.sh
./scripts/check-native-import-history.sh
./scripts/check-service-import-history.sh
./scripts/check-mom-import-history.sh
./scripts/check-loom-import-history.sh
pnpm install --lockfile-only --frozen-lockfile --offline
