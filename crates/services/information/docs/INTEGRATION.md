# Product integration

<!-- current-service-surface: information -->
<!-- retired-edge-parent: 5390edfcfb6b8412ae45f1e51d44644be3f7e8e3 -->

Applications depend on the host or leaf crates they actually need. Until an
authorized remote exists, sibling native applications should use explicit
workspace-relative paths; a future Git dependency must pin an exact revision.
The native kit does not depend on Loom, a model runtime, or a Tauri app.

```toml
[dependencies]
information-native-host = { path = "../information-native-kit/crates/information-native-host" }
```

## Retrieval-to-model boundary

```text
user request
    -> app chooses allowed resources and query budget
    -> information host returns typed evidence
    -> app filters by UsePolicy and records selected evidence IDs
    -> app delimits excerpts as untrusted source material
    -> llama-native-kit or a provider gateway performs generation
    -> app retains locators and excerpt hashes with the generation
```

The information host can publish a JSON tool definition, but tool execution
returns `EvidenceSet`; it does not manufacture prose or merge source text with
system instructions. Backend-native query syntax should not be exposed to a
model unless the app grants that capability deliberately.

## Managed-document boundary

Trusted native integrations may call the host's narrow
`materialize_documents`, `search_managed_documents`,
`plan_managed_documents_removal`, and `remove_managed_documents` methods. The
caller supplies typed content and identities, never an output database path.
Materialization does not read the source URI: a source-specific adapter must
already have verified and bounded the inert text and must retain exact artifact
and record lineage in the contract.

For Kiwix, `materialize_zim_documents` accepts a native-only
`ZimMaterializationRequest` containing one already-authorized local archive
path plus its expected size/SHA-256 and catalogue identities. The host invokes
the bounded OpenZIM producer and then the same atomic managed-document staging
path. The path type is not serializable and this method is not part of the
model tool or renderer IPC surface. OPDS/Metalink discovery and acquisition
remain separate; conversion never performs network fallback.

Managed search is local-UI-only. It does not appear in the model tool surface,
and the default use policy leaves model context unknown/fail-closed while
forbidding export and redistribution. A product that later grants a specific
conversation access must do so through a separate exact capability rather than
changing this storage default.

## Loom

Global library payloads stay outside `.loom` projects. A Loom branch records the
resource ID, release ID, representation ID, source locator, selected excerpt,
and excerpt hash. `.loom/indexes/` is appropriate only for project-local
overlays and disposable caches.

## Product edge authority

The current executable integration boundary is
`crates/services/information/crates/information-native-host`; products call its
narrow native methods and keep paths outside renderer contracts. There is no
current `crates/services/information/crates/tauri-plugin-information-native`
crate, command registry, or permission set. Its last-present implementation is
historical at exact parent
[`5390edfcfb6b8412ae45f1e51d44644be3f7e8e3`](https://github.com/delysis/native-platform/tree/5390edfcfb6b8412ae45f1e51d44644be3f7e8e3/crates/services/information/crates/tauri-plugin-information-native).

A future product-specific edge must receive opaque, app-owned picker or target
grants rather than renderer paths; bind caller, expiry, and replay; separate
mount, local-UI query, and model-context authority; and expose only commands
needed by that named vertical. Historical generic plugin permissions are not a
current contract or a template to re-enable wholesale.

Acquisition authority remains constrained inside the host: `file:` roots and
private-network destinations require explicit grants. File pickers, user
confirmation, app-data paths and Windows DACL setup, background scheduling,
progress UI, cancellation, and display of license/disk impact remain product
responsibilities.
