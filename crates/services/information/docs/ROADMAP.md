# Capability ledger

This file distinguishes implemented proof from architectural reach.

## Initial release target

- versioned resource/release/representation, rights, use-policy, install,
  query, evidence, and locator contracts;
- strict JSON catalogue validation and deterministic install planning;
- Kiwix OPDS discovery, exact Metalink 4 resolution, and Overture STAC release
  discovery;
- bounded native OpenZIM v6 reading for exact local archive size/SHA-256,
  directory identity validation, uncompressed/Zstandard cluster decoding,
  inert HTML/text extraction, and typed `managed.documents.v1` production;
- immutable Overture release/item STAC identity, exact local GeoParquet
  partition admission, explicit bbox/theme/type selection, and a bounded typed
  engine interface with predicate receipts and GERS provenance;
- policy-bounded HTTP/file acquisition, durable HTTP resume, and exact size and
  SHA-256 verification with source attestations;
- managed staging, same-filesystem activation, receipts, and external read-only mounts;
- strict `managed.documents.v1` materialization with immutable documents,
  ordered segments, FTS5, provenance/lineage, private fail-closed defaults, and
  exact managed-byte-only removal;
- federated bounded lexical retrieval with reciprocal-rank fusion;
- strict Alexandria, Community Archive v28, encyclopedia-article, and Scripture
  citation SQLite backends with stable profile-specific locators;
- operator CLI and optional permissioned Tauri plugin;
- real zero-write smoke against Christian and MPC Alexandria, Community Archive
  v28, the encyclopedia archive, and the Scripture citation index.

## Next adapters, in order

1. Exercise the shipped safe first-party OpenZIM core against exact acquired
   Kiwix archives, extend only with bounded audited pure-Rust codecs where real
   evidence requires them, and add a typed article retrieval/conversation-grant
   surface. A `kiwix-serve` sidecar or unsafe libzim FFI is not approved.
2. Bind a production safe-Rust GeoParquet engine to the shipped Overture typed
   boundary, accept exact real partitions, then compose only the typed spatial
   result in Mom. Do not add raw SQL or route Overture through a text library.
3. Raw OSM PBF regional installs and replication receipts; PMTiles is a
   rendering backend, not the semantic data source.
4. OPDS 2, Data Package, RO-Crate, Croissant, BagIt, and IIIF import adapters.
5. Wikimedia/Wikidata snapshot adapters, then commit-pinned Hugging Face data
   and selective Common Crawl WARC records.
6. Durable background install jobs with progress subscriptions and cooperative
   cancellation, plus a separately confirmed partial-install abandon command.

No item in the second list should be reported as shipped merely because the
manifest format can describe it.

The generic managed-document materializer is shipped infrastructure, not proof
of every source adapter. The Kiwix core now produces exact hash-bound inputs in
portable and hostile-fixture tests, but it has no real-corpus or launched-
product acceptance yet. It supports OpenZIM v6 with uncompressed and
Zstandard clusters; v5, historical LZMA/zip/bzip2, split archives, non-UTF-8
article decoding, active HTML, and dictionary/skippable Zstandard extensions
remain unsupported.

Concretely, the current release still has no OSM PBF query backend. Overture has
an exact-byte admission and query contract, but no bundled production
GeoParquet engine, acquisition command, real-partition runtime evidence, or Mom
surface. STAC discovery alone remains catalogue evidence; deterministic fixture
proof for the typed boundary is not product acceptance.
