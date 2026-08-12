# Cross-product Wave 1 rows

This repository aggregates caller-owned observations; it does not execute a
product, read another repository, or turn supporting lifecycle tests into a
vertical baseline. `validate_row_baselines` requires every manifest case once
and delegates each case to the existing exact baseline validator. It preserves
each product's source, prerequisites, and projection independently, and
requires every observation to repeat the manifest's exact omitted-claim list.

## Corrupted disposable caches

The complete row 17 bundle is:

- `verticals/v0/corrupted-disposable-caches.manifest.json`
- `verticals/v0/corrupted-disposable-caches.mom.projection.json`
- `verticals/v0/corrupted-disposable-caches.information.projection.json`
- `verticals/v0/corrupted-disposable-caches.information-resume.projection.json`

It contains one Mom-owned case from accepted adapter commit
`764b4cc1790709a23ecae74e746ca6b4ecb7321f`. The product manifest at that
commit is SHA-256
`83dccd2453e26fcaf175fbbf4fa5fdfe7c0b1d14020f7cbc350dcfffea580b61`.
Its exact replay covers Mom's two persistent disposable formats: the encrypted
native-prefix cache and encrypted session-KV cache. Both before and after
logical states are typed, frozen, and authenticated; the production
`ConversationDb` remains unchanged through corruption and reopen.

The first Information case is copied exactly from accepted adapter merge
`68bcf2c554da810b9b3fa8cda279822e810f56c9`. Its product manifest is
SHA-256 `e47353752cfe262686abfd0a132750c2c5033297377529142c5d72761bfd4911`.
That manifest binds unchanged production source trees at
`750e27e5ad27b6040e7ab7b66f7a2acb910b613a`, and its exact replay proves
that `ManagedStore::prepare_install` removes a malformed disposable
`.journal-*.tmp` acquisition-journal publication temporary while preserving
the valid staging manifest and artifact target. The package remains partial
and never becomes ready.

The second Information case is copied exactly from accepted adapter merge
`7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe`. Its product manifest is
SHA-256 `2972dd6fb8eefc0e0b31d6d87ce44b3118ca6c1cf6439684b8fcdb57afe4e66d`.
That manifest binds the same unchanged production source trees and its
loopback-only replay exercises Information's distinct durable HTTP-resume
pair. A cancelled identity-bound partial artifact is retained, its sidecar is
made malformed, and the production `AcquireClient` discards that state and
restarts from byte zero without a `Range` request. Only the exact verified
artifact is published; the malformed sidecar is removed and caller-owned
authoritative state remains byte-identical.

Together, the accepted Mom case and two accepted Information cases cover every
product-owned persistent disposable format identified for row 17. Row 17 is
complete and present in the sealed eighteen-row candidate. The lock binds the
catalogue bytes but is not a steward receipt and does not authorize W2.

The staged slice deliberately has no invented case for:

- FTE provider-owned remote caches;
- Loom, which explicitly owns no disposable cache in its accepted adapter;
- Native, which owns cache value types but no persistent cache store;
- Speech's externally owned Hugging Face model cache.

These boundaries are omissions, not passing product claims; a later product
may add a case only after it owns a persistent disposable format and supplies
a production-derived observation. Durable resume remains unavailable on
non-Unix platforms, and the accepted replay makes no hosted-network or
credential claim.

## Quit and relaunch using fake owners

The staged row 8 bundle is:

- `verticals/v0/quit-relaunch-fake-owners.manifest.json`
- `verticals/v0/quit-relaunch-fake-owners.mom.projection.json`
- `verticals/v0/quit-relaunch-fake-owners.speech.projection.json`
- `verticals/v0/quit-relaunch-fake-owners.loom.projection.json`
- `verticals/v0/quit-relaunch-fake-owners.native.projection.json`
- `verticals/v0/quit-relaunch-fake-owners.fte.projection.json`

The canonical manifest is SHA-256
`4f3e0c13b9413a48a56a16c4cd08422288856c2dd03ee484f9186e089a6deb08`
and contains exactly five product-owned cases. It copies the cases and exact
projection identities from these reviewed product revisions:

- Mom merge `3cf5794`, product-manifest SHA-256
  `120b31a7134ba884d0fe0e425fcf0322a5602709258c07d08c14ee3621459480`,
  binding production baseline `b5a276c6152e9bf1d6d1f2b5cf9c199871c45778`;
- Speech merge `b836318`, product-manifest SHA-256
  `594db6a977db9bb96f6c0e8eaa12e7972bf459d8c1f245231b7e765b87e3f907`,
  binding production baseline `2c427e39ee07c944e0ef51d471729fb676e2f62a`;
- Loom merge `223110b`, product-manifest SHA-256
  `728e9fe6f3d8b598e3b627074cca14d0ba28e4e53555d2616642822df5d83159`,
  binding production baseline `5b0d81ebbf0f7561f81829a34ef84b50412c17b1`;
- Native merge `16168bd`, product-manifest SHA-256
  `c3ecc8fd3dd578d9c1ffc63f1c66e0169b23d5dd638ba2333dc9e955560996a5`,
  binding production baseline `897dd86a961707c66021d1eaabcfd19314cb05f7`;
- FTE merge `67814e76659688fef61f311db588d17eddee0a66`, containing the exact
  reviewed head `a86a86d5d33303ea635e3986814a2d5325f5b9a3` and binding production
  baseline `797500060047ccd10f9810fb4d5c8f374e00eb08`; its product-manifest
  SHA-256 is
  `aa38fe3f5e9fb305751181ef3f0dfbc84d0043303629a8d239609a1adc7b529d`.

The staged validator supplies every case exactly once to
`validate_row_baselines`. Each observation retains its own repository, source
identity, replay, state boundary, worker facts, and expected projection. The
aggregate does not merge projections or turn one product's evidence into
another product's claim. The manifest omission list is the deduplicated union
of all five product manifests, including every real-model, GUI/process,
platform, credential, hosted-provider, native-worker, and downstream-store
boundary they disclaim.

Information has no resident worker owner for this row and is intentionally not
given an invented case. Row 8 is present in the sealed candidate, but the lock
does not turn omitted claims into passing evidence. A separate steward receipt
is still required to accept W1 and authorize W2.
