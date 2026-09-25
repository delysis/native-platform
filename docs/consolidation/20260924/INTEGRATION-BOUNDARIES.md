# Consolidation boundaries and acceptance

Pinned base: `637e60b6b044230ed24ed3118615a2e5538cae83`.

## What this candidate actually changes

The stack supplies an FTE cancellation repair, one bounded document projection
core shared by the two products, a single executable choosing one existing
Tauri shell, quiet workspace-pane defaults, corrected ownership/format docs,
and preservation/review tools. It does not claim a finished common frontend,
common encrypted store, or complete import of all outstanding branches.

The executable routing is real source wiring, not a CLI that spawns another
application. `loom-app --mode document` calls the unchanged Loom library;
`loom-app --mode chat` calls Mom's retained Tauri entry after extracting it as a
library. Both are linked, but only the chosen composition root starts. The old
Mom executable delegates to the same function and remains until parity passes.

The CLI selector is desktop-only; the existing mobile library entry remains
document-only and does not take a dependency on the retained desktop Mom shell.

This is an intentional transitional boundary. Do not present a two-shell
launcher as a completed chat/document integration, and do not retire either
vertical until its complete product journey is proven in the final shell.

## Common document versus storage, view and model input

A common document consists of typed, ordered parts. Each part retains source
occurrence, selected source range, exact authored UTF-8, part kind, and typed
product metadata. The chat adapter borrows the entire Message record rather
than reconstructing a lossy subset. Distinct occurrences stay distinct even
when their bytes or digests match. Chat role names retain the current serde
wire contract. No role is inferred from Markdown headings, speaker labels,
filename or generated prose.

`Document::text()` concatenates exact selected bytes. `transcript()` is an
explicitly labelled human-readable view, not a canonical prompt or persistence
format. Template application remains at the existing model adapter; raw prose
is not silently converted to chat. Loom's source-specific range/UTF-8 errors
remain intact. The common projection adds an explicit 128 MiB / 65536-part
budget; it rejects rather than truncates, and rendered-size checks run before
allocation. This budget must be reviewed against product limits before merge.

Mom's encrypted store/keychain identity, Loom's visible manuscript and private
sidecars, and the stores' schema acceptance are unchanged. No legacy codec,
automatic migration, plaintext export, database deletion, WAL deletion or key
rotation is introduced. A later common persistence owner must support both
frontends and preserve encrypted chat before deleting either old store path.

## Existing consultation authority to preserve during the next cutover

Mom's retained commands cover typed chat dispatch; expert/persona freeze,
version, group, instantiate, update and removal; mentions and synthesis;
model-context grants and information citations; attachment imports/previews;
MCP/tool loops and explicit approvals; caches and slots; dictation/read-aloud;
conversation/message branch operations; drafts, import/export and cancellation.
`inventory/mom-application-commands.csv` is the audit-derived non-loss checklist,
not a proof that every command ran in this environment.

The normal chat mode still reaches that implementation. The document shell
has NOT yet been rewired to use its consultation dispatcher. Completing that
integration requires a single application-owned coordinator, not a second
independently constructed Native owner inside the Loom plugin.

Keep frozen expert prompt/history, persona version, template/model/sampler
identity, selected source occurrence/version/range, tool bindings, approval
identity, cache fingerprint and per-attempt cancellation identity. Never infer
an actual cache hit from identical prompt text: require the existing receipt.
A referenced expert is prompt context, not permission to execute tools.

## Minimal-by-default versus actual authority

The candidate fixes an observed mismatch: all four workspace panes previously
became visible when even an empty `.loom.md` config was enabled. Now only the
editor is visible until each optional pane explicitly sets `visible = true`.
Theme-only configuration and malformed-config fallback do not reveal advanced
panes. This preserves the existing strict parser and authored config document.

That is NOT a universal feature gate. Other advanced Mom controls, model/API
configuration, browser/terminal affordances and services need a complete
per-feature policy pass. Hidden controls do not revoke IPC authority. The
startup config in `desktop-launch` selects only a shell and cannot grant
network, arbitrary paths, tools, model downloads or a different storage key.
Hosted FTE routing must remain explicit and local-only requests must not fall
back to hosted providers. Peer/Signal/relay/compute features must not start
because a configuration file, attachment, workspace or saved view was opened.

## EASL boundary

Normal main remains Tauri/Wry. Preserve EASL text editing, input, accessibility,
layout/rendering and required licensed vendor code as a separate research
workstream. Do not import the stale Loom-EASL product shell to obtain text work.
The supplied preservation tool writes an independent local Git bundle and a
focused source archive from exact commits; the complete branch history remains
available while its product-specific frontend is parked. It does not claim to
have ported the renderer to a general-purpose Tauri backend.

## Gates before removing the compatibility shell

1. Build and package the single executable on each supported desktop platform.
   Prove that the selected shell loads its own embedded assets and ACL, including
   development TAURI_CONFIG overrides, signing identifiers and Keychain access.
2. Mom parity: existing encrypted conversations remain readable with the same
   key; typed messages, branching, drafts, arbitrary Attachments, citations,
   long cached expert consults, group synthesis, cancellation and approvals all
   work through the selected chat mode. Prove cancel-before/while/after run,
   shutdown during unlock/build/run, and failed joins retaining safe ownership.
3. Loom parity: unchanged authored whitespace, CRLF, focus/IME/caret, source
   projection, correlated real ghost render, candidate identity, acceptance,
   exact reversal, persistence/reopen and 75-second idle/resume. Preserve the
   existing packaged-native gates; never substitute a fixture or UI-only pass.
4. One simplified frontend with a common command/document coordinator. Then
   remove duplicated rendering/history/prompt code behind proven adapters;
   do not remove a feature merely because it was not in the most recent smoke.
5. Retire the compatibility binary only after source, portable, browser and
   exact packaged-device evidence is attached to the candidate tree.
