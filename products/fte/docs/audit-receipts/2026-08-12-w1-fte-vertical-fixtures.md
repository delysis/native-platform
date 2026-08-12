# FTE Wave 1 vertical fixture status

This change retains `platform-contract-testkit` at accepted lifecycle revision
`cbab33555ab9355a6ac453d659c55ec9e0666821` and separately consumes
`platform-vertical-fixtures-v0` from exact commit
`9fd803f5efcc46ac0256dab876e7c0b1f03bb448`, tagged
`w1-vertical-protocol-v0-2026-08-12`. The dependency is optional and exercised
only by the Rust 1.92 Wave 1 job, preserving FTE's ordinary Rust 1.88 lane.

## Complete product-owned cases

### Hosted contract

The deterministic hosted case binds the complete checked-in corpus and an
exact expected projection through the central manifest validator. Production
provider code translates chat, raw-text completion, and raw-token completion;
parses chat and raw responses; consumes an authenticated stream split inside a
JSON key, inside text, across an SSE delimiter, and inside `[DONE]`; preserves the
request identity; rejects unsupported BOS mutation; and redacts unsafe provider
error detail. The projection authenticates each canonical request, response,
stream, error, and request-ID byte sequence by SHA-256.

No credential is loaded and no hosted network request is made. Live-provider
interoperability, availability, billing, and quota behavior are omitted claims,
not promotion gates.

### Local loopback

The local case uses the real product `Gateway`, HTTP protocol adapters,
`LoopbackServer`, authentication and host checks, concurrency limiter,
file-backed `SqliteStore`, response retrieval, full store close/reopen across
server shutdown/restart, and continuation by previous response ID. The exact
stored response is fetched after reopening and compared with the pre-shutdown
response. Only model generation is deterministic. The listener is
asserted loopback-only; absent authentication, DNS rebinding, and concurrent
over-admission fail closed. The central validator authenticates the shared
request corpus and exact projected results.

Both cases authenticate one checked-in production-source descriptor. It binds
the exact pre-test prefixes of the two files that contain inline tests and the
Git tree identities of `fte-router`, `fte-protocols`, `fte-store`, and
`fte-types` at baseline commit `0ba33bb786f068830cf288c629d8eedc63e56029`.
The replay verifies that the fixture revision descends from that commit and that
every bound production root is unchanged. The generic projection makes no
listener-worker-count claim because `LoopbackServer::shutdown` intentionally
does not expose an authoritative joined-task receipt.

## Partial supporting row

### FTE legacy database

The earlier draft created a current database and then added a test-only legacy
table. That is not an independently produced prior database and has been
removed from this change.

Existing production-path tests still support the credential behavior: exact
write/readback precedes plaintext retirement, every crash boundary preserves
recoverability, stale stores fail closed, and reopen observes retirement. They
do not freeze a redacted historical `gateway.db`/`gateway-v2.db` corpus from an
independent prior release. Because backward compatibility was explicitly
waived, this row remains partial/supporting rather than manufacturing a state
baseline.

The repository pin policy independently hard-codes both accepted revisions;
changing `w1-contracts.env` and the manifests/lock together cannot move either
authority. Only exact, uncommented, optional dependency declarations pass.

## Verification

- `cargo fmt --all -- --check`
- `./scripts/check-w1-contract-pin.sh`
- `./tests/w1_contract_pin_policy.sh`
- Rust 1.92 vertical tests: 23 passed
- Rust 1.92 vertical Clippy with `-D warnings`
- Rust 1.92 contract-router tests: 42 passed
- Rust 1.92 contract-router Clippy with `-D warnings`
- Rust 1.88 full workspace: 117 passed, two real-GGUF tests ignored because
  `MOM_LLAMA_MODEL_PATH` was not supplied
