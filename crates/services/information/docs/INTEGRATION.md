# Product integration

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

## Loom

Global library payloads stay outside `.loom` projects. A Loom branch records the
resource ID, release ID, representation ID, source locator, selected excerpt,
and excerpt hash. `.loom/indexes/` is appropriate only for project-local
overlays and disposable caches.

## Tauri authority

The default permission set grants status only. Separate permission sets grant
catalogue inspection/planning, visible local-UI query/read, exact-target
model-context query, external registration, mounting, acquisition, and removal
planning. Local-UI commands force `local_ui`; only the typed model tool path can
request `model_context`.

The external-registration command accepts an opaque, app-owned native picker
grant, never a raw path from webview IPC. The embedding app supplies the grant
resolver and is responsible for caller binding, expiry, and replay prevention
while resolving the token to an authorized absolute path. Mounting is a
separate permission and can rebind any of the four compiled SQLite profiles at
startup without granting registration or query authority. Webview mount IPC
cannot opt a private Community Archive into model context; that policy remains
a trusted host-startup decision.

Acquisition authority is also constrained inside the host: `file:` roots and
private-network destinations require explicit grants. File pickers, user
confirmation, app-data paths and Windows DACL setup, background scheduling,
progress UI, cancellation, and display of license/disk impact remain product
responsibilities.
