# Mom W1 product-local vertical fixtures

Date: 2026-08-12

Accepted base: `097da612140c6479f9d40e7816f0500271464ca9`

Accepted vertical protocol: `fc24ffff08c52690390b4460f44617d5d9732563`
(`w1-vertical-protocol-v0-2026-08-12-r2`)

Feature gate: `unstable-w1-vertical-fixtures`

These fixtures are compiled out of the default product feature graph. They use
the production Mom state, attachment, chat-commit, cache, and operation-owner
paths, while refusing subprocess, network, hosted-provider, real inference, or
personal Keychain authority. The vertical protocol and its matching contract
types are optional dependencies at the accepted merge commit; the previously
accepted lifecycle contract/testkit pin remains exactly `cbab335...`.

## Frozen inputs and projections

| Fixture | SHA-256 | Purpose |
| --- | --- | --- |
| `fixtures/w1/chat-cancel-retry-v1.json` | `e4ef9e79f8289a0e050b916b606b227baf97de334181d4f368ec4fb4c04ba8c8` | Exact app-admitted operation, request, and attempt identities, initial draft, retry input, and assistant fixture text |
| `fixtures/w1/ordinary-notes.md` | `19b385128f4ee3d9e2a1a6f5adaabb73c46fea8463c8fb72f53b24966e75304c` | Ordinary user-authored Markdown entering the attachment-native host |
| `fixtures/w1/ordinary-notes-projection-v1.json` | `500b0af87cefc0c2f60f02754376030befc085d409e5aae5bc57025761c7a792` | Exact attachment ID, content-addressed root, manifest namespace, policy fingerprint, artifact processor provenance, bounded prompt text, and prompt SHA-256 |
| `fixtures/w1/prior-store-v1.json` | `7e44507f4ee444becf112ed1853e9cfb301618aadac19b1909a41cacf95c6ccf` | Redacted logical-store import baseline plus logical versions and deterministic fixture-only key |
| `fixtures/w1/cache-corruption-v1.json` | `b2c713869df2a75145af92b2f01993a77537e8d2bc08d5f92cc88871e87b2f8c` | Exact Mom native-prefix and session-KV cache namespaces, a typed authoritative conversation, and their distinct cold-fallback dispositions |
| `fixtures/w1/cache-native-prefix-state-v1.json` | `787f58b20bbfe28415acb4721b3293e5af42bdd056368b1cac14fe8aec3dd3b8` | Exact typed logical native-prefix cache state before ciphertext corruption |
| `fixtures/w1/cache-session-state-v1.json` | `4bfc026b7b842e4ef65d6b0993fa6eb13f0d33e0573296aba6b48c04da34425a` | Exact logical session-KV metadata and blob identity before ciphertext corruption |
| `fixtures/w1/cache-native-prefix-after-state-v1.json` | `38e0b9de817f645c4bec37c0d4a3e58baecccb040f5718dc069a72c7385a0bed` | Exact typed absent native-prefix cache state after quarantine and reopen |
| `fixtures/w1/cache-session-after-state-v1.json` | `1885429208d88d71764bef9f98d867f16e5c54e7672226734a137201978fab9b` | Exact invalidated session-KV metadata and absent blob state after reopen |
| `fixtures/w1/quit-relaunch-v1.json` | `156aa56cad1e34731ce61a5ec8e25c8db154afe38593707a207986f7b8c7a487` | Deterministic application-operation identity and durable draft for the full runtime quit/relaunch case |
| `fixtures/w1/chat-cancel-retry-manifest-v0.json` | `d5e78f687cb8d06c823e91cdaa46c7564e042e7f118194dcd2c5a1042d24375d` | Authenticated central-protocol case for Mom chat/cancel/retry |
| `fixtures/w1/chat-cancel-retry-projection-v0.json` | `5089d654ffc27ce2b2ce34d102a98cd0160a074cb2f9c458dd7fbdacac2bb441` | Frozen product-derived stream, lifecycle, durable-state, and ownership facts |
| `fixtures/w1/attachment-manifest-v0.json` | `01c8701dc70b586bf3cbe9ad069b4fb97f2fe2719ce8a1b43e7b66d179c9cdb0` | Authenticated central-protocol case for Mom attachment |
| `fixtures/w1/attachment-projection-v0.json` | `fc10bb8b589c1290e546a7ad0b936b47954d541070154843b1c0beb193d4cb7f` | Frozen import/send/reopen projection |
| `fixtures/w1/prior-store-manifest-v0.json` | `8259231ba399eabb0f087be4f80f45a1b5986da98b0c1558387b86545e764e86` | Explicit redacted logical import/recovery case with historical DB evidence marked unavailable |
| `fixtures/w1/prior-store-projection-v0.json` | `2c7bd3fcd1e93ee5bc70ab8c3cf0be6a7f33b71bf03dbff9cd971306bf01b136` | Frozen logical recovery and plaintext-cleanup facts |
| `fixtures/w1/cache-corruption-manifest-v0.json` | `83dccd2453e26fcaf175fbbf4fa5fdfe7c0b1d14020f7cbc350dcfffea580b61` | Mom-owned cases within the cross-product corrupted-cache row |
| `fixtures/w1/cache-corruption-projection-v0.json` | `d2e03c9923dd36837c1b13b9a47b2426bf3730652338362417d60a7da420cbe7` | Frozen native-prefix quarantine and session-KV invalidation projection bound to their exact logical before and after states |
| `fixtures/w1/quit-relaunch-manifest-v0.json` | `120b31a7134ba884d0fe0e425fcf0322a5602709258c07d08c14ee3621459480` | Authenticated Mom case within the cross-product fake-owner quit/relaunch row |
| `fixtures/w1/quit-relaunch-projection-v0.json` | `4f38c918a9f89768d0a5f5dd97647e9ce045d709f0b0b074e6af228c1b55077a` | Frozen full-AppRuntime admission-close, cancellation, terminal, exact worker epochs, join, zero-orphan, same-store reopen, and fresh-admission facts |

## Product evidence

- `AppRuntimeHandle` admits the chat send, whose production
  `ChatStreamLifecycle` emits its own `started` event. The
  separately admitted production `chat_cancel` command reaches the request
  through `cancel_native_request`; only that native cancellation boundary
  releases the fixture wait. The same lifecycle then emits `cancelled`, commits no
  messages, preserves the encrypted draft exactly, and leaves no active chat
  request. The ordinary production command result is classified as cancelled by
  the operation supervisor only when its closed `chat_cancelled` blocker is also
  present in the receipt; unrelated blocked results remain completed transports.
- Retry uses the new request `mom-w1-request-retry` and the separately owned
  app-admitted attempt `mom_llama_chat_send:3#attempt-42`. The earlier
  `mom_llama_chat_send:1#attempt-41` remains an immutable retained terminal fact
  of `cancelled`; the retry is retained as `completed`. Reopen recovers the
  exact two committed messages and receipt ID.
- Both admissions retain distinct production `operation_id` and `attempt_id`
  values. The conversation ID is the explicit common correlation ID; it is not
  substituted for either admission identity.
- The Markdown file is read through `ProvidedAttachment::read_bounded`, inspected
  and canonicalized by `attachment-native-host`, stored under root object
  `19b385128f4ee3d9e2a1a6f5adaabb73c46fea8463c8fb72f53b24966e75304c`,
  projected inside the explicit untrusted-data boundary, committed to the user
  message, and reopened with the same attachment, object, content, manifest,
  policy fingerprint, artifact-processor provenance, and receipt identities.
  The canonical projection is recomputed after reopen and must equal the frozen
  projection byte-for-byte. No scheduled task is involved.
- The redacted logical-store fixture is an import/recovery baseline, not a
  historical `runtime.sqlite3` or schema-migration artifact. It imports once
  into `conversations.v2`, round-trips, deletes its temporary plaintext JSON,
  reopens under the same deterministic key, rejects a different key, remains
  encrypted at rest, and preserves one encrypted document. Its key scope is
  `deterministic_fixture_only_not_personal_keychain`; this is not evidence about
  George's Keychain.
- Corrupt `native-host-prefix-cache.mom-llama` ciphertext is quarantined and
  becomes a cold miss after reopen. Corrupt `kv-cache.v3.blob.w1-tampered`
  ciphertext invalidates its metadata, deletes the unusable blob, and becomes a
  cold miss. The exact typed logical cache states before corruption and after
  reopen are independently frozen and hashed. In both cases the authoritative `conversations.v2` value is
  loaded through the production store as a typed `ConversationDb` before and
  after corruption and after reopen.
- The row-8 case runs a controlled fake owner inside the full `AppRuntime`.
  Application quit closes admission and publishes cancellation before the owner
  publishes its authoritative cancelled terminal; the runtime then releases and
  joins its retained worker, drains the gateway and application work, joins the
  empty native host, and reports exact expected/joined worker equality with zero
  active operations or retained tasks. A new `AppRuntime` over the same encrypted
  durable store reopens the exact draft, admits and joins a fresh successful
  operation, and shuts down without reusing the closed supervisor lifetime.
- A smaller raw-supervisor test remains supporting evidence only. It is not the
  source of the row-8 projection.

## Verification

Passed locally on 2026-08-12:

- `cargo fmt --all -- --check`
- `cargo test -p mom-llama-runtime --features unstable-w1-vertical-fixtures w1_ --no-fail-fast` — 5 passed
- `cargo test -p mom-llama-app --features unstable-w1-vertical-fixtures w1_ --no-fail-fast` — 2 passed
- `cargo test -p mom-llama-app --features unstable-w1-vertical-fixtures w1_vertical_fixtures::full_app_runtime_quit_joins_fake_owner_and_reopens_same_store -- --exact` — 1 passed
- `cargo test -p mom-llama-runtime --features unstable-w1-vertical-fixtures --no-fail-fast` — 97 unit tests passed; 38 integration tests passed; 13 real-model tests ignored by their existing hardware/model gates
- `cargo test -p mom-llama-app --features unstable-w1-vertical-fixtures --no-fail-fast` — 44 passed
- `cargo clippy -p mom-llama-runtime --features unstable-w1-vertical-fixtures --all-targets -- -D warnings`
- `cargo clippy -p mom-llama-app --features unstable-w1-vertical-fixtures --all-targets -- -D warnings`
- Rust 1.88 locked default checks for `mom-llama-runtime` and `mom-llama-app`;
  the Rust 1.92 vertical protocol is absent from both default dependency graphs.

## Deliberate boundary

This slice does not claim real-model inference, a personal Keychain unlock, a
historical database-schema migration, a native GUI relaunch, or hosted-provider
coverage. The prior-store row is therefore an honest redacted logical
import/recovery baseline, not a claim about old physical bytes. The
full-AppRuntime row-8 case intentionally uses a deterministic fake owner and
does not claim that a native GUI process was relaunched. The five Mom-owned cases are validated
through authenticated central-protocol manifests and exact projections, but
they do not accept the cross-product row or the final eighteen-row lock.
