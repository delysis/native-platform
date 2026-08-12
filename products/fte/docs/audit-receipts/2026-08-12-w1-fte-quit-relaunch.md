# FTE Wave 1 product-owner quit/relaunch

This row exercises `quit_relaunch_fake_owners` without a provider key, hosted
request, model file, wall-clock schedule, or OS Keychain interaction. The only
generated component is a deterministic local backend. Both lifecycle owners
are the production `GatewayRuntimeOwner`; routing, admission, cancellation,
terminalization, shutdown coordination, native-host ownership, and durable
storage are production code.

The production baseline is commit
`797500060047ccd10f9810fb4d5c8f374e00eb08`. It exposes an owner-level shutdown
receipt only after Gateway closure and native-host join. The receipt carries
the exact expected and joined backend worker IDs plus the retained task count.
The projection freezes both the canonical joined-worker sets and the unsorted
completion-order sequences returned by the production `JoinSet` receipt.
The checked-in source descriptor authenticates the pre-test production prefixes
of the router, desktop owner, and product database, plus exact Git blobs for the
desktop assembly and credential store.

The input corpus SHA-256 is
`2692b9ddb4e2645e3d13b644e859463f7f46d770eb3d4ca1e4ea51b999e54014`;
the expected projection SHA-256 is
`c55756ac4aaa63134ccf7e7f363d4de81d40677e54861d7ebbf3e4f7638560dc`;
and the manifest SHA-256 is
`aa38fe3f5e9fb305751181ef3f0dfbc84d0043303629a8d239609a1adc7b529d`.

## Replay

The first product owner binds a real FTE SQLite database, stores a non-secret
profile marker through `Database::save_profile_field`, registers the
deterministic ready backend through `Gateway::register_backend`, and accepts one
request through `Gateway::execute`. While that request remains active, the
owner shutdown begins. The fixture observes the production cancellation call,
proves that a second request is rejected with `gateway_quiescing`, then releases
the backend's authoritative cancelled result.

The fixture proves shutdown remains unfinished while the authoritative backend
final is withheld. After release, the receipt proves all nine registered
backend worker IDs were joined, zero tasks were retained, the Gateway is closed
with zero active operations, and the application-owned native host joined. The
original owner is dropped. A distinct production owner with a separately frozen
runtime ID then reopens the same product database, reads the exact marker,
registers a fresh deterministic backend instance, accepts and completes new
work, and returns its own exact 9/9 zero-retention shutdown receipt.

The counting credential store asserts zero reads, writes, and deletes. The
fixture executes an exact local route and never starts a listener or hosted
transport. Cerebras is present only as one inert catalog backend whose shutdown
worker ID must join; no Cerebras credential or live call is required.

## Evidence boundary

This is reproducible model-free lifecycle evidence. It does not claim live
Tauri window event dispatch, real model inference, live hosted-provider
behavior, or independent OS process enumeration beyond product-owned join
receipts. Those omissions do not weaken the row's contract: quit admission,
cancellation, authoritative terminal release, exact worker joins, zero retained
tasks, durable reopen, distinct owner construction, and successful fresh work
are all directly observed.

```sh
cargo test --locked -p free-token-energy \
  --features unstable-w1-vertical-tests --lib \
  gateway_runtime::w1_quit_relaunch_tests::w1_product_owner_quit_relaunch_is_quiescent_and_fresh \
  -- --exact
```
