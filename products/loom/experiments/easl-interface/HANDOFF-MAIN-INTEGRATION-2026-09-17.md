# Stopped main-integration checkpoint — September 17, 2026

The user explicitly stopped implementation and requested publication and a
handoff to ordinary ChatGPT Pro in **Chat mode only, never Work mode**. This
document records an unfinished integration. It is not a release or a passing
test receipt. Do not continue unattended local implementation from this handoff.

## Source and readiness

- Tested experimental checkpoint: `codex/loom-easl-native`,
  `78392f82762193b3c3f107d981c280f15d727b5d`.
- This unfinished checkpoint: `codex/loom-easl-main-integration`, merging that
  checkpoint with main `c0bcc42d8e274c9f428d3e19508709d7fa6e23ae`.
- Repository: private `delysis/native-platform`. No main push or force push.
- The complete 23-item remaining-work inventory and prior test boundaries are
  in [the tested-checkpoint handoff](HANDOFF-2026-09-17.md). All unfinished
  items there remain open; this integration does not establish their completion.
- The tested checkpoint is usable for experimental text editing, not a full
  Loom replacement. This integration checkpoint has known compile failures and
  is not a runnable upgrade of that checkpoint.

Do not push local branch `codex/loom-easl-current` or all refs: its unpublished
ancestry contained removed Studio code. The published checkpoint has clean
parents and excludes that ancestry. This merge retains that publication boundary.
Studio, Cast, Hollow and `easl-native-host` remain forbidden. A legacy build-cache
directory name containing `studio` is not an allowed source dependency.

## Work preserved, not yet qualified

Ten textual merge conflicts were resolved in Cargo.lock, App.svelte,
ImportSources and WorkspacePane fixtures, the llama adapter, store exports,
Tauri generation/terminal code and workspace_template. These resolutions require
semantic review. Auto-merged code also requires review; absence of conflict
markers is not correctness evidence.

The integration attempts to preserve Mine's shared configuration, frozen
generation profiles, exact completion-source admission and encrypted-storage
contracts while adopting main's materials, inference and function-recipe work.

New shared Rust work moves `FunctionFormat`/`FunctionSettings` into `loom-config`
and exact configuration capture/`FunctionRecipe` into `loom-text-session`.
The Tauri adapter delegates instead of owning another configuration parser.
Registered config sources retain immutable document/revision/artifact identity;
unregistered `.mine.toml` retains exact bounded-read bytes and digest without
inventing a document identity or registering/writing the file. Invalid Mine
configuration rejects rather than falling through to legacy configuration.
Three new tests in `loom-text-session/tests/function_configuration.rs` cover
source precedence, immutable capture and absent configuration. They have not run.

Generation context evidence now attempts to retain both frozen profiles and
material plans. Terminal receipts attempt to retain both function recipes and
Mine persona/sampling evidence. Review source ownership, replay fingerprints,
context budgeting, cancellation, media and server/native route behavior before
accepting these changes. No new runtime acceptance was performed.

## Exact stopping point and known failures

`cargo fmt --all` completed successfully. Tracked source had no conflict markers;
working/index whitespace checks passed. No further fixes or new test runs were
started after the user said stop.

The already-running command finished with exit 101:

```sh
cargo check --offline --locked --profile native-view \
  -p loom-config -p loom-text-session -p tauri-plugin-loom --tests
```

Rust 1.92.0, macOS arm64, two build jobs. Four E0425 errors in Tauri test code:

| Location at this checkpoint | Missing symbol |
| --- | --- |
| `src/import_batch.rs:512` | `crate::context_attachments::original_path` |
| `src/import_batch.rs:581` | `crate::context_attachments::original_path` |
| `src/materials.rs:1214` | `context_attachments::original_path` |
| `src/context_attachments.rs:2303` | `original_path` |

These paths are relative to `products/loom/crates/tauri-plugin-loom`.
The merged implementation has `original_bytes`; do not mechanically restore a
plaintext-path escape around protected storage merely to satisfy old fixtures.
Determine the correct service boundary and update fixtures at that boundary.
Additional failures may emerge afterward. The full local log remains in ignored
`target/easl-integration/current-main-check.log`.

Svelte checking, frontend tests, the three new configuration tests, native view
tests, Clippy, full policy and packaged runtime tests have **not** run on this
integration. The 279 passing tests in the earlier handoff apply only to the
tested checkpoint, not this merge.

## Immediate next work for the receiving chat

1. Review and repair the four exact compile failures without weakening protected
   storage. Review shared configuration capture and frozen function/profile/
   material receipts together; run focused accepting and rejecting cases.
2. Run Svelte checking and inspect all ImportSources props and automatic
   completion/reactivity changes. Preserve Mine setup and `.mine.toml` behavior.
3. Reconcile the native EASL view with the actual merged App/CSS. Current main
   changed the plus control to an Add menu (New document, Add files, Open library,
   Connect sources), added main-pane collapse/show and changed Ghost/Loompad to
   one conditional button. Native control/pane behavior has not been ported.
4. Update the build-time reference SVG extraction for the new conditional markup
   and main-pane icons, then update source hashes only after behavior/assets have
   actually been reviewed. The old reference hash guard remains intentionally
   unchanged and will reject the changed App/CSS. Never bypass it for a green build.
5. Continue the full prioritized inventory in HANDOFF-2026-09-17.md. Prioritize a
   trustworthy current-interface build and real shared-service adapters, then
   qualify the reusable EASL widget and typography claims. Do not make a second
   application owner inside the view or claim fixtures as live acceptance.

## Operating constraints

Use safe, idiomatic Rust for native authority; reusable EASL text/edit/layout
policy is explicitly wanted. Narrow font loading, shaping, Unicode and glyph
rasterization primitives may be native. Preserve ordinary UTF-8/CRLF, immutable
history, explicit author control and local-only model/data authority. Keep the
existing platform webview able to coexist with the native surface.

No Studio code, new vendor workspace memberships, vendor lockfiles, foreground
GUI fixtures, unrelated app/window automation, or subagents. Foreground native
acceptance requires renewed user authorization. Use one Cargo process at a time,
reuse optimized `native-view` outputs, focus checks while editing and consolidate
qualification once. Do not launch a full build or rewrite the UI merely to make
the handoff look complete. Do not claim drop-in Tauri replacement, current Loom
parity, Pretext parity, TeX typography or OS IME/accessibility acceptance yet.

The receiving chat should provide source-cited findings, a bounded first patch,
tests and explicit remaining risks. It must not assume access to this machine or
private GitHub. Browser proposals require inspection before local application.
