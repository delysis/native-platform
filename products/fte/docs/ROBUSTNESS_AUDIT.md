# Robustness and unification audit

This ledger records the current boundary after the reusable gateway workspace
was introduced. It deliberately does not treat a design plan as implementation
evidence.

## Verified now

- Canonical protocol-neutral requests, typed items/events, stable errors and
  usage provenance live in `fte-types`.
- OpenAI Completions, Chat Completions and Responses plus Anthropic Messages
  are encoded at protocol edges instead of being routed through one lossy chat
  shape.
- The embedded llama.cpp adapter executes through a pinned immutable
  `llama-native-kit` revision with no HTTP or subprocess hop.
- The reusable loopback edge is opt-in, bearer-authenticated, Host/Origin
  checked, bounded, and covered by a real socket test.
- Loopback token creation and rotation use private atomic files. Authentication
  comparison is constant-time for the fixed-size installation token.
- Gateway routing enforces privacy/capability/readiness gates, bounded
  admission, route affinity, pre-output-only retry, deadlines, cancellation and
  one terminal event.
- The native prefix cache binds exact token and runtime fingerprints. A mismatch
  is a normal miss; required caching fails closed.
- Speech shutdown attempts every registered backend and aggregates failures.

## Still a breaking migration, not release evidence

- The desktop window still serves its historical UI through the legacy
  `Router` and `gateway.db`, while the plugin and reusable loopback use the new
  `Gateway`. Provider configuration is shared only through a deferred database
  secret resolver. The UI is not yet proof that the reusable gateway is the
  sole authority.
- Legacy provider secrets remain plaintext in the permission-restricted SQLite
  database. Moving them to an injected OS credential store requires a
  transactional migration with verified read-back before deleting old rows.
- The desktop does not yet discover and register local GGUF descriptors. The
  `fte-backend-llama` adapter is real and is consumed by Mom Llama, but the FTE
  desktop currently registers hosted routes only.
- Plugin `on_drop` still performs blocking async shutdown. A dedicated,
  idempotent application-exit shutdown coordinator remains necessary.
- IPv6 listener failure does not yet have a structured status field. IPv4 is
  authoritative and IPv6 is best effort.
- The proposed `fte-core-2026-08` compatibility manifest and SDK conformance
  suite are not yet published as a versioned machine-readable profile.
- Speech request-size/duration limits and worker-join semantics need a separate
  platform-backend hardening slice.

## Next safe slice

Create one `GatewayRuntime` in the desktop application and migrate one command
family at a time behind golden old/new parity tests. Migrate credentials only
after the new resolver writes to the OS store and reads the exact value back.
Once every desktop command and the playground use that runtime, remove the old
router, proxy and database in one explicit pre-1.0 migration commit.
