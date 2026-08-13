# information-native-kit

A product-neutral, local-first Rust subsystem for discovering, installing,
importing, querying, and citing offline information resources in native AI
applications.

The kit treats a 30 GB Alexandria FTS database, a Kiwix ZIM, a regional OSM
extract, an Overture GeoParquet selection, and a small hand-curated SQLite
library as different representations behind one lifecycle and evidence
contract. It does not pretend that all formats support the same operations.

Query support in this release is deliberately narrower: the compiled backends
cover Alexandria blocks, Community Archive v28 messages, encyclopedia
articles, and Scripture citation occurrences/passages in SQLite. Kiwix OPDS
and Overture STAC are discovery surfaces. ZIM reading and OSM/Overture
materialization or query backends remain future work.

## Workspace

- `information-native-types`: versioned resource, release, representation,
  install, query, evidence, citation, trust, and tool contracts.
- `information-native-catalog`: validation and normalization for built-in
  manifests and remote catalogue providers such as OPDS and STAC.
- `information-native-store`: private staging, same-filesystem activation, external
  read-only imports, full-byte verification, and append-only receipts.
- `information-native-acquire`: the sole network-authority crate; bounded
  streaming fetch, durable HTTP resume, redirect/DNS attestations, and
  digest/length verification.
- `information-native-retrieval`: backend traits, capability-aware routing,
  result fusion, budgets, and stable citation lookup.
- `information-native-backend-sqlite`: strict Alexandria block/FTS adapter.
- `information-native-backend-community`: privacy-preserving Community Archive
  v28 messages/FTS adapter.
- `information-native-backend-encyclopedia`: origin-aware Encarta, Britannica,
  and Wikipedia article adapter.
- `information-native-backend-scripture`: normalized Scripture-passage and
  citation-occurrence adapter.
- `information-native-host`: immutable composition root and agent-tool surface.
- `information-native-cli`: an operator oracle for catalogues, installs,
  imports, queries, and receipts.
- `tauri-plugin-information-native`: optional Tauri 2 IPC with narrow permission
  sets.

## Principles

- The catalogue describes releases; an install plan resolves exact bytes.
- The CLI treats a plan loaded from disk as detached input: it preserves the
  source plan fingerprint for audit, downgrades its catalogue authority to
  detached-unverified, and receipts the rewritten effective plan.
- The manifest can describe region/theme/column selection, but the current
  planner rejects non-full selections until a format-specific materializer can
  produce and receipt the exact derived bytes.
- Canonical local databases are referenced read-only, never migrated in place.
- All SQLite operations use zero-write immutable transport. Live-read-only
  rebinds identity per operation and is allowed only for a quiescent source
  with no pending WAL or rollback journal; immutable-read-only additionally
  pins full-file identity and SHA-256.
- Managed state, indexes, and receipts are physically separate from sources.
- Managed installation is crash-recoverable at verified staging and atomic
  activation boundaries. Verified acquisitions are published as immutable,
  per-artifact journal entries; the staging manifest is never rewritten. Each
  receipt retains the ordered, half-open byte ranges and source attestations
  from every transfer attempt, including attempts superseded by a restart.
  Byte-range continuation is narrower: it applies only to HTTP(S) artifacts
  with a valid durable resume sidecar and validator on platforms where safe
  file identity is implemented. On Unix the staging file and sidecar must share
  one owner-private directory held under an exclusive transfer lease. Other
  platforms restart an interrupted artifact from byte zero.
- Local `file:` acquisition and private-network access require explicit
  capabilities; public HTTP(S) still passes address, redirect, and peer checks.
- Search results are evidence envelopes with stable locators, not prompt prose.
- Apps explicitly select evidence and delimit it as untrusted model input.
- Unsupported operations are capability errors, never empty success.

On Unix the store syncs both parents around activation and syncs registry and
journal publication. Windows retains the portable structural and recovery
checks, but directory sync is currently a no-op: those publications are
best-effort across sudden power loss, and this release does not claim a
power-loss-durable Windows commit boundary.

The current host install call is synchronous and callback-driven internally;
it does not yet expose a durable background-job, progress-stream, or cancel IPC
contract. Partial installs are inspectable, but deletion remains a separate,
product-confirmed responsibility.

See [ARCHITECTURE.md](docs/ARCHITECTURE.md),
[RESOURCE_MANIFEST.md](docs/RESOURCE_MANIFEST.md), and
[ECOSYSTEM.md](docs/ECOSYSTEM.md). The [operator CLI guide](docs/CLI.md), current
local adapter survey, and honest
capability boundary are recorded in [LOCAL_RESOURCES.md](docs/LOCAL_RESOURCES.md)
and [ROADMAP.md](docs/ROADMAP.md).

The initial provider registry is [default-sources.json](catalogues/default-sources.json).
Every entry says whether support is shipped or registry-only, so catalogue
aspiration cannot be mistaken for an executable adapter. It is a strict v1
operator input rather than an implicitly trusted built-in: validate or inspect
it with `information-native sources validate --registry PATH` and
`information-native sources list --registry PATH`. `registry_only` means the
entry is a descriptive upstream lead; no provider adapter, acquisition path,
materializer, or query backend is implied.
Local source paths are deliberately operator-owned and stay outside version
control. A documentation-only, deliberately non-executable starting point is
provided in
[local-resources.documentation.example.json](configs/local-resources.documentation.example.json);
the CLI does not parse that file.
