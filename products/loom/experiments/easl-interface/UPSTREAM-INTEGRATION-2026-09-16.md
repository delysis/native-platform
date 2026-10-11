# Upstream integration checkpoint

The local EASL branch now combines upstream main
`90349a54061790954cf8a88160e4e29a8a325d4d` (PR 50) with Mine draft PR 47 at
`a43231626f5deb4ad8f6f08beb36dca40a236270`. Mine remains unmerged upstream.
The EASL import and corrected-interface work was first preserved in local commit
`dc7c654a816bf99f1885142f6a8ef3a24c405e91`. A binary-capable tracked patch and
archive of all 1,143 then-untracked source files also remain under
`target/easl-integration/before-upstream-90349a5/`.

Seven source conflicts were resolved by preserving both authorities:

- Google authorization reads credentials through native Mine settings. The new
  operation ID binds authorization and cancellation; browser IPC carries no
  client secret. Both settings and pending-import WebKit tests remain present.
- Context instructions and selected materials remain separate. Material records
  retain manifest revisions and exact edited excerpts through frozen co-writer
  application. Profile source identities and ordered selections are validated.
- Import conversion/staging stays outside editor locks. Staged manifests use the
  existing vault codec, bound to their eventual published path, before acquiring
  publication authority. Cached-source availability checks account for encrypted
  object lengths; payload verification remains the authenticated read boundary.
- Mine's frozen generation settings and shared Rust context renderer are
  retained. Current-main context-only co-writer libraries remain readable without
  invented historical settings. Explicit saves use the v3 profile envelope;
  incompatible mixed-text records are rejected without rewriting their bytes.
- Current materials/import UI and provider/cache repairs are included. The
  native view's titlebar fragment and PaneDivider reference remain byte-identical
  to upstream main. The complete combined App source SHA-256 is
  `3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725`.

The first merged context test exposed plaintext staged manifests in an encrypted
project. The codec integration above fixes that failure. An additional encrypted
test applies edited Unicode/CRLF material, removes its library entry, changes
configuration, and verifies the retained source version, frozen settings, exact
original bytes and separate instructions. Existing cancellation tests verify
worker ownership and rejection of publication after cancellation.

Local validation on Rust 1.92.0:

- 276 Tauri library tests passed; three external-runtime tests remained ignored.
- 135 native EASL view, Markdown and session tests passed; two manual tests
  remained ignored. These used the optimized `native-view` profile.
- 467 web unit tests and five focused headless WebKit interactions passed;
  Svelte check reported zero errors or warnings.
- 126 CI metadata, workflow and preservation tests passed. The ignored-test
  catalog includes the EASL target and its reviewed source-binding build script.
- Strict Clippy passed for Tauri and the native view/Markdown/session crates.
  Current-document validation and ignored-test inventory passed.

Logs and machine-readable source identity are in `target/easl-integration/`.
This is a source integration checkpoint. No current combined-source packaged
application, real model, OS IME, encrypted relaunch, remote CI, or cross-platform
runtime acceptance is claimed. The still-running pane review bundles predate
this integration. The earlier unexplained stray-key observation remains open,
as do the remaining requirements in `PARITY.md`. Nothing was pushed or merged
into main, and no user manuscript was changed.
