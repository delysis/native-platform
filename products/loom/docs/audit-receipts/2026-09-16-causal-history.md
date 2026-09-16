# Causal history synchronization and owner seals

This change follows `43f12f9594789504291b6be528bfdeed1f3715d1` on
`codex/loom-cabals-signal`. Main at
`fb3a1b2d5d64b85d148d064cda908912b689347c` remains incorporated.

## Behavior

Synchronization now advertises each document's stored graph heads and missing
dependencies, rather than every historical envelope hash. Each set is bounded
at 256 entries per document. A response sends at most 128 signed changes within
the existing frame budget. Partial graphs survive restart and resume from their
missing dependencies.

An owner's revocation seal names document-scoped Automerge heads. Their causal
dependency hashes commit to accepted history. A late joiner receives those heads
before their ancestors, so old-epoch changes can be verified incrementally. Every
envelope still requires its own valid author signature, matching Automerge actor,
cabal/document binding and non-future epoch. An admitted member cannot authorize
an unsealed old change just by depending on it. Earlier seals remain available
while an owner catches up; unresolved new work cannot block removal of a device
or confer authority on an unknown dependency.

Quarantined changes return to active history only when a sealed descendant proves
their inclusion. The exact signed envelope remains available throughout that
transaction. Unsealed offline edits remain recoverable. Transaction failure keeps
the prior causal and authorization indexes; reopening checks stored envelope
hashes instead of trusting a corrupt SQLite key.

The application preserves its saved local file bindings while a graph is
incomplete. A partial page cannot cause an existing manuscript to be published
again under another document identity. Local writing remains in its original
file and later merges from the saved projection basis. Explicit recovery can
save both visible file changes and an unsent editor draft privately, even before
the complete graph is available.

The cabal store is version 5, membership is version 2 and the workspace protocol
is `app.delysis.loom/cabal/3`. The signed document payload remains version 2.
Unsupported experimental stores are rejected without rewriting their bytes.
Private projection records never enter the network inventory.

## Evidence

Nine new store/protocol tests cover:

- A late joiner recovering 400 edits from a removed author, reopening after the
  first 128-change page, and converging to the exact original envelope hashes.
  This case's roster remains below 2,048 bytes and its completed inventory below
  512 bytes.
- Quarantine recovery after a new seal arrives before its missing descendants,
  while unrelated offline writing remains private and recoverable.
- Rejection of an old unsealed dependency referenced by a current member, plus
  successful removal of that member despite the unresolved dependency.
- Document-scoped seals, preserved earlier seals during owner catch-up, malformed
  or oversized inventories, and unchanged bytes after unsupported-store refusal.
- A failed fourth INSERT rolling back the whole page and leaving the prior
  in-memory state intact; reopening rejects a corrupted stored envelope key.
- Actual local QUIC carrying more than one page of sealed history to a newly
  admitted device, with both owned endpoints joined at shutdown.

Two app-store regressions failed before their repairs:

- Partial catch-up incorrectly created a replacement shared document. The fixed
  test restarts the partial store, projects repeatedly without publishing another
  identity, preserves a local file edit, and then merges both authors into the
  original registered manuscript.
- Explicit recovery failed with `Invalid shared document shape` while the graph
  was incomplete. The fixed test recovers separate visible-file and unsent-draft
  copies under `Recovery/`, leaves the source file unchanged and creates no new
  shared change.

The final command `cargo test --locked -p loom-cabal -p tauri-plugin-loom` passed
359 tests; five existing plugin tests remain ignored. The final log is
`/tmp/loom-causal-frontier-final-gate-2.log`. Reproductions and focused repairs are
recorded in `/tmp/loom-causal-projection-before.log`,
`/tmp/loom-causal-recovery-before.log` and `/tmp/loom-causal-recovery-after.log`.

Strict Clippy passed for both packages and all targets after replacing seven
bare test `unwrap()` calls with descriptive expectations. That diagnostic-only
repair is recorded in `/tmp/loom-causal-frontier-clippy-final.log`. Workspace
formatting, current-document validation and `git diff --check` also passed.
Required macOS CI and the dependency audit passed on the preceding `43f12f9`
commit; those results do not establish CI acceptance of this change.

## Remaining scope

This establishes store, local transport and app projection behavior. Native UI
acceptance of protocol 3 remains separate. The 20,000-change/64-MiB lifetime cap
has not been raised: bounded archival, with retained signed provenance and usable
offline bases, is still required. Owner-device recovery/transfer, linked Signal,
physical Internet peers and final release acceptance also remain unfinished.
