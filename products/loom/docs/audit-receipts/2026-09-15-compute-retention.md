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

## Native acceptance of format 3

Two separately identified, ad-hoc-signed macOS apps built from clean
`476c6f8b9b9582a493ec2017dc24dc7b032050ff` exercised the new stores. Their bundle
identifiers were `app.delysis.loom.retention476c6f8.owner` and
`app.delysis.loom.retention476c6f8.peer`. Executable SHA-256 values were
`c61fb21bacaedca1070704fa27cf0cc2800a24f7ab3a396107021e3011617749` and
`51111af34ab6f6548470e0bbb5b01a0bd1785c296048c958bfbd7103d887872d`.
The receipt binds the frontend assets, fresh isolated profiles and process IDs.
Earlier acceptance profiles and their author edits were preserved.

Sage admitted Loom through the native invitation flow and granted three Gemma 4
jobs, each limited to 512 tokens and 120 seconds. The host loaded the real local
Gemma 4 12B model and projector; the requester had no model files.

| Job | Native interaction | Retained outcome |
| --- | --- | --- |
| `91db2f35-d356-4de4-e172-f83cd9f20fa3` | Submitted a framed moonlit-pond prompt | Completed; 15 generated tokens; the requester displayed the host's sentence |
| `63704168-81db-aa1a-7ee0-b65c743f940b` | Ctrl+C during a counting response, then Check | Signed cancellation recovered after initial uncertainty; native worker stopped after 71 tokens |
| `69eafb9c-d93e-cd0e-2067-cfb0cbb22b1d` | Quit host during another counting response | HostBusy after the native worker stopped at 78 tokens; requester displayed the plain-language shutdown message |

Both ledgers retained identical signed receipt chains, with settlement and exact
encoded byte totals agreeing: zero active jobs/bytes, 7,974 retained host bytes
and 9,489 retained requester bytes. Native inference evidence hashes were
`2682909b9c15d86321c1af2504db3bb1565bd13e337b8804cad1566633c065e7`,
`2dcc7c8d5bf217cc012d97b6573c605d302b37d4ed21fcd8dfdfb46dc6b25b2b` and
`73a4ed45746cf80f9821f138924b9167eb4de6506bd9dc7ed404a09b328eeabe`, respectively.

Restarting both apps and reopening their saved workspaces restored all three
terminal outcomes and automatic membership reconnection without another
invitation. Exact input rows, receipt hashes and job counts were unchanged;
the host showed zero of three jobs remaining. No new execution occurred.
Both manuscript copies kept SHA-256
`1c01e6b776ff0bf8c8a1c808762d9be4873c0fc8bea29f8911563949bcc0c762`.
The exhausted synthetic grant was revoked, both apps quit, and their final
processes (60950 and 61048) were absent.

Detailed local receipt: `/tmp/loom-native-compute-retention.json`, SHA-256
`9d66f2c596d70443c8ab39bcc396ca9ef152ba7c49b897deb8e2e70ce8882299`.
Required macOS CI and dependency checks passed at `476c6f8`; advisory platforms
were still running when inspected. This is actual same-Mac UI and inference
evidence, not hundreds of native jobs or physical Internet acceptance.

Collaborative-document archival, owner-device recovery/transfer, phone-linked
Signal, physical Internet peers and final release acceptance remain unfinished.
