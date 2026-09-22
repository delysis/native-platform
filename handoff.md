# EASL integration handoff for Chat Pro Astra

## State preserved for continuation

Branch: `codex/loom-easl-main-integration`  
Base: `5b34e054cb65a34dd8f44f28fcec94d1e535fe86`  
Packet: `/Users/george/Downloads/loom-easl-next-5b34e054`  
Packet validation: `node tools/preflight.test.mjs` passed (8/8), and the packet
preflight/export plus `--apply` both verified the exact base, source blobs,
unique fragments, patch hashes, and the 18 resulting candidate files.

The two supplied patches are applied in this dedicated feature branch as a
**takeover handoff**. This is intentionally not a promotable/main-ready
revision: retain the blockers below in the branch history and resolve them
before opening or merging a promotion PR.
Two mechanical validation repairs were necessary:

1. `src/accessibility_tests.rs`: use the pinned `accesskit::Node::is_expanded()`
   accessor (which returns `Option<bool>`) instead of nonexistent `expanded()`.
   This changes no product behavior.
2. `ui/loom.easl`: rename the `pane-toggle` parameter `position` to
   `pane-offset`. The installed EASL compiler rejects a function parameter that
   shadows its top-level `position` binding. This one rename cleared the shared
   `CantShadowTopLevelBinding("position")` error that had caused 53 review tests
   to fail at compile time.

The complete review-only test receipt is **not green**: 102 passed, 2 ignored,
and one failed after 107.58 seconds:

```text
document::tests::discard_waits_for_an_accepted_journal_and_leaves_only_the_saved_source
products/loom/experiments/easl-interface/src/document.rs:751
Storage response timeout
```

The packet does not modify `src/document.rs`; all newly reached current-chrome
and accessibility tests preceding this result passed. Reproduce the named test
once in isolation before deciding whether it is a test/runtime timing regression
or an environment flake. Do not mask it with a longer arbitrary timeout: inspect
the accepted-journal lifecycle and preserve its no-data-loss assertion.

Focused reproduction has passed once: `1 passed; 104 filtered out; 14.79s`.
That makes the full-suite failure timing/contention-sensitive rather than a
deterministic failure, but it remains a delivery gate until a clean complete
receipt is captured.

## Blocking work: native reference guard, do not bypass

The normal native build is presently blocked before application compilation:

```text
cargo check -p loom-easl-interface
error: Loom interface reference changed. Review the current view and its native
port together before updating the reference hash.
actual App.svelte SHA-256:   6667474039c166a5a33b1f4e2cd23e5cca79353962c73b1df4ac0e5c3552709a
build.rs expected SHA-256:   3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725
```

The packet itself says the App/CSS guard hashes are intentionally unchanged and
is only an unqualified candidate. Do **not** replace the expected hash merely to
make CI green. First reconcile the current `products/loom/apps/loom/src/App.svelte`
with the EASL port and review whether the native experiment still represents the
current Loom interface. If and only if that review establishes an intentional
reference change, update the guard together with the port and add source-grounded
tests/receipt. Otherwise restore/rebase the appropriate reference source and
keep the guard strict.

## Required continuation gate and delivery

After resolving the guard on an intentional basis:

1. Focused reproduction of `document::tests::discard_waits_for_an_accepted_journal_and_leaves_only_the_saved_source`, followed by `cargo test -p loom-easl-interface --no-default-features --features review-only`
2. `cargo check -p loom-easl-interface` (native-default guard/build)
3. `cargo fmt --all -- --check`
4. `cargo clippy -p loom-easl-interface --no-default-features --features review-only --tests -- -D warnings`
5. Run the requested macOS native acceptance separately: launch the actual
   binary, exercise Add/pane controls/focus/IME, and verify the app/bundle/SHA
   identity. The packet explicitly supplies no native product qualification.
6. Only after the above passes, commit the candidate plus the two narrow fixes,
   push `codex/loom-easl-main-integration`, and dispatch/reuse its GitHub CI.

Known non-blocking warning seen during review compilation:
`crates/services/easl/vendor/harfrust/src/hb/face.rs:439` has unused `shape`.
It predates this candidate and was not altered.

## Review-only lint gate: seven required fixes

`cargo fmt --all` has been run and its follow-up `cargo fmt --all -- --check`
passes. `cargo clippy -p loom-easl-interface --no-default-features --features
review-only --tests -- -D warnings` still fails with these actionable findings:

1. `src/accessibility.rs:43` — `update_accessibility` is 108 lines; split into
   bounded helpers rather than suppressing `too_many_lines`.
2. `src/document.rs:822` — replace `Duration::from_secs(60)` with the clearer
   `Duration::from_mins(1)` (or an equivalent named timeout policy).
3. `src/interface.rs:352` — invert the `if kind != "control"` condition to the
   Clippy-preferred `if kind == "control" { None } else { ... }` form.
4. `src/theme.rs:70` — avoid direct equality of the `[f32; 9]` fixture; compare
   bit patterns or use an exact-domain assertion that documents why zero/one are
   safe values.
5. `src/lib.rs:376` — `perform` is 109 lines; extract coherent action families
   while retaining the current explicit authority boundaries.
6. `src/lib.rs:399` — add the suggested semicolon after `pointer_edit(kind, arg)?`.
7. `src/lib.rs:827` — `window_event` is 103 lines; extract event-specific helper
   paths, preserving native-window ownership and ordering.

Do not add blanket `allow(clippy::...)` attributes: this package opts into
pedantic Clippy and the requested delivery gate uses `-D warnings`.
