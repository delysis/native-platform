# Mine integration receipt — 2026-09-14

Implementation source: `1a0e5bdd7eadea6c99142ac4e77e8c1139537135` on `codex/mom-loom-convergence`, compared with integrated base `1cee70d53605ba3bfba6a479e15eda308066ad21`. This worktree is `/Users/george/.codex/worktrees/native-platform-mom-loom-convergence`. Subsequent receipt/documentation commits do not change tested application source. Nothing in this pass was pushed, merged, installed, or tested against an existing user project.

## Delivered behavior

- Starting the recommended model download offers a skippable setup flow: writing alone or writing with chat, then automatic suggestions or assistance on request. Writing remains available. Answers create the same editable `.mine.toml` used by expert configuration. Skip leaves transfers running and creates no settings.
- The settings file is an ordinary source document. Command/Control-comma opens it; invalid configuration offers repair and preserves the last valid layout. Existing bytes are never replaced by setup. Composition, pending draft writes and asynchronous settings reads have explicit ordering guards.
- Named profiles and exact persona files feed production writing and terminal/chat generation. Model settings reach explicit native loading/reloading and preserve the exact previous resident when admission fails. Co-writer source snapshots, applied context and generation profiles commit together and survive later preset edits.
- Shared document snapshots, context rendering, sampling policy, model discovery, run lifecycle and speech adapters have real product consumers. The [integration review](integration-review.md) compares both implementations by class and identifies remaining work.

## Consolidated checks

The final Rust test command selected all affected integration consumers:

```sh
cargo test --locked --no-fail-fast \
  -p desktop-context -p desktop-generation-policy -p desktop-model-discovery \
  -p desktop-speech -p workspace-document -p operation-lifecycle -p loom-config \
  -p mom-llama-runtime -p mom-llama-cli -p mom-llama-app \
  -p loom-document -p loom-store -p loom-host -p loom-backend-llama \
  -p tauri-plugin-loom -p loom-cli -p loom-app
```

Result: **843 passed, 0 failed, 22 intentionally ignored**, including integration tests and doctest targets. Output is retained in [the final Rust log](evidence/integration-final-tests.txt); captured logs normalize trailing whitespace. Ignored tests require real models, local corpora, large files or other explicit runtime prerequisites; they do not count as acceptance.

Strict Clippy **passed** using the same package list with `--all-targets -- -D warnings`. Workspace policy also passed. Additional check output is retained in [the final check log](evidence/integration-final-checks.txt).

| Additional check | Result and scope |
| --- | --- |
| Frontend unit suite | 455 passed; one App compilation test exceeded its five-second timeout during concurrent builds. The isolated file rerun passed all 29 tests, including the timed-out case. Thus all 456 test identities passed across the suite and focused retry; the first full run was not green. |
| WebKit component browser tests | 13 passed across `SetupFlow.browser.test.ts` and `WorkspacePane.browser.test.ts`; covers navigation, choice retention, Skip, repair, available writing area and exact LF/CRLF settings editing. |
| Svelte check on final source | 0 errors, 0 warnings. |
| Production frontend build on final source | Passed; the existing large-chunk advisory remains. |
| Rust formatting | Passed. |
| Workspace policy (`cargo run --locked -p xtask -- policy`) | Passed. |
| CI metadata selection | 9 passed. |
| Current documentation validator | Passed: 2 service surfaces, 7 documents, 5 current paths, 2 retired edges. |
| Ignored-test registry | Passed: 44 registered/source ignored tests, 16 Cargo targets, 7 reviewed build scripts. |

The earlier combined Rust run exposed a Mama view fixture assigning messages to the wrong conversation. The fixture now supplies the correct owner; strict production graph validation remains intact. Final tests include that correction. Integration also corrected an oversized model-plan enum with one boxed settings snapshot; it did not suppress the lint.

The ignored-test validator initially exposed pre-existing registry drift: two real-native tests lacked entries and the reviewed hash for the unchanged plugin build script was stale. Their source and prerequisites were reviewed, the exact entries/hash updated, and the new shared model-discovery test target registered. No production gate was relaxed.

## Measured source change

These are `git diff --numstat --no-renames` counts against the integrated base, not a speculative estimate of future deletion. Rust and frontend numbers include tests.

| Source category | Files | Added | Removed | Net |
| --- | ---: | ---: | ---: | ---: |
| Rust | 64 | 8,199 | 2,451 | +5,748 |
| Frontend | 8 | 356 | 36 | +320 |
| Combined source | 72 | 8,555 | 2,487 | +6,068 |

This foundation currently grows the codebase. Validation, durable snapshots and live adapters account for new code; relocating code into a shared crate is not net deletion. Retiring Mama's separate UI, mutation/dispatch and packaging paths requires the remaining product consumers and native acceptance first.

## Compatibility and acceptance boundaries

- Loom store schema **16** is required. Schema-15 stores are refused before recovery or semantic writes, without rewriting ordinary manuscript files. Co-writer library schema **v2** likewise rejects and preserves earlier v1 files. This pass has no in-place migration and used disposable projects only.
- The common durable document snapshot backs all six Loom revision-creation paths and explicit Mama export/import. Live chat still uses the existing raw-completion pane; typed role/template requests, chat mutations, unsent drafts and complete branch navigation remain to implement.
- Imported Mama snapshots retain full branch/source evidence, but the destination transcript and private filesystem blobs are **not encrypted**. Attachment identities are references, not a bundled media migration. Imported tool metadata grants no effects.
- Named task profiles are visible through `.mine.toml`; co-writer library commands remain IPC capabilities without a rendered picker. Persona groups/history modes, capability-bound tools, Information grants/citations, payload/cache protection and complete menu-to-configuration cutover remain outstanding.
- The setup card is a fixed, optional flow started with a new recommended download; it is not an arbitrary script runner or a resumable onboarding engine. No real download, native model reload, installed app interaction, actual microphone/playback or existing-user-store acceptance occurred in this pass.
- No temperature, context heuristic, power-sampling or control-vector setting was empirically established as optimal here. The earlier research packet distinguishes literature-backed candidates from measurements still required. Defaults remain typed, validated task defaults.
- Mama is deprecated as the implementation destination. Its binary has not been deleted, and Loom's Cargo, bundle and executable identities have not been renamed. Complete single-binary parity is not claimed.
