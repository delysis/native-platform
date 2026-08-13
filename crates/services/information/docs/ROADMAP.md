# Capability ledger

This file distinguishes implemented proof from architectural reach.

## Initial release target

- versioned resource/release/representation, rights, use-policy, install,
  query, evidence, and locator contracts;
- strict JSON catalogue validation and deterministic install planning;
- Kiwix OPDS discovery, exact Metalink 4 resolution, and Overture STAC release
  discovery;
- policy-bounded HTTP/file acquisition, durable HTTP resume, and exact size and
  SHA-256 verification with source attestations;
- managed staging, same-filesystem activation, receipts, and external read-only mounts;
- federated bounded lexical retrieval with reciprocal-rank fusion;
- strict Alexandria, Community Archive v28, encyclopedia-article, and Scripture
  citation SQLite backends with stable profile-specific locators;
- operator CLI and optional permissioned Tauri plugin;
- real zero-write smoke against Christian and MPC Alexandria, Community Archive
  v28, the encyclopedia archive, and the Scripture citation index.

## Next adapters, in order

1. Supervised `kiwix-serve` integration on top of the shipped bounded OPDS and
   Metalink resolution. Linking GPL C++ `libzim` through an unsafe FFI boundary
   is not accepted as hidden “native Rust”; a mature audited pure-Rust reader
   can replace the sidecar.
2. Overture STAC traversal plus bounding-box/theme GeoParquet materialization,
   then DataFusion-style predicate pushdown and GERS locators.
3. Raw OSM PBF regional installs and replication receipts; PMTiles is a
   rendering backend, not the semantic data source.
4. OPDS 2, Data Package, RO-Crate, Croissant, BagIt, and IIIF import adapters.
5. Wikimedia/Wikidata snapshot adapters, then commit-pinned Hugging Face data
   and selective Common Crawl WARC records.
6. Durable background install jobs with progress subscriptions and cooperative
   cancellation, plus a separately confirmed partial-install abandon command.

No item in the second list should be reported as shipped merely because the
manifest format can describe it.

Concretely, the current release has no Kiwix ZIM content reader, no OSM PBF
query backend, and no Overture GeoParquet materializer or query backend. OPDS
and STAC discovery results are catalogue evidence only until those adapters are
implemented and verified.
