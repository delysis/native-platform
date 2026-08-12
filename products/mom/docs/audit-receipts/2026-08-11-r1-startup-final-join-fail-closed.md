# R1 startup final-join fail-closed receipt

Recorded: 2026-08-11

## Decision

Mom Llama may return from the final Tauri exit boundary only after native-host
join evidence is present. Startup failures before native ownership remain
retryable. Any failure after native ownership was constructed is terminal for
the process: successful cleanup requires restart, while failed cleanup retains
the sole runtime or owner and aborts final exit before Rust/static Metal
teardown.

Repeated quit while native construction is already quiescing creates no second
waiter.

## Verification

- `cargo test -p mom-llama-app -- --test-threads=1`: 41 passed
- `cargo clippy -p mom-llama-app --all-targets -- -D warnings`: passed
- `cargo fmt --all --check`: passed
- architecture, contract, persona-product UX, and workflow-policy checks: passed
- JavaScript syntax and `git diff --check`: passed
- independent final review found no unsafe owner drop, retry loop, shutdown
  liveness regression, or remaining correctness blocker

## Exact current-code bundle

- implementation revision:
  `44a6f5bde09be51a315b4ba665195ba06e0ba1d9`
- bundle: `target/release/bundle/macos/Mom Llama R1 44a6f5b.app`
- identifier: `com.delysis.llama-native-kit.mom-llama`
- executable bytes: `35,163,056`
- executable SHA-256:
  `c1d338ae69412b5f72b69e1bae6325b83af5a3ff6da73368bbcb31d6996c2fa4`
- signature: ad hoc; no TeamIdentifier
- attached launched PID: `18639`
- visible state: responsive `Unlocking Mom Llama's encrypted local data...`
- credential interaction: none; no Keychain action was taken
- quit: focused native Cmd+Q, exit code `0`, no signal

## Evidence boundary

This is non-credential lifecycle evidence. It does not claim Keychain unlock,
real-Qwen generation on this revision, same-store draft recovery, or a signed
release bundle. The launched slice proves only visible secure-store waiting and
normal pre-native exit on the exact current code. The remaining launched
acceptance gates stay open.
