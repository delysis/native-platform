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
parses chat and raw responses; consumes fragmented stream frames; preserves the
request identity; rejects unsupported BOS mutation; and redacts unsafe provider
error detail. The projection authenticates each canonical request, response,
stream, error, and request-ID byte sequence by SHA-256.

No credential is loaded and no hosted network request is made. Live-provider
interoperability, availability, billing, and quota behavior are omitted claims,
not promotion gates.

### Local loopback

The local case uses the real product `Gateway`, HTTP protocol adapters,
`LoopbackServer`, authentication and host checks, concurrency limiter,
`SqliteStore`, response retrieval, server shutdown/restart, and continuation by
previous response ID. Only model generation is deterministic. The listener is
asserted loopback-only; absent authentication, DNS rebinding, and concurrent
over-admission fail closed. The central validator authenticates the shared
request corpus and exact projected results.

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
