# R5 foreground-command audit receipt

Date: 2026-08-11

Worktree: `/Users/george/.codex/worktrees/phase1-r5-integrated`

Exact base: `d0aca6ff4883ac51514fea5e5fb75ffbb3c8c264`

Implementation revision: `e78111cbc754fa285153623ab8791e3923494460`

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

No launched desktop product was exercised for this combined R5 worktree yet.
The production native import/staging command, opaque native-focus consumption,
store mutation, and progressively disclosed frontend flow have compiled and
passed their automated regressions; that is not elevated into launched-UI
acceptance. The required launched-product promotion remains an explicit final
acceptance gate. The implementation is committed locally at the immutable
revision above. No push, merge, dependency publication, or remote branch
change was performed.
