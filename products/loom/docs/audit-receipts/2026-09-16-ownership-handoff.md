# Cabal ownership handoff and offline divergence

This change follows `a29c32a3f39e4e60971ad2c0c5a9de6773f83682` on
`codex/loom-cabals-signal`. A fresh fetch confirmed that main at
`fb3a1b2d5d64b85d148d064cda908912b689347c` is already incorporated.

## Behavior

The current owner can review an existing device under **Hand over the keys**.
Confirmation binds the recipient and exact signed membership hash. A changed
membership requires another review, and a lost reply never repeats the action
automatically. The native command also checks the active workspace and session.

The owner signs a delegation containing the successor, prior roster and authority
hashes, revision, epoch, and exact initial membership state. That signature can
publish only the committed initial handoff. Later membership decisions require
the successor's key. Peers pin the known chain: conflicting branches are refused,
and an earlier owner cannot reclaim authority by advertising a higher revision.
An offline peer can verify intervening delegations from its original trust root.
The chain is bounded at 64 handoffs, with checked membership counters.

Handoff changes administrative authority without changing the writing epoch.
Existing pairing and offline edits remain valid. The previous owner remains a
writer until removed; only the successor can admit or remove members. A removed
original owner can still receive the current signed revocation over QUIC.
The roster transaction commits before any in-memory ownership changes.

A separate reproduced sync defect occurred after revocation sealed more than
one page of history and two peers then edited offline. Each peer repeatedly sent
the same first 128 already-held sealed changes, starving their newer edits.
Inventories now acknowledge stored sealed roots with one bit per root, bound to
the exact roster hash. These roots let the sender recognize shared ancestry even
when it has not seen the recipient's new tip. Missing dependencies still stop
the ancestry walk. Stale-roster bits are ignored; malformed matching inventories
are rejected. No author signature or membership authorization is relaxed.

The current cabal store is version 7, membership schema 3, and workspace ALPN
`app.delysis.loom/cabal/4`. Incompatible experimental stores are preserved and
refused, without migration. Document payload schema remains 2.

## Evidence

The failing-before sync reproduction is
`sealed_history_does_not_starve_two_peers_diverging_offline` in
`loom-cabal/tests/causal_history.rs`; its log is
`/tmp/loom-sealed-divergence-before.log`. After 150 prior edits and a revocation,
both peers independently change different paragraphs. Before the fix, eight
bidirectional exchanges still leave different histories and text. After the fix,
both histories and both paragraphs converge.

Nine ownership cases cover offline writing and restart; stale reviews and invalid
recipients; former-owner replay and forged rosters; conflicting chains and
cross-cabal rebinding; injected SQLite rollback and retry; actual local QUIC
revocation delivery; a successor's invitation after restart; the 64-handoff
bound; and exhausted revision counters. The focused final log is
`/tmp/loom-owner-focused-final.log`, with ten causal-history and nine ownership
tests passing.

Three WebKit cases exercise exact review confirmation, membership changes during
review, and a lost reply followed by refreshed ownership. They use a component
harness and do not establish packaged native acceptance.

The consolidated command `cargo test --locked -p loom-cabal -p tauri-plugin-loom`
passed 375 tests, with five existing ignored plugin cases. This includes the
large archived-history cases, local QUIC, compute retention and native projection
store tests. The log is `/tmp/loom-ownership-consolidated.log`. Strict Clippy for
both packages and all targets passed; its log is `/tmp/loom-ownership-clippy.log`.
The frontend passed 491 unit tests, the three WebKit cases, Svelte checking with
zero errors or warnings, and the production build. That log is
`/tmp/loom-ownership-frontend.log`. Formatting, current-document validation and
`git diff --check` passed. Generated native command permissions include the new
handoff command.
The native consumer check `cargo check --locked -p loom-app` also passed,
including regeneration of the app's permission manifests and schemas.

## Remaining acceptance

The Mac was locked when native archive acceptance was attempted. No interaction
with those prepared bundles was verified. Native acceptance of the current
store/protocol and ownership controls remains open. Earlier native receipts
describe their recorded revisions, not this revision.

Losing the sole owner's key before handoff remains unresolved; this implementation
does not invent authority from a stale backup or self-signed replacement.
Phone-linked Signal, physically separate Internet peers, physical concurrent
typing/IME and final release acceptance also remain open. No Signal account was
linked, group edited, or message sent during this work.

## CI follow-up and native preparation

Run `35062073078` caught an omitted reviewed build-script hash after the handoff
command was registered. The failing guard also reproduced locally in
`/tmp/loom-owner-policy-before.log`. Review of the change confirmed that the only
build-script addition is `cabal_transfer_owner` in Tauri's command list; it adds
no test-harness flags or conditional compilation. The reviewed hash in
`ci/ignored-tests.json` now binds that exact script. The source guard and ignored
test inventory remain intact. The full policy command passed all 129 Node tests,
all four current-document tests, document validation and `xtask policy`; its log
is `/tmp/loom-owner-policy-after.log`.

The debug native build at `30f2b418f0f24d07f7b81a1ed4930c933e5e4ac1` completed;
its log is `/tmp/loom-owner-30f2b41-build.log`. Two independent bundle identifiers,
`app.delysis.loom.handoff30f2b41.owner` and
`app.delysis.loom.handoff30f2b41.peer`, use fresh isolated profiles. Both ad-hoc
signatures passed deep, strict verification. Executable and frontend hashes and
exact paths are recorded in `/tmp/loom-native-handoff-preparation.json`. Neither
bundle was launched: native discovery again reported that the Mac was locked.
PID and accessibility evidence are therefore absent, and this is build
preparation only.

Review of upstream's `scripts/product-state-backup.mjs` confirmed that it restores
the historical file tree exactly. That rollback behavior is not a device-recovery
protocol: the snapshot may precede an ownership handoff, invitation consumption,
or spent compute budget. It also does not transfer Signal's OS-vault credential
or external workspace folders. No profile or credential was restored during
this review. The missing recovery authority remains an explicit design boundary.
