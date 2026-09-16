# Web delivery integration and native verification

Functional source: `20b8312bc005641d216cb493441ded1ef6f1e51d`.
Tree: `9cead059520452a8d40be875148e56f77c17b57f`.
This integrates feature `cd79b47a0fd8f10adbcf9d641544544bbd00930f`
with main `c0bcc42d8e274c9f428d3e19508709d7fa6e23ae` through normal
two-parent merges. The supplied web packet was based on the earlier main
`68ae9d40f4010e86afe1eecb9dc156433f3edc92`.

## Delivery identity and review

The owner supplied `loom-integration-delivery`. All 105 entries in its
`SHA256SUMS` verified, all 78 changed-file base blobs matched the real feature
commit, and every changed-file ZIP payload matched its declared SHA-256.
The checksum manifest itself hashes to
`ea5f0dfe19d253b5d9b5cc00952a07ac9041f7bf61046a2463a8cdf96c664451`;
the focused lifecycle patch hashes to
`fcad3923597aeeb18fd9e4abd788b8ff0e05c4ba5fea8df68f2dc6ff7ea4b5ea`.

The reconstructed feature tree omitted one tracked file that matches an ignore
rule: `products/loom/fixtures/compat/state/loom-prior-v10/.loom/loom.sqlite3`.
Removing only that fixture in a disposable Git index exactly reproduced the
packet's feature tree. The fixture was retained in the integration. With that
single correction, applying the packet against its supplied main reproduced its
reported final tree. ZIP entries did not actually carry tracked modes; existing
modes came from Git and the patch's new-file headers established mode 100644 for
new paths. The packet's helper scripts were not executed.

## Behavior retained across the newer upstream merge

- A published import result is followed by an owned OS-thread join on the
  blocking pool. Success, conversion failure and panic all await thread exit.
  Abandoning the caller leaves ownership available to shutdown; two close calls
  cannot mistake a taken join handle for completed teardown.
- Main's completion cleanup remains: a normally awaited worker is removed from
  the registry only after that shared join finishes. Main's TLS-destructor
  regression and the web packet's completion/shutdown regressions both remain.
- Folder traversal keeps main's cancellation and shared scan budget. Peer
  lookups still freeze exact evidence before dispatch and retain Check/Resume
  behavior. Stop uses the admitted generation route without waiting for a held
  workspace lock; the combined regression checks both local cancellation and
  durable remote cancellation intent, rejecting stale project/session authority.
- Upstream folder copying, mapped PDF pages and CI coverage/cache selection are
  preserved alongside cabals, Signal and peer compute. Opening an original still
  uses its registered material identity. Native Tauri builds regenerate the
  command/permission schemas; the ACL regression verifies command equality and
  separation of default, local-generation and peer-compute authority.

## Observed checks

The tests-only patch failed against the original feature at the intended
assertion: `Success: result delivery is not thread exit`. This is an executed
red regression, unlike the web packet's unavailable-Cargo receipt. The focused
repair then passed all nine import lifecycle tests. The final integrated gate
passed **733 Rust tests** across the complete `product-loom` package group;
**11 explicit opt-in tests were ignored**. The native plugin accounts for 357
of those passes and is not counted again.

Frontend checks passed **515 unit tests**, Svelte checking with zero diagnostics,
and the production build. WebKit passed 178 of 179 cases on the first compiling
integration; the remaining case expected the former "Open original" label.
After matching upstream's "Reveal original" label, all 14 source-view cases
passed. Thus all 179 cases have passing observations, across the full run and
the focused rerun. A duplicate test-harness callback declaration introduced by
automatic merging was also removed before the compiling browser run.

The eight CI-script suites passed **144 tests**, including native-generated ACL
consistency. Current-document and ignored-test metadata validation passed.
The repository policy passed, including OMP2 source verification, and all 26
xtask tests passed. Final lint results and command-log hashes are recorded in
the [validation manifest](2026-09-16-web-delivery-validation.json).

The first full native attempt failed to link because `CARGO_NET_OFFLINE=true`
disables ort-sys binary discovery before checking its cache. Running the normal
locked build reused the existing ONNX Runtime cache and passed. Strict Clippy
also identified similar channel names and missing documentation backticks in
the new tests; both were corrected. The backtick correction is the only Rust
source difference after the functional revision above and changes no behavior.
No production deadline, assertion, test concurrency or dependency pin was relaxed.

The macOS build used Rust/Cargo 1.95.0, Node 26.0.0, pnpm 11.19.0, the frozen
JavaScript lockfile, two Cargo build jobs and the existing feature build cache.
Loom's native build reused the existing Signal worker executable, SHA-256
`1f94b6018da68fd5a69cd682abe0ba6b7b2a78476f1fe7e7705c63fe993367d7`.
The worker, protocol and build-script sources were verified unchanged from the
feature tip. This is not a fresh Signal worker build or packaged-app acceptance.

No phone was linked, real chat sent, group edited or physical Internet peer
exercised. The remaining [networking acceptance](../networking.md#remaining-acceptance-and-implementation)
and current packaged/native acceptance remain open; PR #49 remains draft.
Full local command logs are retained with the validation manifest in
`/Users/george/Downloads/loom-integration-validation-2026-09-16/`.
