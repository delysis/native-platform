# Consolidation gates

These gates supplement, not replace, existing workspace, WebKit and packaged
native gates. Source assertions are drift detectors, not execution evidence.

```sh
node --test scripts/ci/consolidation-contracts.test.mjs
cargo fmt --all --check
cargo metadata --offline --locked --no-deps --format-version=1
cargo test --offline --locked -p workspace-document -p desktop-launch -p loom-document
cargo test --offline --locked -p fte-loopback response_cancel::tests
cargo test --offline --locked -p mom-llama-runtime document::tests
cargo test --offline --locked -p mom-llama-app
cargo test --offline --locked -p tauri-plugin-loom workspace_template::tests
cargo check --offline --locked -p loom-app -p mom-llama-app
cargo clippy --offline --locked --workspace --all-targets -- -D warnings
pnpm --filter @delysis/loom check
pnpm --filter @delysis/loom test
pnpm --filter @delysis/loom test:browser
pnpm --filter @delysis/mom-llama check:frontend
pnpm --filter @delysis/mom-llama test:frontend
```

Run native packaging and acceptance using the existing exact-candidate
workflow, including unchanged source/visible projection, caret/IME/focus,
correlated real completion, acceptance and exact reversal, persistence/reopen,
shutdown and idle/resume requirements. No compiler exit code, synthetic test,
HTML fixture or cancellation acknowledgment establishes native acceptance.

For chat-mode packaging, additionally verify that the single Loom executable
loads Mom's assets/ACL, uses the original encrypted store and Keychain identity,
preserves startup unlock/build/close transitions and completes real cached
expert consultations with unchanged attachment/citation/approval identities.
The Tauri CLI may inject configuration overlays: both embedded contexts must
be tested rather than assuming context isolation from successful linking.

Record command, candidate commit/tree, tool versions, exit status and log hash.
Missing hardware, missing offline dependencies, blocked CI billing and failed
commands remain distinct non-pass states. Never inflate generation guards,
deadlines, previews or fixture outputs to produce an acceptance receipt.
