# R1 visible secure-store bootstrap

Candidate: `c038c4f` (`Show Mom before secure-store initialization`)

The prior exact bundle blocked in `SecItemCopyMatching` before Tauri created a
window. This candidate constructs the visible Tauri shell and shared Gateway
first, then invokes encrypted settings resolution from the renderer through a
blocking-pool command. A failed or cancelled secure-store read is retryable from
the visible shell; retry evicts only a cached `Unavailable` result and never a
successfully cached installation key. Service, account, data directory, and
encrypted-store identity are unchanged.

The startup state machine separates the Keychain-only `Unlocking` phase from
native `Building`. Quit during unlock prevents native construction and exits
without claiming a native join. Quit after native construction begins waits for
the build to finish, rejects installation, quiesces, and joins any constructed
native runtime before permitting process exit. The ordinary ready-runtime quit
path retains its existing gateway drain, application-work drain, and final
native-host join.

The same candidate also adds the approved `#[cfg(unix)]` guard to the private
`collect_cached_models` test import that Windows Clippy had rejected.

## Verification

- `cargo fmt --all --check`: passed.
- `cargo test -p mom-llama-app`: 38 passed, 0 failed.
- `cargo clippy -p mom-llama-app --all-targets -- -D warnings`: passed.
- `cargo test -p mom-llama-runtime`: passed.
- `cargo clippy -p mom-llama-runtime --all-targets -- -D warnings`: passed.
- JavaScript syntax, persona/product UX, architecture, contracts, workflow
  policy, and diff checks: passed.
- Current-head remote macOS app, macOS product, Ubuntu product, and workflow
  policy jobs: passed; both Windows jobs were still running when recorded.

Exact bundle:

- path: `target/release/bundle/macos/Mom Llama R1 c038c4f.app`
- identifier: `com.delysis.llama-native-kit.mom-llama`
- executable bytes: `35,229,376`
- executable SHA-256:
  `07173e0463238de11d9cb310a971c8e209921ef1df1b14d4cb8e535aee314679`
- signature: ad hoc; no TeamIdentifier

Launched read-only acceptance proved the exact bundle displays a responsive
window with `Unlocking Mom Llama's encrypted local data…` while its background
worker waits in the same `SecItemCopyMatching` / SecurityServer decrypt path.
Cmd+Q closed this pre-native startup state with no new Mom diagnostic report.

## Open acceptance boundary

No Keychain prompt was visible through the app or accessibility surface, and no
credential/security approval was granted. The secure-store operation was not
bypassed or modified. Real-Qwen generation, same-store draft recovery, and the
ready-runtime Cmd+Q/Dock/direct-AppleEvent join paths therefore remain open.
