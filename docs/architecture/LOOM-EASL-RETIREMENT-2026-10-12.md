# Loom EASL and standalone editor SDK retirement

The 2026-10-12 product decision retires the alternate Loom EASL editor and both
standalone editor SDKs, `loom-markdown` and `loom-text-session`. It accepts
external API breaks. This is deliberate capability withdrawal, not a claim
that their tested or functional code was dead.

The strongest reason to keep the assembly was an independent non-WebView
writing route: winit/Softbuffer, native AccessKit/input/focus, EASL text policy,
source/visual/verse panes and an optional Wry companion. The Markdown SDK also
offered exact-byte, selection, revision/history and atomic editing contracts
without window or storage dependencies. The text-session SDK offered a bounded
leased project worker, draft/checkpoint receipts, exact source capture and
configuration. Those distinct APIs and their own controls are withdrawn.

The chosen product remains `products/loom/apps/loom` and its Tauri plugin.
Current repository imports of the two SDKs were confined to the EASL assembly
and their own tests. Shared Store, Document, Host, Types, preferences, native
models, imports/materials/PDF, Speech, Signal and peer owners are retained.
No implementation is copied or ported into a new runtime layer.

The three Cargo members, EASL Git text-engine patches and their pin-policy
exception, package-group entries, reviewed build script and two opt-in renderer
checks are removed coherently. The whole-App/CSS reference and icon extraction
guard belonged to the retired native port and is removed with that owner.
Retained build-script hashes and current test/evidence guards are unchanged.
Only lock nodes exclusive to withdrawn roots are removed; retained canonical
package identities, sources, checksums and ordered dependency edges stay exact.
All platforms and features represented by the current lock are considered.
The separate Signal and Attachment fuzz workspaces and locks remain.

`loom-config` remains a separate workspace root with its current tests/API.
After retirement its former app/session runtime consumers disappear; a wider
configuration-SDK withdrawal is not part of this decision.

Dated experiment records and all previous failed receipts remain attached to
their original Source. The original public implementation is recoverable at
`979a7cc9d8ec206acad5f86e3839c840faae8d5a`. Canonical EASL research remains
outside this repository in `delysis/easl`; it is no longer an allowed current
root-workspace Git dependency.

The decision and Source graph are not compiler, resolver, component, native,
model, performance, accessibility or packaged-product qualification. Current
retained-product validation still runs through the existing owners and gate.
