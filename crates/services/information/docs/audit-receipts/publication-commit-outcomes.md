# Publication commit outcome receipt

## Identity

- Repository: `delysis/information-native-kit`
- Base: `ad6b9be24a2824f9478b0a4623ccb35dbaa3f25f`
- Committed predecessor: `7bf5b81c9eaeaa4cedb4e2ff06c39067093e0c64`
- Managed-store follow-up: `210d33da2aaf1405c93a8d4cf0197807e41f5c2d`
- Goal: `R3-INFO-COMMIT`
- Status: locally committed candidate; remote matrix evidence remains open

## Commit contract

`PublicationReceipt` records the planned artifact ID when one exists, exact
SHA-256 and length, destination path and strongest available safe identity,
visibility, file-sync completion, directory-sync completion, and whether the
result came from an idempotent recovery.

Ordinary acquisition errors remain not-published outcomes. Once a no-clobber
artifact publish, sidecar link, sidecar replacement rename, or final sidecar
removal has made the verified state visible, a later parent-sync failure is
reported as `AcquireError::PublishedDurabilityUnknown` with that receipt. The
visible path is not rolled back or presented as a generic failure.

An exact retry opens the existing private regular file without following an
alias, checks its complete length and digest, re-syncs it and its parent, and
returns success with `idempotent_recovery = true`. A different or unverifiable
existing file returns `StagingPathExists` and is never overwritten or removed.
The host journals recovered bytes as `PreexistingStage`, without fabricating a
network or file-source contact for the recovery attempt.

Managed-store package activation and final registry metadata carry the same
distinction. `StoreCommitReceipt` identifies package activation or the exact
registry event, records which rename-parent directories were synced, and marks
whether the result arose while recovering an earlier uncertain commit. A
post-rename sync failure is `StoreError::CommittedDurabilityUnknown`, not a
generic store I/O error. The host preserves a distinct retryable
`information_store_commit_durability_unknown` code.

An activation retry accepts only the exact installed plan and re-hashes every
artifact before re-syncing the package destination and staging source parents.
If a ready registry event already exists, the retry also re-syncs its directory
and returns that event without appending a duplicate revision. A changed
artifact, conflicting plan, or simultaneous staging and package path fails
closed.

## Deterministic evidence

The focused acquire and store suites include these named regressions:

```text
parent_sync_failure_reports_published_durability_unknown
exact_retry_recovers_published_artifact
conflicting_retry_fails
sidecar_post_rename_failure_is_unambiguous
activation_sync_failure_reports_visible_state_and_exact_retry
activation_source_sync_failure_records_destination_durability
ready_receipt_sync_failure_is_exactly_idempotent_and_conflict_safe
```

Local macOS verification at this candidate:

```text
cargo test -p information-native-acquire --lib
  PASS: 43 passed
cargo test --workspace --all-targets --locked
  PASS: 196 passed, 4 explicitly ignored real-library tests
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  PASS
cargo fmt --all -- --check
  PASS
./scripts/check-boundaries.sh
  PASS
git diff --check
  PASS
```

## Evidence boundary

The injected parent-sync failures establish the post-commit result shape and
visible bytes on this macOS filesystem. This run does not establish power-loss
survival. Unix receipts record successful real directory syncs. The existing
non-Unix parent-sync implementation remains a no-op, so non-Unix receipts do
not claim `directory_synced = true` and this change does not strengthen the
repository's documented Windows power-loss claim.

No Windows or Linux runner was executed locally. The unchanged portable CI
partition must pass on Linux, macOS, and Windows, and the separate Tauri job
must pass on Linux before a steward promotes this candidate. The follow-up is
committed locally but not published; its local gate results do not establish
remote CI.
