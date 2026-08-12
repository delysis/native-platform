# R5 foreground-command audit receipt

Date: 2026-08-11

Worktree: `/Users/george/.codex/worktrees/phase1-r5-integrated`

Exact base: `d0aca6ff4883ac51514fea5e5fb75ffbb3c8c264`

Foreground implementation revision: `e78111cbc754fa285153623ab8791e3923494460`

Launched-acceptance revision: `c6d676ce2b2896c45db13c509605f8f64eaa475f`

## Accepted claim

`VerifiedForegroundCommand` means only that the trusted application host
accepted one focused, one-use command in this process for the exact pending
promotion. It does not prove physical user presence, OS-authenticated input,
biometric identity, or safety after host-process compromise.

## Implemented path

- The host registry binds a random nonce to process/application session,
  native window and focus epoch, document, candidate, command, canonical
  promotion fingerprint, and bounded expiry. It assigns a monotonic event
  index only during successful atomic consumption.
- Host window maps and pending challenges are bounded. Expired challenges are
  pruned before admission.
- Tauri installs the focus event listener before its initial focus sample and
  rechecks `Window::is_focused()` at the consuming command edge. That reading
  is moved through an opaque registry/window-bound sample that expires after
  one second and must postdate the challenge. Production construction accepts
  the native Tauri window and queries it internally; callers cannot supply a
  Boolean focus claim. There is no unchecked public `consume` method or
  naked-boolean production consumption method.
- Project and application close revoke foreground authority and clear pending
  research decisions before generation or model drain.
- The production `research_promotion_import` controller opens a native file
  picker, reads one bounded regular non-symlink packet, admits its exact
  mixed-authorship record in the active store, derives the current source and a
  fresh command identity in Rust, and calls the private staging edge with the
  resulting non-serializable lease. Renderer IPC supplies no path, packet
  bytes, source identity, command ID, lease, or authority.
- The production `research_promotion_confirm` command consumes the submitted
  one-use binding at the native window edge and passes the move-only value
  directly to the store.
- The store transaction validates the live subject lease, durable request,
  current revision, visible source, and exact result bytes, then atomically
  commits the derived foreground receipt, selected manuscript revision,
  provenance operation, ordinary command receipt, generic command request,
  and pending visible-file outbox row. Projection is settled after semantic
  commit under the existing recoverable outbox contract.
- The quiet writing surface exposes pending research decisions only inside
  Writing options and an explicit modal. It does not add research chrome to
  ordinary writing.

## Schema reconciliation

R4 migration `0010_token_piece_evidence.sql` remains unchanged. R5 is the
additive `0011_foreground_command_receipts.sql`; current store version is 11.
The v9 regression executes v10 then v11, verifies both strict tables and both
migration rows, and reopens version 11 idempotently.

## Verified evidence

The following targeted regressions completed successfully:

- `tests::foreground_registry_bounds_windows_and_pending_challenges`
- `tests::native_focus_recheck_fails_closed_and_spends_nonce`
- `tests::native_focus_sample_is_bound_to_registry_and_spends_nonce`
- `tests::stale_native_focus_sample_fails_closed_and_spends_nonce`
- `schema::tests::version_eleven_adds_foreground_receipts_after_token_piece_evidence`
- `research_admission::tests::database_cannot_reconstruct`
- `tests::production_research_packet_reader_admits_only_bounded_regular_files`
- `tests::production_research_confirmation_promotes_in_one_store_contract`
- frontend: 30 files, 177 tests
- frontend Svelte check: 0 errors, 0 warnings
- frontend production build (with only the existing large-chunk advisory)

The final combined-worktree gates also completed successfully:

- `cargo test --workspace --all-targets` (630 passed; 8
  environment-dependent real-model, live-frontier, and
  preserved-Python-fixture tests remained explicitly ignored by their
  existing gates)
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `rustup run 1.88.0 cargo check --workspace --all-targets`
- `./scripts/check-workflow-policy.sh`
- `cargo fmt --all --check`
- `git diff --check`

## Evidence boundary

The first launched exact-head bundle exposed a real main-thread deadlock: the
synchronous `research_promotion_import` Tauri command invoked the native
dialog's `blocking_pick_file` while WebKit IPC was holding the main thread.
Revision `c6d676ce2b2896c45db13c509605f8f64eaa475f` makes that command async,
matching the working model picker. Renderer authority remains unchanged, and
the native focus check still occurs after selection and before the packet is
read or staged. The full plugin suite passed 71/71, the focused research suite
passed 3/3, the renderer authority suite passed 3/3, and focused Clippy and
formatting checks passed.

The corrected exact-head bundle was then exercised natively:

- app: `Loom R5 c6d676c.app`
- bundle identifier: `app.delysis.loom.r5.c6d676c.acceptance`
- executable bytes: `20,841,280`
- executable SHA-256:
  `f0c80f2c3051d4f21c327c5c96d2bedf8c8ec6e51dd0fe73c24e5d71ca7105d8`
- model: `gemma-4-E2B-base-Q8_0.gguf`, 4,954,576,032 bytes, selected through
  the native model picker
- model fingerprint shown ready by the app:
  `9d53598892698e981fc42f78b0f8c005cecd63ca`
- human prefix: `At dawn, the locked observatory began to breathe.`
- observed result: a visible caret-local continuation and the accessibility
  status `Suggestion available. Tab accepts; Escape dismisses.`

The separate reviewed-research acceptance used an exact excerpt from
`03_scene_lab/runs/prompt-autoresearch-v15-branch-loom/research_review.v1.md`,
lines 7-9. The excerpt is 483 bytes with SHA-256
`70b8fe3d21679286fac6ec9205d30dd56f9db7a16ec1e3059c1a7fa46a4e711a`.
Its operation is truthfully recorded as `historical_text`; it is not attributed
to the Gemma suggestion run. The source review explicitly says that the V15
campaign stopped without promoting a winner. Promoting this reviewed finding
into the acceptance manuscript does not reclassify or promote a V15 fiction
candidate.

Without switching applications or inspecting the store between file selection
and confirmation, the native import returned, displayed the exact excerpt,
offered a separate one-use foreground decision, and accepted `Promote reviewed
result`. The launched UI then showed `Research selection promoted` and the
exact excerpt in the manuscript. Read-only post-confirmation inspection found:

- command `01KZST7KEASR0BJ6AVAXSXA3AY`, kind `promote_candidate`
- foreground receipt blob
  `f238f694ea01df2b61a7bdc3cb0079c4f829a18f01c3be2426eedefad57ecd2f`
  with schema `loom.verified-foreground-command-receipt.v1`, claim
  `trusted_application_host_accepted_one_focused_command`, and subject kind
  `mixed_authorship`
- revision `01KZST7SCTWZ66WPE955DPPRPT`, reason
  `foreground-authorized research promotion`, contribution kind `mixed`
- provenance operation `01KZST7SCT01001SNC8JPMAKDS`, kind `select`, metadata
  decision `research_promote`
- admitted operation `01KZST5NAMD5E8V2EG0CV4PB9Q`, kind `historical_text`,
  referencing the exact excerpt digest
- completed visible-file outbox row whose target is that same digest
- visible manuscript length 483 bytes and SHA-256 equal to that same digest

This proves the launched native selection, focused confirmation, exact visible
projection, and durable receipt chain for this candidate revision. It does not
claim physical user presence, OS-authenticated input, biometric identity,
base-writer-only provenance, or promotion of a V15 fiction winner. The branch
was pushed for draft-PR CI; it was not merged or published as a release.
