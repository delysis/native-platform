# W1 Loom model-free and state vertical receipt — 2026-08-12

Production baseline: `loom-native@e32d0697f6b5e28716e34c6b051d47d5031d010c`.

Vertical protocol: `w1-platform-contracts@9fd803f5efcc46ac0256dab876e7c0b1f03bb448`
(`w1-vertical-protocol-v0-2026-08-12`). The accepted lifecycle contract remains
pinned independently at `cbab33555ab9355a6ac453d659c55ec9e0666821`.

The feature-gated `unstable-w1-vertical-tests` lane parses the canonical
manifests and projections through `platform-vertical-fixtures-v0`, authenticates
every checked-in byte artifact, constructs typed observation envelopes, and
calls `validate_baseline` for rows 6, 7, and 14. No later production candidate
exists in this two-commit freeze, so `compare_candidate` is intentionally not
used.

`fixtures/w1/source/loom-production-tree-e32d069.json` is the SHA-256-bound
production-tree byte artifact. It records the Git tree identity of every
application and crate `src` root at the production baseline. The fixture
descendant test proves the baseline is an ancestor and every current source-root
identity remains exact; the evidence commit changes no production source file.

## Row 6 — suggestion and promotion

The product replay starts from one exact 61-byte revision and three verified
candidate bodies. It proves one caret-local ghost, hidden additional candidates
behind explicit review, exact-boundary Tab promotion, ordinary Tab otherwise,
and no manuscript mutation on dismissal or stale presentation. It also proves
the absence of persistent candidate-count chrome, `Skip to manuscript`, and a
primary `Use this` control. The store replay promotes the primary candidate,
classifies all three terminal records as fixture evidence, and verifies the
revision, receipt, outbox, selection, generated provenance, family, and exact
reconstructed bytes after reopen.

Canonical manifest and projection:

- `fixtures/w1/manifests/loom-suggestion-promotion-v0.json`
- `fixtures/w1/projections/loom-suggestion-promotion-v1.json`

## Row 7 — diagnostic and admitted authority

The admitted store consumes one store-owned authority. An exact pre-request
copy has the same source bytes, output bytes, project identity, and durable
admission row but a distinct runtime session; it cannot mint assembly,
qualification, request, or promotion authority before or after reopen. Reusing
the admitted request fails, and reopening cannot recreate the spent capability.
This is model-free authority/lifecycle evidence and requires no provider or
credential.

Canonical manifest and projection:

- `fixtures/w1/manifests/loom-research-diagnostic-admitted-v0.json`
- `fixtures/w1/projections/loom-research-authority-v1.json`

## Row 14 — prior project store

The checked-in baseline is an actual 1,101,824-byte SQLite v10 database plus its
exact project manifest, content blob, and visible manuscript. Fixed ULIDs and
the ten authenticated migrations from accepted v10 source
`d0aca6ff4883ac51514fea5e5fb75ffbb3c8c264` reproduce the frozen database
byte-for-byte. The migration replay starts from those checked-in pre-open bytes;
it never initializes a current store or reverse-deletes v11 objects.

Current code applies migration 11. Project ID, active revision ID, exact visible
bytes, blob identity, durable counts, zero pending outbox entries, and zero
selection events are checked after the first open. Identity, visible bytes,
pending outbox, and zero selection events are checked again after the second
open. The state manifest binds `StateIdentityV0.before` directly to the frozen
SQLite bytes and binds the post-migration logical state to an exact checked-in
summary.

Canonical manifest and projection:

- `fixtures/w1/manifests/loom-prior-project-store-v0.json`
- `fixtures/w1/projections/loom-prior-project-store-v10-v1.json`

## Claim boundary

Rows 8 and 17 remain outside these three canonical Loom baselines. Existing
lifecycle tests remain supporting evidence only for the future cross-product
quit/relaunch row. No disposable Loom cache is invented. The three baselines
require no hosted provider, provider credential, real model, native UI, or
scheduled execution.

## Verification lane

All final local gates passed:

- `cargo fmt --all -- --check`
- `rustup run 1.88.0 cargo test -p loom-store --all-targets --features unstable-w1-vertical-tests` (152 unit tests and all integration targets)
- `rustup run 1.88.0 cargo test -p loom-store --features unstable-w1-vertical-tests --test w1_fixture_manifest` (3/3 canonical protocol tests)
- `rustup run 1.88.0 cargo clippy -p loom-store --all-targets --features unstable-w1-vertical-tests -- -D warnings`
- `rustup run 1.88.0 cargo test -p loom-host --features unstable-w1-contract-tests` (29/29)
- `rustup run 1.88.0 cargo test -p tauri-plugin-loom --features unstable-w1-contract-tests` (72/72)
- `pnpm --dir apps/loom test` (31 files, 181 tests)
- `pnpm --dir apps/loom check` (zero errors and zero warnings)
- `pnpm --dir apps/loom build` (passed; the existing chunk-size advisory remains non-fatal)
