# FTE Wave 1 product-owner quit/relaunch

This row exercises `quit_relaunch_fake_owners` without a provider key, hosted
request, model file, wall-clock schedule, or OS Keychain interaction. The only
generated component is a deterministic local backend. Both lifecycle owners
are the production `GatewayRuntimeOwner`; routing, admission, cancellation,
terminalization, shutdown coordination, native-host ownership, and durable
storage are production code.

The production baseline is commit
`2db2d4568b277f6829b3b8e3623fce59435847c2`. It exposes an owner-level shutdown
receipt only after Gateway closure and native-host join. The receipt carries
the exact expected and joined backend worker IDs plus the retained task count.
Joined IDs are canonicalized so scheduling order cannot make evidence flaky.
The checked-in source descriptor authenticates the pre-test production prefixes
of the router and desktop owner, plus exact Git blobs for the product database,
desktop assembly, and credential store.

The input corpus SHA-256 is
`8a2308edc6fe670b60a28329b4dc621e3e094fdbc75bb77fbba92e7821dc13ec`;
the expected projection SHA-256 is
`15c79a6a156efa06e2ed6e9adcf77d0eb7e0840bd30326e07a0fc23d1061fcd7`;
and the manifest SHA-256 is
`172597b6ba3127b885dc3410cf03abdb093dc66fa94d499bf8ac4ee9cf9f2dd2`.

## Replay

The first product owner binds a real FTE SQLite database, stores a non-secret
profile marker through `Database::save_profile_field`, registers the
deterministic ready backend through `Gateway::register_backend`, and accepts one
request through `Gateway::execute`. While that request remains active, the
owner shutdown begins. The fixture observes the production cancellation call,
proves that a second request is rejected with `gateway_quiescing`, then releases
the backend's authoritative cancelled result.

The shutdown receipt is awaited only after that terminal is observed. It proves
all nine registered backend worker IDs were joined, zero tasks were retained,
the Gateway is closed with zero active operations, and the application-owned
native host joined. The original owner is dropped. A distinct production owner
then reopens the same product database, reads the exact marker, registers a
fresh deterministic backend instance, accepts and completes new work, and
returns its own zero-retention shutdown receipt.

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
