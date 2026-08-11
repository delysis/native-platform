# Architecture

## Authority layers

```text
catalogue bytes -> normalized release -> install plan -> staged blobs -> receipt
                                                   |             |
                                                   v             v
external read-only library --------------------> installed representation
                                                               |
query/tool call -> capability routing -> backend -> evidence + stable locators
```

The contract, catalogue, and retrieval crates are pure policy layers.
Filesystem mutation begins in `information-native-store`; network authority is
isolated in `information-native-acquire`; SQLite authority is isolated in
the four compiled `information-native-backend-*` crates; Tauri is a leaf.

A catalogue's declared trust is data, not authority. Parsing JSON always yields
an unverified `CatalogAuthority`; only a pinned digest or an explicit local
approval constructor can elevate it. Plans freeze that independent authority
along with the complete selected resource, release, representation, rights,
policy, provenance, and artifact metadata.

A plan imported without its live catalogue is a different authority boundary.
The CLI's `install --plan` path validates the self-contained plan, preserves its
incoming fingerprint as `source_plan_sha256`, replaces its effective authority
with `DetachedUnverified`, and recomputes the plan fingerprint before any
bytes move. A detached file cannot replay a built-in pin or local approval that
the executing host did not independently establish. Embedded applications that
hold a live `CatalogIndex` can instead install the exact plan against that
catalogue authority.

## Resource model

A `ResourceDescriptor` is the human and legal identity of a corpus. A
`ResourceRelease` pins a dated/versioned publication. A release contains one or
more `RepresentationDescriptor` values, each with an exact format, capabilities,
artifacts, size, digest, and optional subset dimensions.

This separation matters. English Wikipedia is one resource with many releases
and representations. A full-text ZIM and a title-only ZIM are not interchangeable.
An Overture release is global, while a bounding-box/theme selection would be a
derived representation. The contract models that selection, but this release
rejects it until a materializer can produce deterministic artifacts and record
their derivation.

## Installation state machine

```text
planned -> fetching -> verifying -> activating -> ready
   |          |            |             |
   +----------+------------+-------------+-> failed
```

Plans are immutable and fingerprinted. Fetching charges a single byte budget.
Verification checks declared length and SHA-256. HTTP resume is allowed only
when a durable sidecar binds the partial file to the original URI, expected
digest, length, server validator, and source attestation. Every completed
artifact records its requested/final URI, redirect chain, connected peer,
timing, resumed bytes, length, and digest. It also records an ordered history of
the half-open byte range and source attestation for every source contact,
including an interrupted contact or one superseded by a restart. Activation is
a same-filesystem rename inside the managed root.

Verified-byte publication has an explicit commit receipt. A fresh artifact is
file-synced before its no-clobber publication and the parent directory is
synced afterward. If that final sync fails, the acquire error carries a
`PublicationReceipt` and reports `PublishedDurabilityUnknown`; it is never
collapsed into an ordinary I/O failure. Resume-sidecar creation and replacement
use the same rule, and a durable artifact is committed only when its complete
staging bytes are synced before sidecar removal. An exact retry reopens without
following aliases, checks the complete length and SHA-256, syncs again, and
returns a receipt marked `idempotent_recovery`; a conflicting destination is
left untouched and fails closed. Recovery records `PreexistingStage` rather
than inventing a source contact that did not occur during the retry.

The resumability boundary is explicit. A completed staged artifact is rehashed
and reused after interruption, and its acquisition record is persisted in the
staging package as an immutable per-artifact journal entry; `stage.json` is
write-once. Partial byte-range continuation is supported only for HTTP(S), only
with a valid durable sidecar and server validator, and only on platforms where
the acquire crate implements strong file identity. On Unix, durable state is
accepted only when staging and sidecar share the same canonical, owner-private
`0700` directory, held under a nonblocking exclusive lease; existing state is
opened without following symlinks. The host restarts the artifact from zero
elsewhere; a partial `file:` artifact always restarts.
After all staged bytes verify, activation uses a same-filesystem rename and
syncs both affected parent directories on Unix. A
crash after that rename but before the ready receipt is recovered as an
`activated_unregistered` install on the next exact-plan attempt. A failed or
cancelled transfer that has entered private staging never produces a ready
registration and appends an inspectable terminal receipt. Errors before a
private staging directory exists—invalid plans, lease contention, recovery
failure, or insufficient disk—are synchronous preflight errors and have no
durable attempt receipt. “Crash-resumable” therefore does not promise
transport-level continuation or a receipt for every preflight failure point.

Ordinary listing checks structure and file sizes. `verify_full` is the explicit
whole-byte rehash operation for large archives and should be run before first
use or when package identity is in doubt.

External imports never enter this state machine: they are registrations of a
caller-granted path with an observed identity and an explicit live-read-only or
immutable-read-only policy.

Managed installation is currently a synchronous host operation. The acquire
layer accepts a cooperative progress callback, but a durable job identifier,
progress subscription, and cancel command are not yet part of the Tauri or CLI
surface. Partial staging state can be inspected; abandoning it is intentionally
left to a product-owned, separately confirmed destructive workflow.

## Retrieval

Backends advertise exact capabilities: lexical search, record lookup, spatial
filtering, temporal filtering, graph traversal, media lookup, or random access.
The router rejects requests that no selected representation can execute.

Federation is bounded per backend and globally. Scores are not assumed to be
comparable; each backend rank is converted with reciprocal-rank fusion, then
provenance-complete identifiers deduplicate hits without erasing stricter rights
or source identity. Rank order is not silently rearranged for diversity. Every
result carries:

- resource/release/representation identity;
- backend and source rank/score semantics;
- document and passage identifiers;
- title, creator, snippet, and bounded context;
- a typed locator (block, page, article path, timestamp, bounding box, or
  record key);
- source URI, rights/attribution, and provenance notes.

Prompt construction is intentionally downstream. The host exposes a JSON tool
schema and typed result, but never concatenates evidence with instructions.
Model-facing calls are forced to `model_context`, require exact
resource/release/representation targets, and fail closed when policy forbids
that purpose. Timeouts are cooperative soft deadlines: the router discards late
backend responses, while SQLite backends also install progress-handler
deadlines so long queries are interrupted inside the database engine.

## SQLite boundary

SQLite adapters inspect schema before querying and accept only four compiled
profiles:

- `alexandria.blocks.v1`: Alexandria blocks and FTS;
- `community-archive.messages.v28`: Community Archive v28 messages and FTS;
- `encyclopedia.articles.v1`: encyclopedia articles and FTS;
- `alexandria.scripture-references.v1`: normalized Scripture passages and
  citation occurrences.

SQL identifiers are compiled into the profile implementation, never supplied
as unchecked catalogue strings. `trusted_schema=OFF` and read-only open flags
are mandatory. Every operation uses a `mode=ro&immutable=1` snapshot so SQLite
cannot create WAL/SHM files beside a canonical source. Both access modes reject
a non-empty sibling WAL or rollback journal and compare main/sidecar identity
before and after retrieval. Live mode rebinds identity on each operation;
immutable mode additionally fixes main-file identity and SHA-256. Because the
immutable transport takes no ordinary SQLite read lock, live mode is an
operator promise that the source is quiescent for the operation, not a safe way
to race a writer. Static verified identity or `--verified-sha256` claims are
rejected in live mode.

Writable sidecars, if introduced by a backend, must be placed under the managed
root and link to the canonical source by fingerprint. They may never share the
canonical database path.

On Unix, the store enforces owner-only directory and file modes for managed
state. The portable path/symlink/identity checks still run on Windows, but this
crate does not claim to audit or rewrite Windows DACLs; a Windows application
must place the store in an app-private directory whose ACL it owns. Windows
directory sync is currently a no-op, so rename recovery, registry publication,
and journal publication there are best-effort across sudden power loss; the
kit does not claim a power-loss-durable commit boundary on Windows.
