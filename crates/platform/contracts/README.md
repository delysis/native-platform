# W1 contract suite

This is the temporary, packet-owned executable specification accepted by
`W1-CONTRACTS`. It is not a production runtime kit and grants no authority.

`platform-contracts-v0` contains only versioned serialized data envelopes.
`platform-contract-testkit` contains test-only adapters and shared semantic
model tests. Current repositories consume an exact content hash only through
test code. During W2 these bytes move into `delysis/native-platform`; the
temporary copies are then retired.

The durable Wave 1 snapshot is temporarily hosted as
`delysis/w1-platform-contracts`. Current repositories must consume one exact
40-character commit revision for test code only; branches and tags are not
dependency identities. The repository publishes no crates. After W2 imports
this Git history and replaces every temporary pin with monorepo paths, the
temporary repository is archived rather than deleted.

The suite deliberately contains no Tauri, product, backend, network,
filesystem, credential, clock, randomness, or live-authority implementation.

Every golden fixture is validated against its JSON Schema using only an
in-memory schema registry. `fixtures/v0/MANIFEST.sha256` authenticates the exact
checked-in bytes; typed round trips assert semantic JSON shape. The suite does
not claim a canonical JSON byte format.
