# Compute retention beyond the active working set

This change follows `69ab799fe110deda434edd4d2419cec0032c4c0f` on
`codex/loom-cabals-signal`. Main at
`fb3a1b2d5d64b85d148d064cda908912b689347c` remains incorporated. The previous
native lifecycle receipt covers ledger version 2; it does not certify this
version-3 storage layout in a packaged application.

## Behavior

Finished jobs no longer occupy the 256-job/64-MiB active working set. Exact input
and signed receipt rows remain where SQLite originally committed them. Appending
a terminal receipt and updating the settlement/byte-count index share one
transaction. Reading a job verifies its exact input, receipt chain and index;
a missing receipt cannot become permission to submit the same identity again.

Retired grants preserve their original bodies and lifetime job counts. Revoked
identities, including uncertain revocations before admission, remain durable.
Only revoked grants or exhausted grants with no unfinished job leave the
64-grant active set. Repeating an exact retired grant cannot replenish or
reactivate it.

Both ledgers reserve result space before admission, retain at most 4 GiB of
payloads, and allow another 128 MiB for SQLite metadata/indexes. Capacity refusal
preserves existing history. Startup verifies only the bounded unfinished set;
settled history is checked on retrieval. UI history enumerates the newest 256
identities and reads one job at a time to build small previews, without retaining
a page of image/audio inputs in memory. Existing terminal run receipts can still
retrieve an older job by its original identity. There is no automatic pruning.

The wire protocol remains version 3. Both local ledger schemas advance from 2
to 3. Unsupported stores are preserved and rejected, without migration. Existing
user-edited acceptance profiles were left untouched.

## Checks

- Host storage retained 260 settled jobs and grant identities across reopen.
  Every original receipt hash matched, spent budgets stayed at zero, and exact
  grant retries did not restore retired grants.
- Requester storage retained 260 cancelled jobs across reopen. The history page
  stayed at 256 IDs; older exact retries recovered their original input,
  cancellation intent and terminal result without preparing new work.
- More than 64 uncertain revocations retained their tombstones while releasing
  active grant capacity. Sixty-four still-active grants continued to refuse a
  new grant. A spent grant with unfinished work retained its slot until settlement.
- Six maximum-media inputs with worst-case escaped output exceeded the old
  64-MiB lifetime payload limit while releasing active bytes. Actual encoded
  payload totals matched the retained-byte index and stayed within reservation.
- SQLite triggers injected failure between receipt insertion and index update
  on both sides. Rollback preserved the prior signed state and exact input.
  Corrupting saved input or removing settled receipts failed retrieval and exact
  preparation, instead of admitting a replacement job.
- Existing actual local-QUIC tests passed for authenticated retries, lost replies,
  host/requester restart, cancellation, revocation, media, deadlines and joined
  shutdown. Their executor is an explicitly labelled fixture, not a base model.

The consolidated command `cargo test --locked -p loom-cabal -p tauri-plugin-loom`
passed 347 tests with five existing plugin tests ignored. The separately added
`spent_grant_is_not_retired_while_its_job_is_unfinished` case passed as well.
Strict Clippy for both crates and all targets, formatting, current-documentation
validation and diff checks passed. Logs are
`/tmp/loom-compute-retention-final-gate.log` and
`/tmp/loom-retention-grant-settlement.log`.

This is storage and integration evidence. Native acceptance of the new format,
collaborative-document archival, owner-device recovery/transfer, phone-linked
Signal and physical Internet peers remain separate unfinished work.
