# Bounded local cabal history archival

This change follows `319b52f63ef802b042b1bef58dd1250c54114e24` on
`codex/loom-cabals-signal`. Main at
`fb3a1b2d5d64b85d148d064cda908912b689347c` remains incorporated.

## Behavior

The original Automerge graph and exact signed envelopes survive archival.
SQLite keeps a bounded recent set and independently archives older envelopes
in the transaction that admits new work. Compression is optional per envelope:
a small incompressible change keeps its exact JSON bytes in cold storage. No
snapshot replaces the causal graph, no authorship or membership proof changes,
and the wire still carries bounded original raw changes.

Crossing 1,024 recent changes or 8 MiB archives oldest rows until at most 512
changes and 4 MiB remain recent. Retention includes both shared and quarantined
writing, bounded at 250,000 changes, 512 MiB of exact encoded envelopes and
384 MiB of raw changes. The SQLite database has a 1-GiB page ceiling, including
metadata, indexes and the existing 256-MiB asset budget. Exhaustion refuses new
work without pruning old writing. Revocation and reactivation do not free or
recharge the retained quota.

Every persisted read verifies the envelope hash, signature, cabal, document,
causal identity and exact sizes. Compressed reads allocate a fixed bounded
output and require a complete zlib stream with no trailing bytes. Startup,
revocation, recovery and transmission decode one envelope at a time. Applying
changes to Automerge uses batches of at most 128 changes and four MiB; serialized
history is never collected into a lifetime-sized buffer. The complete bounded
causal graph still lives in Automerge memory.

Routine admission uses the verified change index, advances a small raw frontier
and stages only newly sealed ancestors. These caches change only after SQLite
commits. Matching peer inventories skip the historical ancestor walk.
Quarantine stays in the same history rows; reactivation
changes its status without reserializing or recompressing the original payload.
The store is version 6; unsupported stores are preserved and refused. Membership
and workspace protocol remain version 2 and protocol 3 respectively.

## Evidence

The consolidated command `cargo test --locked -p loom-cabal -p tauri-plugin-loom`
passed 365 tests, with five existing ignored plugin cases. The log is
`/tmp/loom-archive-final-gate-2.log`. The 41 cabal unit tests completed in 46.55
seconds; protocol, attachment, compute and plugin tests also passed.
Strict Clippy passed for both packages and all targets; the log is
`/tmp/loom-archive-clippy-final.log`. Workspace formatting, current-document validation
and `git diff --check` also passed.

Six archive cases establish:

- 20,011 signed changes exceed the old 20,000-change limit. More than 19,000
  rows are archived. Reopening preserves every envelope hash and the exact
  original signed creation envelope. An edit from the original offline basis
  retains the latest independently renamed document and the new writing.
- Fifty valid changes carrying one-MiB commit messages exceed the former 64-MiB
  lifetime envelope cap. The recent set remains within eight MiB, stored bodies
  occupy less than a quarter of their original encoded bytes, and reopening
  preserves every hash and the manuscript.
- A trigger fails the fourth archival update. Previously changed representations,
  the new edit, counters and causal state all roll back. Removing the trigger
  allows the same edit to commit and survive restart.
- 1,101 private offline changes, including archived rows, remain recoverable
  after removal and reopening. Retained byte and count budgets remain unchanged.
- A new owner seal initially quarantines 1,101 earlier changes. Paged catch-up
  restores all proven ancestors without duplicate storage. Previously archived
  bodies retain their exact compressed bytes, and both peers' retained counts
  and encoded-byte totals agree.
- Corrupt compressed bytes, a truncated stream, trailing bytes and a false
  decompressed length are refused on reads or reopening.

An initial large-history run exposed repeated per-change Automerge validation.
A live process sample at `/tmp/loom-history-sample.txt` identified that cost.
The obsolete test process was deliberately stopped after the implementation
changed to bounded application batches; its unfinished large case is not a
passing receipt. All six archive cases passed in the final consolidated run.

## Remaining acceptance

This receipt establishes storage, local protocol and application-store behavior.
Native acceptance of store 6 remains separate. Owner-device recovery/transfer,
phone-linked Signal, physically separate Internet peers and final release
acceptance remain unfinished. No Signal account was linked and no message or
group edit was sent during this work.
