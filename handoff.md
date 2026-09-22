# Loom audit-next integration handoff

Date: 2026-09-22
Branch: `codex/loom-audit-next-87939f38`
Base: `87939f3873977ef2670ddab9e869ca9d5d7d1739` (`origin/main`)
Source packet: `/Users/george/Downloads/loom-audit-next-87939f38`

## Integrated

Applied the packet patch `patches/0001-current-completion-control-driver.patch`
with the packet's exact preparation tool. It changes only:

- `scripts/macos-smoke-support/README.md`
- `scripts/macos-smoke-support/exercise_loom_completion_controls.swift`
- `scripts/macos-smoke-support/set_loom_completion_toggle.swift`

The patch replaces obsolete no-model control automation with the shared current-
control driver, enforces one input per observed transition, and adds bounded,
read-only completion-witness diagnostics. It does not change generation,
renderer behavior, storage, schemas, dependencies, release gates, or EASL.

## Verification performed

- `node --test /Users/george/Downloads/loom-audit-next-87939f38/tests/*.test.mjs`
  — 11 passed, 1 intentional Linux non-macOS refusal skipped.
- `rustup run 1.92.0 cargo run --offline --locked -p xtask -- macos-smoke-support ...`
  — compiled all support helpers successfully.
- Generated `set_loom_completion_toggle --self-test` — 344 assertions passed;
  reports `native_acceptance: false`.
- `node --test scripts/ci/test-workflows.mjs` — 49 passed.
- `rustup run 1.92.0 cargo fmt --all -- --check` — passed.
- `rustup run 1.92.0 cargo run --offline --locked -p xtask -- policy` — passed.
- `git diff --check` — passed.

The packet's Node tests also rejected five compiling behavioral mutants:
duplicate input, numeric Boolean, metadata-as-proof, wrong mode action, and
retired labels.

## Explicit non-claims

This is not product acceptance. No native AX/DOM observation, foreground app
journey, pixel-level visible in-caret Ghost proof, real-model generation,
artifact/signing identity, relaunch, shutdown-join, or Visual/Source editor
acceptance was run. The Linux test intentionally exercises refusal behavior,
not native success. The completion witness remains diagnostic and must not be
treated as proof that a Ghost is visible.

## Serious remaining work and findings

1. Run the helper against one clean, committed macOS artifact in an isolated
   acceptance session. Verify policy off/on, Ghost/Loompad transitions, busy and
   delayed acknowledgements, wrong/frontmost PID, multiple windows, and
   missing/ambiguous controls. Preserve strict failures; do not add retries.
2. Add the bounded post-terminal AX/DOM observation described in
   `COMPLETION-TRACE.md`, correlated to durable candidate identity and the
   canonical manuscript. Keep pre-terminal failure evidence separate.
3. Only after that observation identifies the falsified hydration/publication
   edge, add a failing regression and repair the implicated renderer/controller
   boundary. Do not infer a renderer fix from helper metadata.
4. Prove both Visual and Source journeys: true in-caret Ghost, four distinct
   Loompad choices, cycling without a hidden fifth run, exact accept, separate
   unconsume and ordinary undo, stale-scope rejection, worker joins, and
   exact-content preference-preserving relaunch.
5. Finish artifact hashes/signing/model identity and the settled Rust/frontend/
   policy gates. Keep Linux and Windows evidence separate from macOS evidence.

All 37 inherited audit findings remain recorded in the packet's
`FINDING-EVIDENCE.json`; this follow-up requalifies only the control-driver and
diagnostic portions of F12, V04, and V07. It does not close those findings or
the other rows.

## Next agent request

Start from the pushed commit below. Review this file and the packet's
`LOCAL-AGENT-PROMPT.md`, `COMPLETION-TRACE.md`, and `NEXT-TODO.md`. Treat all
portable results above as component evidence only, and return native receipts
for any acceptance claim.

## Streaming-preview repair follow-up — 2026-09-22

Packet: `/Users/george/Downloads/native-platform-streaming-preview-repair-20260922`

The packet was applied onto this branch after the prior control-driver commit.
It changes the Loom streaming-preview/controller/editor path and adds the
packet's browser regressions and contract documentation. The packet's safe
apply utility passed 9/9, and `pnpm --filter @delysis/loom check` passed with
zero Svelte diagnostics.

Focused execution found unresolved regressions, so this revision is not a
product or test-suite acceptance claim:

- Unit suite: 121 passed, 1 failed in `ghostText.test.ts` (`Option-Return`
  requires the new exact visible-widget witness in a legacy mock).
- WebKit suite: 85 passed, 3 failed. Two new growing-prefix cases remain at
  the old preview (`one two`) after `appendStream`; one existing exact-byte
  case cannot find the full-text widget by its old word-level locator.
- The packet's portable probes and source syntax receipts remain historical
  component evidence; they do not override these current failures.

Queued work for Chat 6 Pro:

1. Diagnose why the browser harness refresh does not publish the new
   presentation identity/text into the mounted editor, without weakening the
   strict prefix, stale-session, pending-insertion, retired-policy, or actual
   DOM-text guards.
2. Repair the exact-widget witness test seam (or production behavior if the
   browser reproduction proves it) so Option-Return remains authorized only
   when the real widget text/key/geometry agree.
3. Re-run the focused unit and real-WebKit suites, then the consolidated Rust,
   policy, frontend, and workflow gates on one settled commit.
4. Only after those pass, build a fresh macOS artifact and run the packet's
   approved Gemma 4 Visual and Source journeys. Native acceptance, signing,
   model identity, relaunch, lifecycle joins, and rendered multi-frame proof
   remain open.
5. Do not infer bounded four-way speculative scheduling from this repair; the
   packet explicitly does not implement that separate scheduler.
