# Loom completion stabilization acceptance — 2026-08-23

Date: 2026-08-23  
Evidence class: designated-machine packaged-product acceptance  
Verdict: accepted for the scoped macOS release-candidate slice

## Superseded handoff

This receipt closes the six unresolved completion-interaction items in
`2026-08-22-completion-stabilization-handoff.md`. That handoff remains immutable and correctly
records that its earlier runs were not acceptance evidence. Only the exact archive and receipts
identified below support this acceptance claim.

## Immutable release subject

- Runtime source revision: `9905b3231ce8b1f6a14ad72b403339451cbf36e4`
- Runtime source tree: `aa60ce02ae3cfba974a060e440a4ee84c7ffd186`
- Source state at build: clean
- Branch at build: `codex/loom-completion-lifecycle`
- Bundle identifier: `app.delysis.loom`
- Release kind: `candidate`
- Signing: ad hoc; strict deep signature verification passed
- Release directory:
  `/Users/george/.codex/worktrees/native-platform-production-release/dist/macos/loom-v0.1.0-9905b3231ce8-20260823T045223Z`
- Archive: `Loom.app.zip`, 8,733,704 bytes
- Archive SHA-256: `f7b76fbac6aa0cee4a6f1aa4eeb43a584e2fb5e9e90298dfae53e363b96f363e`
- Executable SHA-256: `39b6debbec862c27495c0caec0d6da36a86d2f7e8f1b6052216e09c38e0a134e`
- Release receipt SHA-256:
  `9fba9c5c3157cf42ae2b1f48ea9d7ddca0608bc36204e16c3dad19be21dd51dd`
- Model-free smoke receipt SHA-256:
  `36090edb0520faac4764f47311843d330fcdb860d07bab7dbb0b00a1b28dd069`
- Real-model smoke receipt SHA-256:
  `f80d37de138928e272e5ccd1bed4e69d2b1a28e28c15e254814d5db30f5e0c56`

The release receipt binds the archive and executable to the clean source revision. Both smoke
receipts independently bind the same archive, executable, and release-receipt hashes.

## Exact model provenance

- Model: official `google/gemma-4-12B-it-qat-q4_0-gguf` snapshot
  `29d097773436b69ff9feafd636ab4cf873786537`
- GGUF bytes: 6,975,879,296
- GGUF SHA-256: `93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b`
- Source device/inode: `16777231:94637768`
- Runtime admission: the smoke created an isolated hard link and rejected any device/inode drift
  before launching the app.
- Runtime model-environment artifact:
  `01M0PFGKZASTGYJM1YN63MHSKH`
- Model-environment artifact blob:
  `8732a2111a62b40be6ff617925817f77ab391f735066eb3ee0d7eace662ac11b`
- Runtime environment ID:
  `ff385278db197e5c234e39c8ea5e2afe56e6f268fe044077d4a23fe6c9425149`
- Runtime `model_identifier`, `model_fingerprint`, and `tokenizer_fingerprint` all bind the same
  `93567e57…a538b` model SHA-256.
- Runtime backend identifier:
  `llama-native-build-v3-31cb719af12f1f462f0c5362f036b275e95e43dc05307a13265c37009b9d0fcf`

The generated family therefore came from the exact admitted GGUF, not a fixture, an embedded
weight, a different cache entry, or a static browser test.

## Defects designed out

### Project-operation admission

- Native session admission now waits at the bounded lock rather than surfacing a normal overlap as
  `project_busy`.
- Renderer retries are limited to typed current-session/transition cases; unrelated errors still
  fail closed.
- chooser, export, model-application, close, and startup transitions release admission through
  scoped lifecycle guards, including error and cancellation paths.
- current-project identity is explicit throughout transitions, and close/startup cannot race a
  detached operation into the next session.

The final real-model run continuously scanned the exact PID's accessibility alerts for 1,190
polls. It observed no `project_busy` alert. Post-exit stdout/stderr and durable command receipts
also contained zero `project_busy` events. The model-free run independently observed the same
result across 771 polls.

### WYSIWYG terminal prose and formatting palette

- Visual Markdown preserves one writer-entered terminal prose space without broadening the safe
  parser/serializer identity boundary.
- The palette opener is a real state-owned button with `aria-expanded`; it no longer depends on
  WKWebView's false-actionable `<summary>` exposure.
- palette selection is captured by exact document identity, follows deliberate focused-editor
  selection changes, and is restored from an immutable post-command selection after late WebKit
  accessibility/contenteditable reconciliation.
- corrective focus/selection restoration is tagged so it cannot masquerade as caret navigation or
  start inference.
- structural unwrap, inline terminal-space trimming, and existing-link replacement are single,
  reversible transactions.

The packaged run exercised 18 exact persisted stages: Title/Body, Heading/Body,
Subheading/Body, Bold/on-off, Italic/on-off, Block quote/on-off, Bulleted list/on-off, Numbered
list/on-off, Link, and Remove. Every stage retained the exact accessible selection and returned
focus to the manuscript. The final Markdown and its terminal space exactly matched the initial
sentinel.

### Completion presentation and session identity

- completion sessions are keyed by exact project session, document, revision, mode, candidate,
  run, and presentation identity.
- one shared cached engine serves visual autocomplete, the four-choice fan, Return/Tab/Right, and
  Shuttle; mode toggles do not invent adjacent inference.
- first acceptance freezes candidate authority; rollback is immediate and preserves the cached
  remainder.
- exhausted-family handoff is atomic, while pre-exhaustion cached interactions cannot admit a
  replacement family.
- physical bare-Option state is reconciled across focus, blur, visibility, pagehide, pointer, and
  modified-key boundaries. Cmd-Option and Ctrl-Option are not treated as bare Option.
- ghost widget identity includes the rendered presentation, preventing stale DOM reuse. The fan is
  an accessible four-option list with human-readable labels and exact ordinal-to-run joining.

## Real-model acceptance sequence

The exact archive was launched into fresh isolated app-owned state with autocomplete off. The
terminal-space sentinel was typed and durably reconciled before autocomplete was enabled exactly
once.

One family was admitted at the exact byte-39 boundary:

- `01M0PFGKYZ3ZWVMR9Z0H9YG2A1`
- `01M0PFGKYZ4C81DFW45QCEP0CZ`
- `01M0PFGKYZB4Y74CHBTWQDYSKQ`
- `01M0PFGKYZRZ6E6YM9C7H3MJQW`

All four terminals reached `completed`. For every run, the terminal candidate equaled the durable
candidate and terminal-evidence candidate; candidate/evidence output blobs were equal;
candidate/evidence token-trace artifacts were equal; and the generated-span artifact equaled the
terminal output artifact.

The native interaction checks then proved:

1. visible ghost presentation announced `Suggestion available.`;
2. bare Option exposed four non-empty accessible choices joined to the exact four run IDs;
3. Up/Down changed and restored the selected cached candidate;
4. Option-Right accepted one cached word and Option-Left restored the exact prior manuscript bytes
   and ghost remainder;
5. Return and Tab accepted the selected fan candidate, and each acceptance was exactly rolled back;
6. Shuttle used the same frozen cached session, hid inline ghost while active, accepted one cached
   word, and rolled back without inference;
7. disabling the engine cleared the presentation and session;
8. the generation count was four before and four after all interactions.

The generation guard ran for 530 polls with zero unreadable polls. Its maximum and final observed
counts were both four; no fifth run was observed. The original manuscript SHA-256
`f0429dd3ff247b59f50f8fde46bf0cead66453be958090ad9a21dafa611e1395` was restored before
formatting and matched again after formatting.

The app then created and focused a new manuscript, quit through the exact native Quit command,
reopened the same isolated project in a second launch, reproduced the original manuscript hash,
and quit with status 0 again.

## Deterministic gates

- `svelte-check`: 0 errors, 0 warnings
- Loom unit tests: 43 files, 297 tests passed
- real WebKit editor interactions: 1 file, 23 tests passed
- workflow contract tests: 33 passed
- release-gate Rust tests: migration/reopen, exact-boundary promotion/reopen, and active-family
  close/cancel/replay passed
- production frontend build: passed
- Tauri release build: passed
- `cargo fmt --all --check`: passed
- `sh -n scripts/smoke-macos-app.sh`: passed
- embedded Swift acceptance helpers: compiled during the passing native smoke
- `git diff --check`: passed
- strict deep code-sign verification: passed
- model-free exact-archive smoke: passed twice
- official-Gemma exact-archive smoke: passed twice

## Negative evidence retained

Earlier candidates are not silently promoted. They exposed, in order, an admitted-versus-completed
family race, inaccessible machine-only fan metadata, a WKWebView `<summary>` AX activation trap,
stale palette selection under click-only activation, post-command WebKit focus/selection
reconciliation, and a single-lookup verifier race while the AX subtree rebuilt. Each failure led to
a product or verifier invariant above. None is counted as acceptance evidence.

## Boundary of the claim

This receipt proves the scoped native macOS candidate on this designated machine: exact archive
identity, official local Gemma generation, four completed and provenance-matched candidates,
visible WYSIWYG ghost text, cached fan/Option/Return/Tab/Shuttle/rollback behavior, terminal prose
space preservation, all visible palette controls, bounded project admission without a user-facing
`project_busy`, clean close, persistence, and relaunch.

It does not claim notarization, Gatekeeper assessment, Developer ID distribution signing,
cross-platform UI acceptance, latency/thermal targets, or stable-channel publication. The release
receipt explicitly records notarization as `not-requested`; this artifact remains an ad-hoc-signed
candidate.
