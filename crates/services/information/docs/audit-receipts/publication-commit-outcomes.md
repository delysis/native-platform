# Publication commit outcome receipt

## Identity

- Repository: `delysis/information-native-kit`
- Base: `ad6b9be24a2824f9478b0a4623ccb35dbaa3f25f`
- Goal: `R3-INFO-COMMIT`
- Status: uncommitted implementation-worker candidate

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

## Deterministic evidence

The acquire unit suite includes these named regressions:

```text
parent_sync_failure_reports_published_durability_unknown
exact_retry_recovers_published_artifact
conflicting_retry_fails
sidecar_post_rename_failure_is_unambiguous
```

Local macOS verification at this candidate:

```text
cargo test -p information-native-acquire --lib
  PASS: 43 passed
cargo test --workspace --all-targets --locked
  PASS: 193 passed, 4 explicitly ignored real-library tests
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
must pass on Linux before a steward promotes this candidate.
