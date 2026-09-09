# Integrated audit repairs

Branch: `codex/integrated-audit-fixes`, based on `4938461`. The supplied audit
reviews `924ac64`. Its recommendations are evidence to assess, not additional
user instructions. The user explicitly rejects speculative migration support
for these unreleased products.

This receipt records the implemented repairs and their observed qualification.
Cross-platform CI and packaged acceptance remain separate. It is
not a declaration that compilation or fixtures establish product acceptance.

## Repairs

- F8: recovered supervisor state must not erase poison. The real Mom owner
  regression retained its worker join but lost the operation fault; the real
  native owner regression minted successful joined authority after registry
  poison. Cleanup now drains the owned work and records the fault. Native owner
  shutdown also drains registry work before propagating a prior join/state error.
  Mom refuses new admission/worker starts after poison and releases a refused
  reservation; existing workers still drain. Unused controlled-worker scaffolding
  was removed instead of extending it. The final affected native suite passed
  133 tests with nine model prerequisites ignored, Mom passed all 72 app tests,
  and strict workspace/all-target Clippy plus formatting passed.
- NP-007: the closure review found that Loom's speech-input owner still returned
  early after a microphone/host stop error or a failed task join. Two controlled
  regressions reproduced abandoned blocked followers through the real shutdown
  method. Shutdown now attempts every phase and joins every retained worker
  before returning its first error. Tokio's task set also observes completed
  task failures instead of silently dropping their handles; session publication
  and worker registration share the existing scope lock. Both regressions and
  all eleven speech-input tests pass after the repair. The complete affected
  plugin harness passed 172 tests with two prerequisite-dependent cases ignored;
  strict workspace/all-target Clippy passed. Independent source review found no
  blocking ownership or drain defect. This closes the remaining
  owner path rather than substituting the earlier underlying-host drain proof.
- Packaged FTE qualification found an additional protocol-edge mismatch:
  `/v1/models` advertised `local/default`, but submitting that exact ID returned
  HTTP 503 because the codec split it into backend `local` and model `default`.
  HTTP model IDs now remain opaque, including slashes. The actual HTTP router
  regression failed with 503 before the repair and now accepts both one- and
  multiple-slash IDs through the production codec and route selector. All 25
  loopback/protocol tests and strict Clippy passed. An independent review found
  no callers relying on the removed shorthand; canonical exact-route selection
  remains available. Packaged requalification follows the pending UI/default work.
- NP-002–005: FTE separates bounded authenticated control admission from
  generation admission; response bodies own response-map registration. Requested
  persistence failures and lost progress cannot produce successful completion.
  Legacy Chat preserves tool calls; completion preserves every final choice and
  emits a final suffix only when observed text is its prefix. Unrepresentable
  output produces an error. Cancellation does not become a success terminator.
- NP-012/F22: FTE and Loom inspect database identity before WAL/schema mutation.
  Fresh/current formats are supported; incompatible formats are refused. Loom
  has one current schema, with writing constraints, indexes, and triggers intact;
  the fourteen-step migration chain and retired research tables are removed.
  Opening a damaged project cannot create a replacement database. Existing
  incompatible data is preserved; no speculative migration or backup framework
  was introduced. The remaining FTE desktop v1-to-v2 upgrade is also removed;
  a real prior-format file is rejected byte-for-byte without WAL/SHM creation.
- F21/F14: Mom no longer automatically imports plaintext legacy JSON. Known
  residual artifacts are refused before key/database creation. Explicit import
  remains supported. A data-directory override cannot enable a deterministic
  release key. Wrong keys do not rotate themselves; receipt insertion is unique.
- F5: Mom callers receive an explicit runtime-owned operation scope. The global
  host lookup and duplicate ambient-call wrappers are removed. A production
  owner constructs its host, binds configuration, cancels its scope on drop,
  and joins its workers. Native-host access does not recursively lock the
  operation registry during Persona admission. Independently owned runtimes cannot discover each
  other's host through ambient state.
- Native empty-output and cancellation receipts preserve the actual invocation
  fact from the owning stream lifecycle. Cancellation before admission remains
  distinct from cancellation after the engine ran.
- Loom avoids redundant editor property updates. Ordinary navigation removes
  the completion decoration and its parent authority; Option word movement uses
  WebKit's native word boundary and commits that selection to ProseMirror before
  another refresh. This fixes a caret trapped next to a recently removed widget.
  Real modifier-key rollback and refocus scenarios verify caret/document state.
- F7: the production native worker supervisor invalidates Ready/active state on
  unwind, including poisoned status locks, and preserves the failed join.
- NP-008/F20: Information publication/removal errors retain exact committed
  identity after rename. Same-identity retries sync the actual parent directories.
  Unsupported synchronization cannot return success.
- F15: Information, FTE, and Loom refuse unsupported private-storage capabilities
  before mutation. Actual storage tests are Unix-scoped; portable logic remains
  enabled and explicit non-Unix no-mutation regressions cover refusal.
- NP-009: persistent cache metadata and encrypted entries are separate. Restoring
  an entry reads that payload once. Owner generations are revalidated at live
  promotion; corrupt metadata clears its family atomically. Updating one entry
  preserves unrelated ciphertext. Mom conversation checkpoint lookup now supplies
  the exact conversation owner, matching persistence; the old unowned lookup
  could never reuse its own checkpoint.
- NP-011: ZIM title ordering uses the path for an empty title, with independent
  valid and reversed title-table fixtures.
- F13: MCP configuration records executable SHA-256; every spawn checks it.
  A replaced/missing executable requires reconfiguration. Managed Persona tools
  retain native/no-argument restrictions. Scripts/interpreter inputs are not
  represented as immutable or sandboxed. A replaced-script regression checks
  that no marker process effect occurs.
- F18: the pinned llama.cpp wrapper takes a destination slice and passes its real
  length. Native export no longer needs an unsafe call. State import remains the
  documented narrow unsafe boundary.
- F17: Vitest/browser packages were updated to 4.1.11, chacha20 to 0.10.2, and the
  GTK-compatible GLib 0.18.5 has a documented two-line upstream security backport.
  Seven maintenance-only notices have scoped reasons and a 2026-12-09 review
  date. No runtime vulnerability is waived. ort-sys already verifies the hash of
  its downloaded archive; no redundant downloader was added.

## Simpler development process

Full CI executes one workspace matrix, doctests, and Linux Clippy. macOS adds
browser interaction and packaging; frontend commands do not repeat Rust builds.
Workspace execution reports failures across all binaries in one run instead of
stopping after the first failing package and requiring another discovery cycle.
The fuzz lane executes bounded inputs and retains crashes. A hash-pinned CPU
model lane runs exact registered tests and rejects zero execution. Advisory
checks scan the actual root and fuzz locks.

PR selection uses Cargo's resolved and declared local dependency edges plus
explicit non-Cargo rules. Unknown paths or unavailable metadata select full
coverage. The old path planner, Mom overlay, and shadow comparison are removed.
The obsolete W8/W9 `xtask lean` census is retired. Git retains its history.

The eighteen existing Swift programs are extracted unchanged from the macOS
smoke shell into `scripts/macos-smoke-support`. A safe Rust xtask command compiles
and links them; the shell builds them once and reuses them for both launch/use/
quit cycles. Full macOS CI and the selected release-tooling lane execute that
compiler gate. Local compilation passed all eighteen programs; the compiled
bundle-inventory helper also executed successfully. This closes the former
shell-syntax-only check without rewriting the platform implementation.

Mom's Consult-to-Persona migration and repair machinery is removed. Current
builtin catalog updates still preserve user edits. The unused file-size-only
native memory estimator and tests of its obsolete formula are removed;
admission uses the configuration-sensitive estimator. Memory budget mode is
explicit; the heuristic migration from historical default values is removed.

Current contributor, product, security, and architecture documentation now
states these boundaries. Dated ADRs and receipts remain historical. The W9
correction names the actual orphan `manifest_tests.rs`, not the incorrectly
attributed `research_admission.rs`; no historical evidence was silently resealed.

Lifecycle finding qualifications remain explicit. F3's old universal-suite
claim is superseded by `CURRENT-DECISIONS`; tests exercise actual owners with
different scopes, not a universal acceptance matrix. F9's cancellation
arbitration and F12's close-abort/resume are intentional behavior. For F10,
executor drop requests cancellation while the actual Speech task supervisor
retains monitor/backend-shutdown panic errors separately. F11's existing panic
fixture covers its helper, not the full admission path. Source confirms the
higher-level application/session poison mitigation; no additional observable
admission leak was demonstrated. No stronger runtime proof is claimed for F11,
and no generic conformance framework was introduced to satisfy a proposed test
matrix.

## Verification recorded so far

Before the HTTP repairs, real adapter regressions observed control HTTP 429,
a leaked response identity after body drop, and `response.completed` following
storage failure. The independent valid ZIM archive also failed before repair.

With the complete Rust 1.92 toolchain on PATH:

- Consolidated workspace execution at `1135259` passed 1,591 tests in 63
  harnesses, with 43 fixture-dependent tests ignored. This includes inherited
  lifecycle, redirect, Speech, FTE, Information, and Mom/Loom store regressions.
- Workspace doctests passed 20 cases across 44 harnesses, none ignored.
- After the later cancellation/cache-owner fixes, the actual Mom library passed
  172 tests, and FTE desktop/loopback activity passed 10 tests. The updated real
  product cache scenario passed on Metal with the hash-verified Qwen fixture:
  encrypted checkpoint creation, cold/warm reuse, explicit clearing, and cache
  off were executed. This is product-runtime evidence, not packaged UI evidence.
- Strict workspace/all-target Clippy passed after the final cache-owner and
  CI runner corrections.
- The wrapper's real CPU capacity test passed for zero, one, and undersized
  buffers with canaries, then exact export/restore: 1 passed, none ignored.
- Native saved-prefix live/durable restoration/context rejection and strict
  pre-cancelled batch handling passed locally and in CI: two exact CPU tests.

Other observed checks:

- Full WebKit interaction suite: 74 passed after the native word-navigation fix;
  the formerly intermittent rollback also passed in isolation. Svelte check
  reports zero errors and warnings. The previous setProps-only repair passed
  locally but failed in CI and was insufficient.
- Loom unit suite passed 448 tests. Unit workers are capped at four; contention
  with native CPU inference had exceeded the compiler test's five-second limit.
  The limit was not increased.
- Simplified metadata/planner checks: 37 passed. Consolidated workflow/planner,
  ignored-registry, and backup checks: 120 passed. The changed workflow/registry
  checks subsequently passed 63 cases.
- Mom architecture and contracts passed (102 commands, 99 affordances, 46 effects,
  36 parity rows, 58 upstream settings, zero blockers).
- Current-document paths and ignored-test source registry validation passed.
- Root/fuzz cargo-deny checks and JavaScript advisory scan passed after repairs.
- CI `34401258044` passed frontend, policy, both bounded fuzz targets, and the
  exact CPU model scenarios. Linux workspace tests/doctests/Clippy passed, but
  its optimized GLib command could not test a non-workspace dependency. A small
  Linux workspace integration target now exercises the real GLib string iterator
  under release optimization. Windows compiled but correctly refused a private
  loopback token in a test that assumed Unix support; desktop activity remains
  portable, while the real private-token case is Unix-only. macOS compiled and
  passed Rust tests, then reproduced the now-repaired native caret failure.
  These failures are preserved, not relabeled as a green full run.
- CI `34403997871` at `34980c0` passed Linux tests/doctests/Clippy and the optimized
  GLib iterator, all three exact CPU model scenarios, frontend, policy, and fuzz.
  macOS passed Rust tests/doctests, all 74 WebKit cases, and Loom/FTE packaging.
  Windows passed the corrected FTE boundary and subsequently exposed 23 Mom
  tests whose setup opened unsupported Information private storage. Those
  composed-store tests now run only on Unix; independent supervisor, approval
  worker, serialization, and policy tests remain portable. A Windows product
  boundary test asserts refusal preserves existing source and creates no store.
- The next consolidated local run completed 63 harnesses with 1,544 passes and
  38 ignored cases. One additional Loom adapter harness was terminated after
  an assertion unwind hung in a fixture that never completed cancellation;
  this is not recorded as a passing consolidated run. That fixture now honours
  cancellation and verifies delivered bytes across variable chunk boundaries,
  retrying empty polls within a bounded deadline. Overflow, contiguous sequence,
  and failed-terminal assertions remain intact. The final focused case passed
  in 2.61 seconds; its full adapter harness then passed 48 tests with five
  fixture-dependent cases ignored in 3.26 seconds. Together with the completed
  unaffected harnesses this records 1,592 passing cases and 43 ignored cases,
  not a claim that the interrupted invocation itself passed.
- The FTE current/prior/foreign schema checks passed eight cases after removing
  the upgrade path. Workflow and current-document checks passed 45 cases.
  Strict workspace/all-target Clippy passed after all final fixture and schema
  edits, using the existing dependency cache.

Earlier Rust 1.95 results and the disk-full Rust 1.92 attempt are not substituted
for current qualification. Commands select the complete 1.92 toolchain on PATH
and reuse the active shared target. CI retains dependency caches on test failure;
failed behavior still fails the aggregate. The model lane now includes Mom's
registered product-cache integration test and rejects zero execution.

## External wrapper

Native-platform pins source commit `eb0e47b57c2fba97ed13e8fe5e949d11798232cb`.
Draft PR: https://github.com/delysis/llama-cpp-rs/pull/11.

The fork also retires upstream registry publication: it is consumed by immutable
Git revision and must not accidentally package against the unrelated registry
sys crate. Both crates are unpublished and upstream release-publication workflows
are removed in `3cea96c5d09c1d9cdc0db418ea27d3f1e200465b`. Main's source pin need
not change for workflow metadata.

CI run 34394817229 passed Linux wrapper tests, Windows/macOS builds, workflow
policy, and CUDA. The CUDA check took nearly its ninety-minute allowance because
it built a multi-architecture binary. Commit `4c000bd5992da2a95beffe3b6777216f2a5fea67`
pins Rust 1.92 and limits that compile check to representative CUDA architecture
75 with a thirty-minute cap. Run `34403567854` passed all five jobs; CUDA completed
in 26 minutes 24 seconds. This is compile coverage, not GPU runtime acceptance.

## Local cache maintenance

The user's September 9 instruction supersedes per-cache approval. The preference
is saved. A dependency-free safe Rust helper checks Cargo identity, a fourteen-day
age threshold, active builds, Cargo locks, symlinks, and open files. It only prunes
reproducible Cargo output subdirectories; model fixtures and source stay intact.
A real filesystem regression passed. The task automation currently follows audit
CI every fifteen minutes; after completion it returns to daily 04:00 cleanup,
quiet on successful cleanup or active-build deferral. It never broadens the
helper's deletion paths or falls back to manual deletion. Research databases,
including Alexandria, Community Archive, Encyclopedia, and MPC, were verified
present after cleanup; the major Alexandria and Community Archive databases
also opened successfully read-only.

The automatic sweep removed 228,332,165,314 bytes, in addition to the obsolete W1
and W9 targets. Available space was approximately 193 GiB after resumed builds.
The disk blocker and old approval request are resolved.

## Qualification checkpoints

Full CI `34406935088` completed successfully at `6d49d14`: all eight jobs passed,
including Windows, macOS, Linux, browser interactions, bounded fuzz execution,
and the selected real-model scenarios. This qualifies the FTE schema
simplification, Mom platform fixtures, and Loom cancellation fixture repair.

The subsequent unused Consult engine and hidden CLI were removed; the current
Persona catalog, mentions, and groups remain. Affected Mom checks passed 168
library, 34 runtime integration, 71 app, and 11 CLI integration cases; thirteen
prerequisite-dependent cases were skipped across those harnesses. Strict
workspace/all-target Clippy passed. The ignored registry now contains 42 entries
because the removed engine's integration case has no remaining implementation.

The existing current four-Persona scenario passed with the hash-verified Qwen
fixture on Metal in 10.88 seconds. It exercised targeted cancellation, real
synthesis, attribution, and unchanged source conversations and Persona versions.
Its dispatch worker now explicitly carries the thread-local fixture directory;
previously it lost that binding and correctly hit the incompatible default-store
rejection. Failure diagnostics now join the dispatch and expose its actual result.

The FTE real OS credential test passed in 0.39 seconds, including create, replace,
readback and deletion of a disposable Keychain entry. Two separate normal Mom
CLI processes created and reopened the same encrypted conversation through the
OS Keychain resolver with the environment-key override unset and secure storage
explicitly enabled for the debug executable. These checks establish the named
runtime paths, not release-bundle UI acceptance.

Run `34409139337` at `ceaf885` passed Linux, macOS, Windows, model integration,
frontend, and fuzz, while its policy tests exposed two stale copied ignored-test counts.
Those assertions now use source reconciliation and a small explicit platform
fixture; the complete 120-case policy test command passed locally. The subsequently found
HTTP model-ID repair and the user's new UI/default-model punch list are not
covered by that earlier revision.

Keep packaged
interaction, OS credentials, native quit/join, and reopen
evidence separate from compilation and controlled fixture execution. No merge
or distribution is implied by this receipt.

## September 9 product punch list

The first Mom pass used a compact overlay titlebar for Settings and the sidebar
toggle, retaining native decorations and lowering the minimum to 640 by 480.
The one-line composer grows with its draft; the permanent shortcut row, fixed
150-pixel composer floor, and 180-pixel transcript spacer are removed. The
space-efficiency preference is recorded in the root and Mom instructions.

A small std-only workspace crate owns the official Gemma 4 12B QAT Q4_0
artifact identity and exact Hugging Face cache lookup. Fresh Mom/FTE setup
uses that cached candidate while preserving explicit choices. Loom's catalog
shares the identity; its existing policy already prefers the official 12B
artifact. Normal model inspection and validation still apply. The local
6,975,879,296-byte artifact matched SHA-256
`93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b`.
The hidden Mom server CLI shim and obsolete cache-policy spellings are removed;
current model residency, MCP, and cache commands remain.

The consolidated Rust run recorded 1,586 passes, 42 prerequisite-dependent
ignores, and one obsolete assertion requiring the previous 760-pixel minimum.
That assertion now verifies entry into the actual compact breakpoint; the
affected app harness then passed all 71 tests. Strict workspace Clippy, the
120-case policy command, ignored-source reconciliation, Mom architecture, and
29 frontend tests passed. This records the failed invocation and its focused
repair rather than relabeling the original run as green.

The active isolated Mom session now uses Gemma and normal reasoning parsing.
Messages and system prompts were compared and preserved in all fifteen
conversations/Personas. The supported model-selection command loaded Gemma on
Metal and returned `host_integrated`. Native UI layout/resizing and the rebuilt
FTE API journey remain to execute. Full CI `34411484993` passed all eight jobs
at `54136ce`, including all three desktop platforms. That revision predates the
following second UI pass.

The user rejected the first layout pass after actual use. Settings now occupies
a compact right sidebar instead of a centered modal. Conversation content uses
the remaining width; invisible message actions reserve no row height. Duplicate
headings, oversized sidebar branding, fixed reading gutters, and extra composer
form margins are removed. Switching conversations updates the sidebar's chat
instructions without replacing unrelated settings edits. The normal AppKit
titlebar replaces Overlay/hiddenTitle; the webview no longer reserves stoplight
space or repeats the native title. This is the standard native fallback, not a
claim that the reported missing-stoplight or resize-cursor cause is established.

The affected Mom app harness passed all 71 tests. Focused WebKit geometry checks
covered 640 by 480 and desktop layouts, and the actual conversation-refresh
function preserved unrelated settings edits/focus and rejected a stale projection.
The debug bundle built in 7.98 seconds and passed local ad-hoc signature checking.
The Mac was locked before native interaction could resume; the user's existing
Mom process and conversation were preserved. Native controls, edge cursors and
resize behavior still require verification in the rebuilt running app.

The closure review also corrected two FTE documents that still described the
removed schema upgrade as current. Historical receipts retain their original
scope. Native memory reservations remain explicitly heuristic; no measured
supported-device envelope or hard process-memory limit is claimed.

Native access briefly resumed. The old Mom process was observed idle with an
empty draft, then quit normally: native and speech hosts joined, all six named
workers joined, zero operations/tasks remained, and the drain completed in
110 milliseconds. The new sidebar bundle launched against the same encrypted
store, but normal Keychain access remained pending in SecurityAgent. Computer
Use refuses that protected app; the user was asked to complete the prompt.
The screen subsequently locked again. No key override or alternate store was
used to turn this into a claimed successful reopen.

The current FTE bundle started its authenticated loopback at port 18491 through
Settings. The advertised `local/default` now routes to actual local Gemma
inference and its completed response can be retrieved from the response store.
However, its generated text exposed another true defect: the shared model-default
chat renderer used old Gemma framing for Gemma 4. The same sentence with the
official Gemma 4 framing through raw completion immediately produced a normal
13-token answer. Successful HTTP transport is therefore not recorded as model
acceptance. A separate live cancellation after the first text delta returned
`cancelling`, then `response.incomplete`, with no `response.completed` event.

The shared renderer now recognizes Gemma 4 before the pinned simple-template
API can misclassify it. It requires the supported canonical embedded template
and follows its default non-thinking text semantics, assistant history and turn
markers; ordinary tokenization adds BOS once. Unstructured tool turns without
required call metadata fail explicitly. Other model families and explicit
template choices retain their own paths. The native renderer suite passed;
the identical packaged FTE request still needs rerunning on the rebuilt bundle.

Full CI `34412937976` passed all eight jobs at `f0c3ca8`. It qualifies the second
Mom layout pass and the Loom speech drain repair, and predates the Gemma 4
renderer and poison-evidence changes described above.
