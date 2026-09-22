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
