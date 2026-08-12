# Mom W1 product-local vertical fixtures

Date: 2026-08-12

Accepted base: `097da612140c6479f9d40e7816f0500271464ca9`

Accepted vertical protocol: `9fd803f5efcc46ac0256dab876e7c0b1f03bb448`
(`w1-vertical-protocol-v0-2026-08-12`)

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
| `fixtures/w1/cache-corruption-v1.json` | `e66e5eed8a5a12029986e40d06c4829388c633855bc20326d1fc53738042a82e` | Exact Mom native-prefix and session-KV cache namespaces and their distinct cold-fallback dispositions |
| `fixtures/w1/chat-cancel-retry-manifest-v0.json` | `01f9cb204007ea4b2a93c9fa875c1424e7c65886c8f6c76d4a25bafad40be790` | Authenticated central-protocol case for Mom chat/cancel/retry |
| `fixtures/w1/chat-cancel-retry-projection-v0.json` | `5089d654ffc27ce2b2ce34d102a98cd0160a074cb2f9c458dd7fbdacac2bb441` | Frozen product-derived stream, lifecycle, durable-state, and ownership facts |
| `fixtures/w1/attachment-manifest-v0.json` | `018f798211f864a12f1f1b7bb57cae94c0e830bf05258878ec70ec0863236ab5` | Authenticated central-protocol case for Mom attachment |
| `fixtures/w1/attachment-projection-v0.json` | `fc10bb8b589c1290e546a7ad0b936b47954d541070154843b1c0beb193d4cb7f` | Frozen import/send/reopen projection |
| `fixtures/w1/prior-store-manifest-v0.json` | `e739e0b734551329543eb617df531b4f5a1b5389b0fe78698a15ddc299ed0b32` | Explicit redacted logical import/recovery case with historical DB evidence marked unavailable |
| `fixtures/w1/prior-store-projection-v0.json` | `2c7bd3fcd1e93ee5bc70ab8c3cf0be6a7f33b71bf03dbff9cd971306bf01b136` | Frozen logical recovery and plaintext-cleanup facts |
| `fixtures/w1/cache-corruption-manifest-v0.json` | `7f5b963a8674c06cf02409ed247adb468b85de52db70b9043e18618582f73d9c` | Mom-owned cases within the cross-product corrupted-cache row |
| `fixtures/w1/cache-corruption-projection-v0.json` | `3778e2484ec9eb6c7cde1d286716fc3ffb98dfdfe9f981f58a63c24da3f381c3` | Frozen native-prefix quarantine and session-KV invalidation projection |

## Product evidence

- `AppRuntimeHandle` admits the chat send, whose production
  `ChatStreamLifecycle` emits its own `started` event. The
  separately admitted production `chat_cancel` command reaches the request
  through `cancel_native_request`; only that native cancellation boundary
  releases the fixture wait. The same lifecycle then emits `cancelled`, commits no
  messages, preserves the encrypted draft exactly, and leaves no active chat
  request.
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
  cold miss. In both cases the authoritative `conversations.v2` fixture remains
  byte-exact.
- Supporting-only evidence: a controlled Mom operation owner receives
  cancellation during quiesce,
  reaches an authoritative cancelled terminal, releases, exits, joins, and
  leaves zero active operations or retained workers. A fresh supervisor starts
  in `running` with zero active operations and shuts down cleanly.

## Verification

Passed locally on 2026-08-12:

- `cargo fmt --all -- --check`
- `cargo test -p mom-llama-runtime --features unstable-w1-vertical-fixtures w1_ --no-fail-fast` — 5 passed
- `cargo test -p mom-llama-app --features unstable-w1-vertical-fixtures w1_ --no-fail-fast` — 2 passed
- `cargo test -p mom-llama-runtime --features unstable-w1-vertical-fixtures --no-fail-fast` — 96 unit tests passed; 38 integration tests passed; 13 real-model tests ignored by their existing hardware/model gates
- `cargo test -p mom-llama-app --features unstable-w1-vertical-fixtures --no-fail-fast` — 43 passed
- `cargo clippy -p mom-llama-runtime --features unstable-w1-vertical-fixtures --all-targets -- -D warnings`
- `cargo clippy -p mom-llama-app --features unstable-w1-vertical-fixtures --all-targets -- -D warnings`
- Rust 1.88 locked default checks for `mom-llama-runtime` and `mom-llama-app`;
  the Rust 1.92 vertical protocol is absent from both default dependency graphs.

## Deliberate boundary

This slice does not claim real-model inference, a personal Keychain unlock, a
historical database-schema migration, a native GUI relaunch, or hosted-provider
coverage. The prior-store row is therefore an honest redacted logical
import/recovery baseline, not a claim about old physical bytes. The
raw-supervisor quit/relaunch check is supporting-only; it is not an AppRuntime
same-durable-state relaunch acceptance. The four Mom-owned cases are validated
through authenticated central-protocol manifests and exact projections, but
they do not accept the cross-product row or the final eighteen-row lock.
