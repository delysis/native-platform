# W1 contract suite

This is the temporary, packet-owned executable specification accepted by
`W1-CONTRACTS`. It is not a production runtime kit and grants no authority.

`platform-contracts-v0` contains only versioned serialized data envelopes.
`platform-contract-testkit` contains test-only adapters and shared semantic
model tests. Current repositories consume an exact content hash only through
test code. During W2 these bytes move into `delysis/native-platform`; the
temporary copies are then retired.

Lifecycle conformance also has an ownership-specific compositional surface.
Its traits are grouped by proof obligation, not by product or runtime. Every
adapter binds a compile-time product and implementation marker. Suites mint
typed private coverage evidence only after their assertions return, and a
manifest accepts only same-marker evidence whose union includes every ADR-003
invariant and each required suite exactly once. This prevents accidental
cross-implementation mixing; it cannot make a deliberately dishonest wrapper
truthful. Source review must still prove that every adapter method and shutdown
worker fact comes from the named production owner rather than shadow state.
Cross-boundary obligations such as progress-to-shutdown and
panic-to-shutdown remain bridge suites; unrelated component-local results
cannot be combined into those claims. These are test traits only, never a new
production runtime abstraction.

The durable Wave 1 snapshot is temporarily hosted as
`delysis/w1-platform-contracts`. Current repositories must consume one exact
40-character commit revision for test code only; branches and tags are not
dependency identities. The repository publishes no crates. After W2 imports
this Git history and replaces every temporary pin with monorepo paths, the
temporary repository is archived rather than deleted.

The suite deliberately contains no Tauri, product, backend, network,
filesystem, credential, clock, randomness, or live-authority implementation.

The v0 privacy-policy envelope is declarative only. It records a local-only or
explicit hosted-routing boundary, opaque provider IDs, hosted data tiers,
payload-redaction requirements, and a safe logging mode. Its pure decision
method denies invalid policies, unknown observations, and routes not expressly
allowed. It carries no credentials, provider endpoints, clients, or authority.

Every golden fixture is validated against its JSON Schema using only an
in-memory schema registry. `fixtures/v0/MANIFEST.sha256` authenticates the exact
checked-in bytes; typed round trips assert semantic JSON shape. The suite does
not claim a canonical JSON byte format.

`platform-vertical-fixtures-v0` is an additive, test-only protocol for freezing
the eighteen Wave 1 vertical rows after contract acceptance. It validates only
caller-supplied bytes and observations, never executes replay recipes, and
cannot access files, processes, networks, models, credentials, or platform
state. Lock validation authenticates all supplied manifest bytes; candidate
comparison authenticates and binds caller-supplied production-tree bytes. See
[VERTICAL-FIXTURE-PROTOCOL.md](VERTICAL-FIXTURE-PROTOCOL.md). This
protocol revision contains no product fixture manifests and does not accept
`W1-VERTICALS`.
