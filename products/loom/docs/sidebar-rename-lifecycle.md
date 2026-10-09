# Sidebar rename/lifecycle continuation

## Exact scope and status

This continuation targets the clean selected source declared in the supplied
manifest: commit `4aa494f380b4fb2049712407536406d4fef981b6`, tree
`643ca2649cbd407465693b474322001d86178260`. It extends, rather than replaces,
[the integrated interaction contract](sidebar-interactions.md). That earlier
receipt describes an earlier intake; its missing-hydration note and execution
counts are not the status of this continuation. The root and Loom contracts,
CONTRIBUTING, streaming contract and supplied hydration continuation were read.

**This is a bounded implementation, not completion of every rename requirement
or native acceptance.** All changed paths are under `products/loom`.

| Sidebar target | Application menu | Rename owner in this patch |
| --- | --- | --- |
| Remembered root, including an unavailable root | Existing typed application menu | Existing bookmark-only label owner; no filesystem move |
| Registered document | Existing typed application menu | Existing session/revision/blob-bound document rename; protections retained |
| Ordinary placed or unplaced material | Typed application menu including Rename | New native binding display-name owner; no source or placement move |
| Derived nested folder | Existing toggle and Copy Path menu | **No directory rename implementation or advertised rename capability** |
| Protected or unobserved material | Applicable read-only actions | Metadata mutation not enabled; native protected-state rejection retained |

No new renderer, pane, model, bridge, credential logic, arbitrary path mutation
command, schema migration, automatic source promotion or alias database is added.

## Material metadata owner

`material_commands.rs::material_rename` binds admission to the existing live
project and session, parses a correlation command ID, and dispatches a typed
`materials::MetadataChange`. The existing pin/remove commands now also require
the captured `expected_metadata_revision`. Removal commits registration removal
before forgetting a selected-source grant; a stale remove cannot revoke a fresh
grant. The internal library-import rollback remains a separately owning operation.

`materials/metadata.rs` owns only `.loom/materials/bindings.json`. On Unix it
walks the native canonical root and fixed private children with held, no-follow
descriptors. It rejects redirected parents, symlinks, non-regular files,
hard-linked bindings, oversized metadata and changed native observations. A
revision hashes exact bytes and native file/directory identity and timestamps;
it is an ephemeral observation, not a persisted identity table or source grant.
No renderer path is used by this owner. The existing ProjectStore lease and
material WRITE_LOCK serialize authorized writers.

Mutation validates the current `loom.materials.v1` representation and stable
binding identity. Display names retain their exact spelling, including legal
leading/trailing spaces, Unicode and slash characters; names are not paths.
The existing 1–512 printable UTF-8-byte name limit is enforced without trimming
or truncating the stored value. Duplicate names are legal and remain ambiguous
for name-only lookup; they never merge registrations or sources.

Rename changes only the existing binding `name` field. ID, attachment ID,
workspace placement, original source, retained evidence and grants are not
retargeted. Newly emitted material references contain the full material ID,
not a label-derived short qualifier. Existing full-ID Markdown links and retained
consult admissions remain bound to their original source even after another
source reuses its former display name. A display-name change is not authority
to rewrite frozen admission/evidence names or immutable revisions.
Name-only and name-qualified expressions remain human name queries: an old label
may become missing or ambiguous after rename. This patch neither migrates
manuscript strings nor adds historical name aliases. The preserved references
are full-ID/evidence links and already frozen consult admissions.

The observation belongs to the whole bindings file. A committed change to any
binding intentionally makes other observations stale. Lists carry one generation
for all rows; the renderer refreshes other rows after a mutation. An unchanged
read preserves the existing row object lease. A changed generation never does.
An in-progress stale rename retains its exact entered text but requires explicit
cancellation/reselection rather than silently rebasing the intent.

### Atomicity, interruption and trust boundary

A private, exclusively created 0600 staging file is written and synced. The
owner rechecks the original observation, the held directory chain and the
staging file's identity/bytes, then atomically replaces the fixed bindings file
and syncs its directory. The reply verifies the resulting native generation.
No document journal or immutable artifact is edited by a metadata rename.

Before replacement, a failed operation leaves the old committed bindings. After
replacement, a lost reply or durability/readback error may mean the new name
committed: the renderer reports failure and performs a read, **never automatic
mutation replay**. A stale request does not become an idempotent replay command.
Successful replies explicitly correlate project, session, request, old generation,
full material identity and exact new name.

Ordinary pre-commit failures remove only a still-owned staging file. A foreign
replacement is retained; a process crash may leave a hidden `.pending` orphan.
Orphans are never read as bindings, promoted as source data or automatically
recovered into a manuscript. Reopen reads only the committed `bindings.json`.
No orphan garbage-collection or metadata command ledger is introduced.

This is serialization against authorized owners, not an atomic compare-and-swap
primitive against an uncooperative process editing the private store between
final validation and rename. Descriptor anchoring prevents symlink redirection;
pre-commit replacement races are rejected. Native identity/timestamp observations
are not a persisted monotonic generation counter and have not been qualified on
coarse-timestamp or hostile filesystems. Non-Unix metadata mutations explicitly
reject before writing; no new cross-platform qualification is claimed.

## Renderer lifecycle and editing

Both placed and unplaced material rows use the existing inline rename pattern:
select-all on entry, Return/F2/context dispatch, Escape cancellation, blur commit,
and IME/229 ownership with commit deferred until composition ends. The native
text input retains its editing/context-menu/undo owner. The original document,
context, terminal and chat editors are not replaced.

Root, document and material inputs no longer have an unguarded two-way value
binding. Input, blur, key and composition handlers reject events from detached
controls, so an old input cannot alter or commit the next target's rename.
Document filesystem rename continues through its existing captured-owner,
dirty-input, recovery, journal and receipt rules.

Material list replies are request-ordered and session-bound. A pre-mutation or
out-of-order read cannot resurrect an old label. Metadata updates replace only
material projections, not the keyed manuscript/chat/evidence views. Pin receipts
must preserve source identity and carry a changed observed generation. A late
view removal carries its original object lease; it cannot remove a re-added row.
Unmount suppresses material publication, rereads, failures and focus restoration.
Removing a target during an in-flight rename releases the rename owner when it
settles. An exact successful generation already published by a read is acknowledged
without replay or rebasing. Pending IPC draft writes remain ahead of rename in
the existing session queue; no rename-induced checkpoint, draft clear or chat
submission is added.

## Concrete directory-move blockers

`workspaceTree.ts` synthesizes nested groups from document/material relative
paths. It does not provide a registered directory object or native move lease.
`loom-store/src/store.rs` owns a *single-document* `document_rename_operations`
journal, capture/anchor recovery, identity checks and namespace reservations.
It does not supply an atomic directory-prefix transaction spanning descendants,
active document authority, transient drafts/recovery and filesystem watchers.
Looping over document renames or calling `std::fs::rename` would not provide one.

Additionally, `materials.rs::binding_identity(source, workspace_path)` hashes the
placement path into a material ID. Rewriting descendant prefixes would change
IDs or violate the existing identity check. A correct directory owner must first
separate stable source/placement identity from mutable directory location without
silently retargeting evidence, retained consult references or current-schema
records. An alias database or display-only folder label would conceal, not solve,
this boundary.

The selected archive omits the actual store `schema.sql`, root Cargo.lock,
dependency/workspace members and CI integration sources. No guessed schema,
legacy reader, CI digest or unverified directory journal is supplied. A complete
directory change therefore still needs a concrete native owner and coordinated
store/material/watch/recovery transaction, plus no-replace collision, containment,
symlink, crash and stale-descendant tests on the exact native artifact.

## Regression evidence and qualification

Run `test:sidebar` to execute two deliberately separate evidence classes:
64 production-handler/module cases and three structural command/ACL consistency
cases. The handler harness executes actual extracted Svelte handlers and actual
TypeScript modules, with injected transport/DOM/scheduling; it does not compile
or mount a Svelte component. The structural suite does not run the Tauri generator.
Both are included before the unchanged Vitest command in the package test script.

This continuation adds 14 authored native cases across metadata I/O, binding
mutation, live-session dispatch and retained consult admission. They cover
same-byte replacement, in-place edits, staged-file substitution, failure,
symlink/parent swaps, hardlinks, stale pin/remove/rename, remove/re-add, duplicate
labels, full-ID source/evidence preservation, current-schema rejection, exact
transient draft preservation and reopen/session replay rejection. They were
**not compiled or executed** in the supplied environment.

Three new real-App WebKit cases are authored for placed/unplaced material rename,
exact dirty manuscript and pending chat preservation, plus IME/blur/Escape. They
await the initial editor tick and animation-frame caret lifecycle. These and the
existing browser/undo/streaming cases were **not run here**. Native transport
fixtures, including exact-byte mock draft receipts, are not native acceptance.

Execution used Node 22.16.0 and available TypeScript 5.8.3, not the pinned
TypeScript 6.0.3. Cargo/Rust/rustfmt/Clippy, pinned pnpm/Svelte/Vitest/WebKit,
complete workspace inputs and a signed macOS executable were unavailable.
Syntax-only transpilation is not full type-checking; the attempted strict check
was blocked by absent ProseMirror modules. No consolidated gate, native race
suite, durability/power-loss experiment or signed quit/relaunch journey passed
on this candidate. The external handoff preserves exact commands and raw logs.

Before qualification, apply to the exact clean full checkout, run the existing
pinned generators and formatting, review any resulting integration-only deltas,
and run focused native, Svelte, unit and WebKit checks. Freeze the settled result
in a local commit; serialize Cargo and run ONE unchanged-tree local-ci gate with
a fresh evidence directory under CONTRIBUTING. Then run exact signed material,
document/recovery, retained consult, pending-chat, clipboard/IME/undo and
Visual/Source/Ghost/Loompad journeys, including owned-worker quit and relaunch.
No release, promotion, hosted CI or cross-platform gate is waived by this patch.
