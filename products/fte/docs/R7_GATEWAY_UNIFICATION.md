# R7 Gateway unification ledger

Audited base: `d86edfef40771589af4cd8cd72907fa6d3ce6e96`  
Working disposition: production authority unified; promotion remains gated on live
credential/keychain, hosted-provider, and local-GGUF evidence.

## Production authority

| Command family | Current authority | Evidence |
|---|---|---|
| Models | `GatewayRuntimeOwner` projects the hosted product catalog and configured local descriptors from the same `Gateway` | hosted parity plus `desktop_native_backend_uses_the_shared_gateway_and_local_only_route` |
| Provider inventory/readiness | Gateway backend snapshots plus nonsecret request-log aggregates | `provider_inventory_matches_the_promoted_golden_fixture` |
| Secrets | OS credential store selected by `keyring` 4.1.6 | exact-readback save path and credential migration crash fixtures |
| Playground chat | strict OpenAI chat codec to `Gateway::execute`; allowlisted Anthropic/Gemini compatibility fields become typed canonical state | desktop typed-field/tool-history fixtures plus `fte-providers` emission fixtures |
| Raw completions | strict OpenAI completion codec to `Gateway::execute` | exact prompt/token protocol fixtures and full desktop suite |
| Responses | plugin loopback over the same `Arc<Gateway>` | Responses protocol fixtures and real loopback socket fixture |
| Usage/activity | Gateway response route/usage appended to the bounded nonsecret request log | database aggregate fixtures and desktop command path |
| Proxy/loopback | `tauri-plugin-free-token-energy` loopback over the shared Gateway | authenticated real socket and shutdown fixtures |
| Profile/settings | nonsecret desktop database only; dormant duplicate proxy settings commands removed | desktop database/frontend tests |
| Local model selection | native desktop file picker into the sole `GatewayRuntimeOwner`; nonsecret path/digest persisted in SQLite and restored at startup | reopen, owner-restart, invalid-path, rollback, and frontend-state fixtures |

`run()` constructs one `GatewayRuntimeOwner`. Its `Arc<Gateway>` is passed to
the plugin loopback, while desktop commands receive the owner. The production
app no longer constructs or manages the legacy `Router` or `ProxyManager`.
The retired Router, proxy server, quota tracker/eval store, and old hosted/FIM
provider implementations have been deleted. Duplicate desktop provider wire
DTOs are also gone: Tauri JSON crosses immediately into strict canonical
protocol types and canonical response emitters. Durable contract coverage is
carried by explicit modern golden, protocol, provider, and socket fixtures.

The same owner registers one borrowed `llama-native` adapter on that Gateway
and owns the underlying `NativeHost`. `configure_local_model` accepts only an
explicit absolute regular `.gguf` path and an optional lowercase SHA-256,
publishes the stable `local/default` identity without exposing the filename,
and leaves loading lazy until the first request. Exact local requests are
`LocalOnly`; hosted and `auto` requests remain `HostedOnly`. The Providers view
opens a native `.gguf` picker and receives only the basename plus a typed
readiness state. The canonical path and optional expected digest are stored as
non-secret local configuration in the private SQLite database. Startup restores
that record through the same owner. A missing or invalid saved file leaves the
record available for diagnosis/replacement, publishes no local route, and is
shown as `invalid`; an invalid replacement leaves the last working selection
unchanged. A native registration failure rolls the database record back. After
the plugin drains the Gateway on application exit, the desktop retains
process-exit join evidence from its application-owned host.

## Public model aliases

Provider descriptors retain exact upstream model IDs and carry public aliases
as routing metadata. `ModelSelector::ExactModel` matches either the exact ID or
an alias, and the Gateway scores all matching candidates. Public aliases that
contain `/` are therefore not misread as backend-qualified routes by the
desktop compatibility edge.

## Credential migration invariant

Fresh databases never create `api_keys`. Startup first detects whether an
upgraded database actually contains the legacy table, then processes its rows
as follows:

1. Read the legacy value.
2. Refuse to overwrite a different existing OS credential.
3. Write when absent.
4. Read back and compare exact bytes.
5. Recheck the complete ordered source set inside a SQLite transaction.
6. Delete all rows and drop the table in that same transaction.

Fixtures interrupt before write, after write, after readback, before retirement,
and before table drop. Every interruption retains the complete plaintext table
and rows. A later-row failure, readback mismatch, concurrent source change, or
OS-store conflict fails closed and also retains the source. Empty preexisting
tables are retired; a fresh database remains a no-op. Normal credential reads,
writes, and deletes never use SQLite.

## Intentional parity difference

The retired router calculated quota headroom from a second `QuotaTracker`.
Modern provider projection reports only observations present on Gateway model
descriptors. Until such observations exist, headroom is unknown rather than a
fabricated full quota; the dashboard renders a zero aggregate while individual
providers retain `null`.

The product catalog does not advertise `codestral-latest`, and the retired FIM
adapter has been deleted. The modern hosted backend has no strict FIM
request/response/streaming contract, and sending a generic
Completion request to `/v1/fim/completions` would be false compatibility. FIM
may return only with a dedicated typed codec and modern backend fixtures.

The legacy desktop `task_hint` argument is rejected whenever it is supplied.
The retired task router's evaluation store had no modern typed ingestion or
Gateway request contract, so translating the string would invent routing
semantics and silently ignoring it would claim false compatibility. Callers
must omit `task_hint`; a future replacement requires an explicit typed Gateway
evaluation signal and fixtures.

## Remaining promotion gates

- Exercise migration and exact readback against the real platform credential
  store on a legacy installation. Deterministic fake-store fixtures pass, but
  they are not OS-keychain acceptance evidence.
- Exercise at least one real hosted credential through chat and completion.
- Exercise the native picker and restored model from a launched desktop bundle;
  the picker IPC, honest UI states, durable owner restart, and real model load
  are covered on their respective sides of the GUI boundary, but no launched-
  bundle interaction occurred here.

## Verified on 2026-08-11

- Portable workspace gate: 102 passed, 2 ignored (the two real-GGUF tests).
- Product gate: 29 desktop Rust tests and 2 frontend tests passed.
- New local-model coverage passed for SQLite reopen, a fresh-owner restart,
  missing saved files, invalid replacements, native-registration rollback, and
  frontend `invalid` to `ready` state rendering. The compatibility test also
  proves supplied `task_hint` values are rejected rather than ignored.
- Both ignored real-GGUF tests passed with
  `Qwen_Qwen3-0.6B-Q4_K_M.gguf` (484,220,320 bytes, SHA-256
  `9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`).
  The desktop test exercised Metal inference, the shared Gateway route, drain,
  and retained native-host join; the adapter test exercised a second-request
  stable prefix hit in process.
- Clippy with warnings denied and rustfmt check passed.
- Module-boundary, native-pin, and workflow-policy gates passed.
- Hosted provider fixture tests: 14 passed.
- Loopback tests: 7 passed, including the real loopback socket fixture.
- Model-free native adapter tests: 11 passed, plus the desktop shared-Gateway,
  local-only routing, stable identity, and idempotent host-drain fixture.

No commit, push, pull request, live hosted request, real OS-keychain acceptance
run, or launched desktop-bundle acceptance was performed in this worktree.
