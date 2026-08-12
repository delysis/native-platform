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

## Evidence boundary

This is non-credential lifecycle evidence. It does not claim Keychain unlock,
real-Qwen generation on this revision, same-store draft recovery, or a signed
release bundle. Those launched acceptance gates remain open.
