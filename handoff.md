# Loom streaming-preview continuation

Date: 2026-09-22
Branch: `codex/loom-streaming-preview-repair-20260922`
Follow-up base: `38ed1f26b31544d9bc76e064d2d21e795784614a`
Base tree: `1dde36011de939b8f1eb2d7c34170890e46c59c0`

**Native acceptance remains open. Do not promote from portable checks.**
The prior handoff and raw Mac results remain in Git history at the base above.
No historical receipt, production renderer/controller, native engine, dependency,
schema, acceptance gate or user data is changed by this follow-up.

## Disposition of the four reported failures

The two growing-prefix failures originate in the test harness lifecycle. It
admitted a fixture session before the mounted editor established its end caret.
The initial Visual selection report, or Source focus before setting the end
selection, invalidated that session. `completionControllerView` could still show
the unchanged fallback family, concealing the lost session. `appendStream` then
silently did nothing because it requires a live session.

The harness now admits its fixture family only after the real editor focuses its
end caret. Manual text changes and caret navigation retire both the session and
its fixture source. The stream method reports whether the production transition
actually applied; browser regressions require a bound session, full multi-frame
DOM growth, growth after word acceptance, exact unconsume, and no resurrection
after manual edits or a move away and back. These checks do not inject native
model output or replace the artifact journey.

The Option-Return unit's positive DOM mock lacked `textContent`, so the strict
production widget predicate correctly rejected it. The fixture now supplies the
full expected text. Six negative variants retain rejection for missing,
truncated or rewritten text, a stale key, an offscreen caret and an offscreen
widget. The production guard is unchanged.

The terminal-space browser test supplied `lingers here` but searched for a
standalone `lingers` node. It now requires the actual widget's entire `lingers
here` text and visibility, retaining the exact `Something lingers` insertion
and `here` remainder assertions. There is no one-word render cap.

## Executed here; not a replacement for the Mac runs

The editing runtime was Linux with Node 22.16.0 and TypeScript 5.8.3, not the
repository's pinned TypeScript 6.0.3. It had no pnpm, Vitest, Svelte, WebKit or
Rust toolchain; registry access also failed. No full unit/browser/typecheck,
Rust, release, signing or model run is claimed.

- Source-script lifecycle replay: 4 failures on the original harness, 4 passes
  after repair. It executes the actual harness script and production controller
  with explicit simulated editor focus callbacks; it does not compile Svelte.
- Three executable harness mutations were rejected by assertion failures:
  early admission, retained revoked fallback, and a dropped stream update.
- Existing streaming-controller test bodies: 12 passes through the Node test
  registration adapter against the production modules, not pinned Vitest.
- Unchanged production widget predicate: 7 synthetic DOM/geometry cases pass;
  this is not ProseMirror keyboard dispatch, real DOM or native visibility.
- Changed TypeScript/script syntax and `git diff --check`: pass.

The conversation follow-up bundle preserves commands, raw logs, test adapters,
source identities and the patch. Do not count its portable checks as extra
product gates or rerun them instead of the real suites below.

## Local execution: focused first

Preserve unrelated work. Fetch this branch, inspect status and fast-forward the
existing clean repair worktree; use a new worktree if it contains other changes.
Do not reset, overwrite or reapply the previous 13-file packet.

From the repository root, use the existing pinned pnpm installation:

```sh
pnpm --filter @delysis/loom check
pnpm --filter @delysis/loom exec vitest run \
  src/lib/ghostText.test.ts src/lib/completionStreaming.test.ts \
  src/lib/completionController.test.ts src/lib/completionSession.test.ts \
  --maxWorkers=4
pnpm --filter @delysis/loom exec vitest run \
  --config vitest.browser.config.ts \
  src/lib/completionController.browser.test.ts \
  src/lib/editorInteractions.browser.test.ts \
  src/lib/loompadStreaming.browser.test.ts
```

If a real suite fails, retain its exact test name, assertion, screenshot and
source revision. For a stuck preview report the bound session, selected run,
presentation key, actual DOM text and caret before/after the delta. Do not add
arbitrary sleeps, waive the DOM check, or change a full-prefix assertion to one
word. Once focused tests pass, run one settled consolidated local gate using
`rustup run 1.92.0 cargo ...` for Rust; do not trust old-tree counts. GitHub billing
failures are not product-test results.

## Native acceptance and promotion

Build one clean identified candidate. Use the existing native smoke and writing
journeys with the approved Gemma model, never fixture text or hosted fallback:

- Path: `/Users/george/.cache/fiction-harness/models/gemma-4-E2B-base-Q8_0.gguf`
- Size: `4954576032`
- SHA-256: `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`

Require correlated pre-terminal multi-frame in-caret Ghost in Visual and Source,
four distinct stable Loompad choices with growing tails and fixed W/A/S/D
identity, unchanged-manuscript cycling without hidden extra work, exact insertion,
separate unconsume/ordinary undo, stale-scope refusal, active-work quit with owned
worker joins, and exact persisted bytes plus fresh completion on relaunch.
A bounded post-terminal diagnostic may explain a failure; it must not convert a
failed pre-terminal gate into PASS. Preserve raw logs and source/artifact/model
hashes. Promotion belongs to the local agent only after the exact tree passes.

The exhaustive four-way lookahead scheduler is separate and is not implemented
here. See `products/loom/docs/streaming-completion.md` for the current contract.
