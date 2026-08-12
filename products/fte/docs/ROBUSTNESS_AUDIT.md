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
- Gateway and provider shutdown is explicitly quiescing: new registration and
  admission close atomically, active request IDs are cancelled, authoritative
  backend results and blocking bridges drain, and repeated shutdown is
  idempotent with an observable retained error.
- The Tauri plugin coordinates `RunEvent::Exit`, dynamic plugin removal, and
  concurrent loopback start/stop/rotation without nested-runtime panics or
  listener ownership races. Loopback graceful shutdown has a bounded abort
  fallback for stalled clients.
- The llama adapter borrows its injected `NativeHost`; its shutdown drains FTE
  operations but deliberately leaves the resident host for the embedding
  application to close with joined process-exit authority.
- The native prefix cache binds exact token and runtime fingerprints. A mismatch
  is a normal miss; required caching fails closed.
- Desktop commands and plugin loopback share one application-owned `Gateway`.
  The retired desktop Router, proxy, quota tracker/eval store, and hosted/FIM
  adapters have been deleted rather than retained as a test-only second path.
- Hosted credentials are read through the OS credential store. Fresh databases
  never create `api_keys`; databases with that plaintext table and all other
  unversioned or foreign schemas are rejected before schema mutation. The
  compatibility importer has been deleted.
- The desktop registers the borrowed native llama backend and exposes the stable
  `local/default` identity. A real Qwen GGUF has passed the in-process desktop
  route, Gateway drain, and application-owned host join test.
- The Providers view has a native GGUF picker. The sole Gateway owner validates
  and registers the selection, persists only the canonical path and optional
  digest, and restores it at startup. Portable tests cover database reopen,
  owner restart, a missing saved file, an invalid replacement, registration
  failure rollback, and visible `ready`/`invalid` frontend states.
- Legacy `task_hint` is explicitly rejected because no equivalent modern typed
  routing/evaluation contract exists; it is neither silently dropped nor used
  to resurrect the retired router.

## Remaining acceptance boundary

- Exact bundle `FTE R7 372a088.app` produced two visible real-Qwen
  `local/default` generations around Cmd+Q and immediate relaunch, which
  visibly restored the saved provider as `ready`. A later attached launch of
  the same executable observed native Cmd+Q exit with code 0 and no signal.
  The acceptance did not separately observe a native-picker click.
- No independently produced legacy database was available to authenticate.
  Backward compatibility was waived, so no synthetic database is presented as
  migration evidence; legacy schema is an explicit unsupported input.
- No live hosted credential was used for a provider request. Hosted protocol
  transformations remain fixture-tested rather than live-service evidence.
- IPv6 listener failure does not yet have a structured status field. IPv4 is
  authoritative and IPv6 is best effort.
- The proposed `fte-core-2026-08` compatibility manifest and SDK conformance
  suite are not yet published as a versioned machine-readable profile.
- Local STT/TTS hardening and real-audio acceptance are tracked independently
  in `delysis/speech-native-kit`; they do not gate text/provider releases.

## Next safe slice

Run one authorized hosted-provider chat and completion request when a revocable
credential is already available, without weakening the fixture gates.
