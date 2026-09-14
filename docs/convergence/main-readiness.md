# Mine integration: main readiness

This is the readiness record for [PR #47](https://github.com/delysis/native-platform/pull/47), separate from the [historical first-pass receipt](integration-receipt.md). It records preservation of current-main behavior and the limits of this additive integration. It does not certify that Mine already replaces every Mama capability or is better on every possible correctness, performance, storage, or interaction measure.

**September 14 follow-up:** the sections below retain the earlier `6f15371`
acceptance record. New macOS private-storage encryption and the remaining
custom-download/Google-client settings forms are now implemented. The current
scope, verification and source-preservation boundaries are recorded in
[encryption and power configuration](encryption-and-power-config.md). The live PR
is the authority for its current head, draft status and CI; the older readiness
statements below do not certify the later encryption revision.

## Source and scope

The reviewed main baseline is `8ce8948f3bcfc24df87ed0346801dedc7402feb0`. It is an ancestor of application source `6f153711b09d8f8c3b86a4578a762be1f24c77a3`. The branch therefore includes main's integrated-audit fixes and its macOS development gate. This is an ancestry observation at the stated revisions, not a guarantee that main has not advanced since inspection. A fresh fetch after native acceptance still reports zero commits behind that main. This receipt is a documentation-only follow-up; the live PR records the final documentation head and CI result.

The destination is Loom, becoming Mine. This PR adds a quiet, optional configuration flow during an explicitly started recommended-model download and makes `.mine.toml` the normal new settings surface. Shared Rust implementations replace duplicate context selection, sampling validation, model discovery, lifecycle bookkeeping, speech construction, and document projection. Typed durable snapshots preserve source and lineage in Loom revisions, applied co-writers, and explicit Mama export/import. Existing native, Attachment, Speech, and Information services remain the actual effect owners.

Mama remains built and usable. Its menus, tool runtime, encryption and unique persona behavior have not been removed. Loom's Cargo packages, executable and bundle identity are unchanged. This PR is a reviewed foundation and behavior-preserving integration, not the final single-binary cutover.

## What is preserved from main

| Main behavior | Integrated behavior and evidence |
| --- | --- |
| Existing schema-15 projects and immutable history | Store schema remains 15; `schema.sql` is unchanged from main. Existing revisions derive the common document view from their authoritative artifact/segment records without read-time rewriting. New revisions declare their snapshot format. A missing or malformed declared snapshot remains corruption, rather than falling back to a different interpretation. Fixtures construct main-format records directly and exercise open, edit and reopen with CRLF, Unicode, empty documents, provenance and receipts retained. |
| Existing workspace configuration | Existing `.loom.md` configuration retains its interpretation when `.mine.toml` is absent. Its authored bytes are preserved. Themes, named catalog/profile model selection, pane kinds, references and main's existing pane defaults remain supported. A newly authored `.mine.toml` explicitly selects the new settings format; setup does not overwrite an existing file. |
| Co-writer v1 libraries | Current-main context-only entries remain readable, applicable, editable and deletable. Listing/applying does not rewrite the library. Explicit mutation writes v2 while preserving untouched context-only values and timestamps. A profile freezes at explicit application or save; historical settings and parent revisions absent from v1 are not invented. Invalid configuration does not prevent independent saved-library listing/deletion. |
| Model selection and native ownership | Existing named workspace model choices remain consumers of native admission. Dotfile settings select a verified native configuration. Failed replacement retains the exact previous resident; aliases for the same configuration update presentation without replacing its owner. The loader retains native capability checks and does not silently choose another model when an explicit path is invalid. |
| Main's writing, Loompad, imports and attachment work | Main's Loompad component, completion-session implementation and connected-import implementation have no diff from the baseline. The stylesheet only loses two obsolete transfer-history rules. Their integration paths remain present. The App and attachment merge retains main's import/drop, pane-resizing, source navigation and native media behavior while adding configuration and shared context selection. Frontend unit and selected WebKit component tests include these consumers; this is not blanket native interaction acceptance. |
| macOS development and other-platform CI | Main's workflow definitions and required-job policy are unchanged by this PR. macOS and fast platform-independent checks gate development; Linux and Windows continue under main's asynchronous policy. The new discovery package is included in the ignored-test inventory assertion. No test is made optional to excuse an integration regression. |

The earlier branch's schema-16 and v1-library rejection boundaries were corrected before main readiness. They remain in the historical receipt only as an account of the earlier revision.

## Completed local checks

These results come from the local `/tmp/mine-main-*` logs. The logs span the merge and its focused repairs; they must not be summed into a claim that one final-revision run was entirely green. Rust application source is unchanged between the overflow repair at `22e3440` and final application source `6f15371`; the last application change only moves the existing transfer cards into view and names each Cancel button accessibly. The compact command receipt and native identity records are in `evidence/main-readiness-checks.json`, `evidence/main-native-identity.json`, and `evidence/main-native-model-environment.json`.

| Check | Observed result and boundary |
| --- | --- |
| Workspace Rust tests | The completed `mine-main-workspace-tests.txt` run reports **1,789 passed, 1 failed, 44 ignored** across 73 result rows. The failed target is `loom-backend-llama`; the failure was the text-stream overflow authority test. This first full run was not green. |
| Overflow repair and config regression rerun | `mine-main-overflow-repaired.txt`: backend **51 passed, 4 ignored**, config **13 passed**. The repair rejects a known stream-authority failure before candidate validation or potentially large provenance construction. The test additionally supplies unusable outputs so an output-count failure cannot mask the original overflow. This resolves the failed test locally; final broad checks remain separately recorded. |
| Workspace doctests | `mine-main-doctests.txt`: **20 passed, 0 failed**, across 52 result rows. |
| Focused integration tests | `mine-main-focused-tests.txt`: **280 passed, 4 ignored**, covering generation policy, shared discovery, config and the Loom plugin. A later exact-alias regression also passed. These overlap the workspace run and are not additional unique-test totals. |
| Store tests and Clippy | `mine-main-store-tests.txt`: **145 passed, 1 ignored**, including main-format store preservation. Store Clippy completed successfully. Eight focused snapshot tests also passed; these overlap the store suite. |
| Frontend units | `mine-main-frontend-tests.txt`: **467 passed**, 64 files. |
| WebKit components | `mine-main-browser-final.txt`: **18 passed**, across SetupFlow, WorkspacePane and Loompad. Covers the setup choices/skip and retained writing area, source settings edits, and main's Loompad component behavior. |
| Svelte | `mine-main-download-ui-check.txt`: **0 errors, 0 warnings**, including the final Downloads markup. |
| Fast CI policy tests | `mine-main-policy-node-final.txt`: **124 passed** after updating integration expectations for main's current workflow shape and shared-discovery inventory. The earlier policy run was not green. |
| Package metadata and ignored-test registry | Metadata-selection tests: **9 passed**. Registry validation: **44 registered/source tests**, 16 Cargo targets, 7 reviewed build scripts. Ignored tests remain unexecuted prerequisites, not acceptance. |
| Workspace Clippy and policy | Strict `cargo clippy --locked --workspace --all-targets -- -D warnings` passes. `cargo run --locked -p xtask -- policy` passes. Formatting and diff checks pass. The earlier remote float-comparison warnings were corrected by comparing exact bits in the fixture. |
| Final native build | Tauri debug app built from `6f15371`; local ad-hoc signing and deep/strict verification pass. This is a review bundle, not signed/notarized release qualification. |
| Remote CI | The final source has been pushed. Required macOS checks and the full cross-platform run remain live on PR #47; the PR stays draft until its development gates finish. Earlier superseded failures/cancellations are retained in the Actions history. No remote result is inferred from local success. |

## Native interactions

The first review used application source `22e3440`, bundle `app.delysis.loom.mine-review.22e3440`, PID 31884. Starting the recommended model/projector download exposed the two-question setup flow while the manuscript remained editable. Choosing chat and “Let me ask” persisted `.mine.toml`, displayed chat, and disabled automatic suggestions without changing the manuscript bytes. Inserting malformed TOML displayed the parse error while retaining the prior layout and editable source. Repairing the configuration saved a light theme and admitted the local E2B writer. The first load spent several minutes compiling embedded Metal shader source, then recovered without intervention. Native initialization and dependency pins are identical to main. The review process exited following Command-Q.

The final review uses application source `6f15371`, bundle `app.delysis.loom.mine-review.6f15371`; executable/asset hashes and process identities are recorded separately. In its disposable project:

- Ordinary WASD/IJKL text persisted across reopening.
- “Skip setup” left both transfers active and created no `.mine.toml`.
- The Downloads section exposed progress and individually named Cancel buttons above collapsed Advanced controls. Cancellation reached the terminal Cancelled state. The small projector completed download/hash verification; this is download evidence, not multimodal inference acceptance.
- Command-comma opened the actual TOML source. The light theme, visible chat pane, exact policy/model path, and requested native sizing were saved through that document. Completed function receipts retain the verified resident descriptor with 8,192 context tokens, 512 batch tokens and four parallel cases on Metal; `main-native-model-limits.json` retains only that metadata, excluding user input/output.
- The exact local Gemma E2B base fingerprint produced a real four-family inline continuation. The visible editor showed ghost text while the manuscript on disk still contained only the authored prefix. Persisted generation terminals include completed and explicitly cancelled runs.
- The user then exercised the chat pane; its completed receipts bind to explicit chat submissions and the current project/document. They are not automatic-writing results or cross-project cached output. User chat text is excluded from this receipt.

Automated typing paused when the UI tool detected user input. One-word acceptance, cached Loompad paging, minimum-size layout, a final model-size replacement, and final-instance quit after generation are not claimed as native checks here. Their existing focused regression/component coverage is separate. The final review window remains available for the user. A tool-relaunched instance without the acceptance environment was stopped and restarted with the verified disposable root; its startup is excluded from the recorded interaction evidence.

These checks establish the stated review-bundle interactions. They do not certify existing user-project interactions, every model family, audio, or a production-signed release.

## What this does not complete

- Live typed chat still needs native role/template requests, version-bound append/edit/select, attachment-aware unsent drafts, and complete branch navigation. The current chat pane continues its existing raw-completion history path.
- Mama's full/system-only/empty persona history modes, groups, handles, capability-bound tools, Information grants/citations, encrypted payloads and persistent cache protection have not all moved to Mine. Imported metadata is evidence and grants no effects; imported transcript text and private filesystem blobs are not encrypted, and attachment references are not a bundled media migration.
- Named task profiles and exact persona context have dotfile consumers. The co-writer library commands remain IPC capabilities without a rendered picker. Full menu-to-dotfile/command replacement remains unfinished, so existing advanced UI is retained.
- The setup flow is fixed and optional, starts with a new recommended download, and is not an arbitrary script runner or a resumable onboarding engine. The native checks above cover download progress/cancellation, setup/skip, settings repair, reopen and visible generation. IME composition, failed-download recovery, one-word acceptance, live sizing replacement and audio retain the narrower test boundaries stated above.
- No temperature, context-size rule, power sampler or control vector has been empirically shown to be optimal here. Profile validation is implemented; calibration and controlled quality/latency experiments remain separate work.

At `6f15371`, the rename-aware source diff from `8ce8948` adds 8,864 and removes 2,256 Rust lines, and adds 422 and removes 109 Loom frontend lines, including tests: **a net increase of 6,921 source lines**. Manifests, documentation and logs are excluded. Consolidation has removed duplicate implementations, but shared validation, durable records, adapters and regression tests currently outweigh those deletions. Snapshot storage growth, memory use and end-to-end latency have not been comparatively benchmarked; there is no universal efficiency claim.

## Merge scope and decision

The application is caught up with the stated main and the locally observed regressions have been repaired. The PR remains draft while required macOS CI completes; its final description is the live gate-status record. Any unresolved difference that breaks an existing main workflow remains a blocker for this PR. Incomplete Mama-only parity is a blocker for deleting Mama or presenting Mine as its complete replacement, rather than a reason to claim this additive PR already finished that larger task.

The final assessment should therefore be specific: which existing workflows were preserved, which defects improved, which new paths were exercised, and which boundaries remain. It should not say “better in every regard” on the strength of compilation or passing fixtures.
