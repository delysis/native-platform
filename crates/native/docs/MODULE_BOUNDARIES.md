# Ecosystem module boundaries

The canonical source is the root `delysis/native-platform` workspace. Paths
below are workspace paths, not separately maintained product repositories.
Arrows mean **depends on**:

```text
products/loom  ──> crates/native
products/mom   ──> crates/native
products/fte   ──> crates/native (through fte-backend-llama)

Loom / Mom application composition ──> Attachment, Information and Speech
Optional gateway composition        ──> products/fte reusable gateway crates
```

Mom's local chat/consult route calls Native directly. It does not require FTE,
HTTP, a loopback listener or a child inference process. FTE is an optional,
separately authorized routing edge, not an intermediary forced into local chat.

| Workspace location | Owns | Must not own |
|---|---|---|
| `crates/native` | llama.cpp DTOs, owner-thread engine, resident host, cache contracts | products, Tauri, protocols, providers, STT/TTS |
| `products/fte/crates` | protocol gateway, routing, providers and optional authenticated loopback | product document authority or copied native engines |
| `crates/services/attachment` | bounded byte inspection, canonicalization, provenance and plans | ambient filesystem/network authority or silent transforms |
| `crates/services/information` | registered-source retrieval and citation identity | automatic model-context permission |
| `crates/services/speech` | local speech contracts, backends and supervised execution | routing or permission on behalf of every product |
| `products/loom` and `products/mom` | product storage, context grants, UI, receipts and lifecycle composition | copied service implementations |

Shared implementation belongs at the lowest product-neutral boundary that can
preserve its authority and lifetime rules. Consolidation must not replace exact
cache ownership, cited representations or joined-worker evidence with a merely
similar prompt or a cancellation request.

## Native-kit public boundary

Downstream callers construct one `NativeHost`, inject any persistent prefix
store, load bounded resident models and submit `llama-native-types` requests.
The Native service never selects a database, key manager, network policy, hosted
provider, application data directory or user interface.

Every prefix lookup has an explicit `CacheOwnerScope`: `Unowned` matches only
artifacts without an owner, while `Exact(owner)` matches only that owner. A
generic chat lookup cannot borrow a persona-, conversation-, or skill-owned
artifact merely because its tokens are compatible.

New products should use `GenerationBatchRequest` for branch families. Legacy
`GenerationRequest` completion/chat calls remain wrappers over the same raw
case path, while `SharedPrefixBatchRequest` remains available for existing
chat-oriented consumers. Case IDs are causal and cancellation identities, not
content hashes: identical output bytes never collapse two generation cases.

`crates/native/scripts/check-architecture.sh` enforces the negative half of this contract.
