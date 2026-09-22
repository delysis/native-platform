# Audit remediation continuation — 22 September 2026

**NOT PROMOTABLE.** Implementation was pushed to PR #72, but no current macOS artifact or approved-model writing journey was executed. No PR was merged or rebased.

Implementation: `7ed336545cfd8df4489d4db1615bf2a5033699e2`
Implementation tree: `bee8c361981180abbcaba4fc782996c76db575d8`
Parent: `e605f1c62bdb43b785db3a1a97c57079e98aa9b4`
Branch: `codex/audit-release-gates`

## Changes and boundaries

Remembered workspace labels now show exact paths, while root strings and native document-relative paths remain unchanged. Same-title roots are not discarded. Long path labels round-trip through the bounded navigation history. The actual Mac filesystem relationship still needs inspection; a genuine nested `writing/` directory must not be stripped or relocated.

A validated acceptance-directory launch now requests non-persistent WebViews. The intended boundary is isolation of renderer history from normal-profile localStorage without changing native manuscript storage or normal launches. Two Rust configuration tests were added but **not compiled or executed here**. Actual WebKit isolation remains a native acceptance check.

The real-completion toggle now identifies the current `Ghost text` / `Loompad` button and its exact state fields, and uses the existing Cmd+Shift+G policy shortcut rather than mistaking mode switching for on/off. Matching fails closed on ambiguous, incomplete, or unbounded traversal. Diagnostics reuse the same observation-only matcher. The old two-string arguments remain only because the current shell caller still supplies them; they are not searched for in the UI.

**This is not a completed smoke rewrite.** `exercise_loom_completion_controls.swift` still describes retired autocomplete/Shuttle UI; the historical full journey must be reconciled on macOS. No native engine or renderer defect is claimed fixed from selector repair alone.

## Executed evidence

- Navigation assertions: **10 passed** using Node on the same assertion bodies and unchanged transpiled production code. This is not a pinned Vitest, TypeScript typecheck, Svelte or browser run. Old-source negative control: **6 failed, 4 passed**.
- Swift portable AX-contract checks: **21 passed**. Reintroducing retired AX labels fails on `disabled Ghost text`. Linux execution of the native command refuses rather than emitting synthetic success. The macOS framework branch is **uncompiled/unexecuted**.
- Pinned Rust invocation: **BLOCKED**, `rustup: command not found`. No workspace, doctest, Clippy, native, release or model run is claimed.

The accompanying continuation ZIP contains execution-receipt.json, finding-evidence.json, unchanged raw logs, LOGS.sha256, the portable runner, verified base/edited source and a patch. These files are delivered with the chat rather than duplicated as generated repository data. The log identities are recorded below.

## Finding-to-evidence matrix

All 37 findings are represented below and in finding-evidence.json in the continuation ZIP. **Current-tree gate = BLOCKED for every entry until the settled gate exists.** This is an evidence boundary, not a claim that all 37 defects remain open. The authoritative handoff reports the f8588b3e baseline suite; those historical passes are retained, not rewritten as current results.

| ID | Finding | Retained evidence / current boundary |
|---|---|---|
| F01 | Opaque state import provenance | Audit records provenance repair; real importer acceptance remains V01. |
| F02 | Executor identity released before final publication | Executor lifetime/publication work is recorded in #68. Component evidence does not establish real active-quit joins. |
| F03 | Memory/persistent cache ordering | Cache ordering work is recorded in #69; cross-process guarantees remain V03, owned by the injected store. |
| F04 | Ambiguous cache insertion and oversized replacement | Cache outcome/replacement work is recorded in #69; current-tree final-residency controls must run. |
| F05 | Common prefix ignored for multi-branch requests | Canonical identity prerequisite #64 and common-prefix work #69; real native/cache equivalence remains unexecuted here. |
| F06 | Prefill ignores cooperative cancellation | Cancellation work is recorded in #68; no measured native long-prefill or model-load cancellation latency was obtained here. |
| F07 | Controlled UTF-8 decoder divergence | Audit records UTF-8 repair; rerun actual generation paths, not just a text-projection fixture. |
| F08 | Zero-decision cancellation loses controlled diagnostics | Zero-decision diagnostic and verified-wait contracts still require their focused current-tree controls. |
| F09 | Stops inside token pieces | Audit records in-piece stop repair; text output and strict token evidence remain separate assertions. |
| F10 | Ordinary sampler domains are not consistently validated | Sampling-domain work is recorded in #70; public admission paths must reject invalid inputs before native constructors. |
| F11 | Greedy bypasses penalty transforms | Penalty/greedy transform work is recorded in #70; native winner-changing control is not a mock parity claim. |
| F12 | Required macOS PR gate lacks explicit native behavioral checks | CI selection is recorded in #71 and release mutation enforcement in #72. Billing failures are not product failures or passes. |
| F13 | Doctest gap is PR-specific, not a blanket CI absence | Separate workspace doctests passed at the baseline according to handoff; none were executed on this continuation. |
| F14 | Release workflow-only changes miss release-specific selection | Workflow-only selection work is recorded in #71; selector and planner regression execution is pending on the current tree. |
| G01 | Renderable family and retry availability disagree | Current App/family wiring was inspected. The stale smoke selector is repaired here, but rendered native Ghost remains unproven. |
| G02 | Distinct four-word Loompad choices are a changed product contract | Current four-word contract and family retirement were inspected. Complete stable real Loompad words/WASD are not established. |
| R01 | Vestigial registry worker/operation framework | Registry reduction is recorded in #68; preserve supported ownership/error semantics and actual consumers. |
| R02 | Stale-ticket test does not exercise its named collision | Same-owner stale-ticket regression belongs to #68; baseline suite count alone is not an independently replayed mutation control. |
| R03 | Numerical controls are reimplemented in live sampling | Shared numerical controls belong to #70; retain deployed-adapter coverage rather than helper-only parity. |
| R04 | Disabled observations still allocate full-vocabulary snapshots | Observation-path reduction belongs to #70. No allocation or throughput improvement was measured in this continuation. |
| R05 | Heuristic template equivalence and self-authored parity expectations | Exact template dispatch belongs to #70; unknown identity and behavior-changing negative controls remain required. |
| R06 | Mechanical ticket/compatibility duplication | No consolidation performed: ticket ownership, authority and error semantics remain different, as directed by handoff. |
| V01 | Pinned native importer/backend correctness remains unverified | The exact pinned binding/importer was not executed. Metal initialization and component tests cannot close this finding. |
| V02 | Resource, cancellation and recovery bounds | No current-artifact long-prefill, blocked-consumer, active model-load, panic/reacquire or memory-pressure measurement. |
| V03 | Cross-process persistence guarantees depend on the injected store | No cross-process product-store interruption/conflict run here. Generic cache tests cannot confer these guarantees. |
| V04 | No current artifact-bound native writing acceptance | 10 path/navigation component tests and 21 AX-contract checks passed here. Actual Visual/Source Ghost/Loompad, insertion, unconsume, undo, stale scope, active quit and exact relaunch remain BLOCKED. |
| V05 | Materials privacy must match the actual storage contract | Handoff records rejection of protected retention before plaintext binding/evidence/temp publication; ordinary material retains the Unix-permission contract. No key store added. |
| V06 | Import/database preservation and schema compatibility | Handoff explicitly records import cancellation and schema preservation at f8588b3e. Exact native import, Finder, interruption/reopen evidence was not obtained here. |
| V07 | Fixture, capability and live authority separation | Live hosted credentials and physical microphone authority are unavailable. Fixture/capability/doctest boundaries remain explicit. |
| V08 | Build identity, portability and release evidence | Schema/release/pane repairs are preserved. Acceptance WebViews now request isolation; Rust config tests and a clean exact macOS artifact build are still unexecuted. |
| H01 | Hosted redirects and unknown usage | Handoff records local hosted redirect/usage controls passed. Live hosted credential acceptance remains BLOCKED, not inferred. |
| H02 | Mom asynchronous ownership and atomic store creation | Handoff records Mom ownership/atomic storage controls passed. Current-tree macOS re-execution is pending. |
| H03 | Mom/Speech/FTE terminal publication ordering | Handoff records terminal-before-release ordering controls passed. Actual affected product executors must be exercised on the current tree. |
| H04 | FTE Stop, terminal activity and database compatibility | Handoff records current FTE no-migration behavior: prior schemas rejected unchanged; retired v1 upgrade is not resurrected. |
| H05 | Speech first-admission cancellation and microphone revocation | Handoff records injected speech cancellation/revocation controls passed. Physical microphone acceptance remains BLOCKED. |
| H06 | Documentation, lints and no-Python enforcement | Handoff records policy/docs/lints/no-Python controls passed. This continuation performed only a changed-file whitespace check and added no Python; it did not rerun the repository suites. |
| H07 | Follow-up WAL contention and loopback fixture directory | Handoff records coordinated WAL retry and private loopback-token directory controls passed. Current-tree execution remains pending. |

## Promotion barrier

The f8588b3e baseline remains rejected for product acceptance. Its 1,895 workspace tests, separate doctests and other reported passes do not identify or accept a new artifact. The original Mac logs were unavailable here; the audit ZIP was recovered from Library without altering its original bytes.

A complete authorized Mac checkout must run the focused changed-area checks, then one consolidated pinned 1.92 gate, a clean release build and both exact-model writing journeys. Retain true inline Ghost, complete stable Loompad choices, current-family authority, no hidden cycling work, exact insertion, separate unconsume/undo, stale invalidation, joined active quit and exact-content relaunch.

Only after those pass may #64, #66, #67, #68, #69, #70, #71 and #72 be rebased/merged in order and final main tree equality proved. Current artifact/executable/signing/SDK/backend hashes are absent, not guessed. Hosted credentials and physical microphone checks remain separately BLOCKED where authority is unavailable. R06 remains separate. No hosted fallback, project reset, historical receipt rewrite, key store or database migration was introduced.

## Retained component-log identities

SHA-256 values identify raw continuation logs, not a macOS application or model:

```text
f58302abe59795a04f0c07d026c709c34311207c8d63babfc2ab4b515844d193  workspace-positive.tap
9a3b58c549d604df05ac14a279463fb517d5a7a114742431f19dd1a3e0040e40  workspace-negative.tap
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  swift-component-compile.log
2863c0e69311055c25e5d2416debf55335e7116bc1403f27a5b07ac646fc9d98  swift-component-tests.json
90891379e3e57755f275ef9a78016e98ddc1cce5349328884b88aa5e6f4032e4  swift-negative.log
7d514ceefed1d62e719d27af99847fb6a3cd86694d6ab84967472a88ffc7ced6  native-automation-blocked.log
4614f69ced9261417f1934506f32b935a79785255489c3f4c044e17d697281f6  rust-gate-blocked.log
227dfe566942f1ba06474cb0c64d92bf3064187f47ff196f6d482bdf9188f337  implementation-blobs.txt
```
