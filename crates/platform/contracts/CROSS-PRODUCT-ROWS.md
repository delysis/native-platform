# Cross-product Wave 1 rows

This repository aggregates caller-owned observations; it does not execute a
product, read another repository, or turn supporting lifecycle tests into a
vertical baseline. `validate_row_baselines` requires every manifest case once
and delegates each case to the existing exact baseline validator. It preserves
each product's source, prerequisites, projection, and omissions independently.

## Corrupted disposable caches

The canonical row 17 bundle is:

- `verticals/v0/corrupted-disposable-caches.manifest.json`
- `verticals/v0/corrupted-disposable-caches.mom.projection.json`

It contains one Mom-owned case from accepted adapter commit
`764b4cc1790709a23ecae74e746ca6b4ecb7321f`. The product manifest at that
commit is SHA-256
`83dccd2453e26fcaf175fbbf4fa5fdfe7c0b1d14020f7cbc350dcfffea580b61`.
Its exact replay covers Mom's two persistent disposable formats: the encrypted
native-prefix cache and encrypted session-KV cache. Both before and after
logical states are typed, frozen, and authenticated; the production
`ConversationDb` remains unchanged through corruption and reopen.

The row deliberately has no invented case for:

- FTE provider-owned remote caches;
- Information acquisition or publication staging, which is authoritative
  publication state rather than a disposable cache;
- Loom, which explicitly owns no disposable cache in its accepted adapter;
- Native, which owns cache value types but no persistent cache store;
- Speech's externally owned Hugging Face model cache.

Those boundaries are omissions, not passing product claims. A later product
may add a case only after it owns a persistent disposable format and supplies a
production-derived observation.

## Quit and relaunch using fake owners

Row 8 remains unmaterialized. No manifest is checked in because a structurally
valid manifest without executable product observations would look complete
while proving nothing.

The missing work is exact and product-owned:

- Mom must project its fake-owner quit/relaunch through the full `AppRuntime`,
  including same-durable-state reopen; its current supporting test uses a raw
  `OperationSupervisor` and explicitly disclaims that acceptance.
- FTE must expose a deterministic Gateway owner observation with quit order,
  terminal publication, retained join, zero orphans, and fresh-state relaunch.
- Native must expose the corresponding model-owner observation without a real
  model prerequisite.
- Loom must expose its deterministic generation-owner observation without
  relying on the real Gemma desktop receipt.
- Speech must expose its deterministic host/backend owner observation,
  including the accepted detached shutdown coordinator and retained worker
  identities.

Information has no resident worker owner for this row and must not be forced
into the case set. Once the five observation adapters exist, one canonical row
8 manifest can enumerate them and `validate_row_baselines` can reject any
missing, extra, duplicate, or weakened case.
