# Workspace sidebar and editing interaction contract

## Scope and provenance

This repair is for the selected source declared as
`3c5d00c50a496c7357fa9ad0da085b5cdad978d0` / tree
`32f868c7e841e5efdfcc85b42df0f9700f32eec0`. The supplied archive is not a complete
workspace or an independently authenticated Git history. All patch paths are
under `products/loom`; no native command, permission, store schema, credential,
model policy, completion controller or Mom bridge implementation is changed.

The owning root and Loom AGENTS, CONTRIBUTING, browser-development guide and
streaming-completion contract were inspected. Loom AGENTS also references
`docs/reviews/2026-09-22-hydration-admission.md`, which is absent from this intake.
The integrator must read that continuation before qualifying these input-surface
changes. The streaming contract and its exact native promotion boundary remain
unchanged.

## One shared interaction layer, distinct owners

`interactionPrimitives.ts` owns composition-aware inline rename and popup
keyboard/placement primitives. The proven document-rename composition guard and
menu helpers are moved, not reimplemented; their old exports remain available.
`sidebarInteractions.ts` owns a closed row/capability union and captured target
validation. Neither module grants filesystem authority. `App.svelte` remains the
interaction owner and dispatches to existing product-specific operations.

| Row | Application-defined menu | Authority |
| --- | --- | --- |
| Remembered root | Open or expand/collapse; Rename Sidebar Label; Copy Path; Remove from Sidebar | Exact-root local navigation bookmark only |
| Nested folder | Expand/collapse; Copy Path | Renderer grouping only; no filesystem mutation |
| Document | Open; Rename; Export Text; platform Reveal when available; Copy Path; Delete Manuscript | Existing captured project/session/document/revision/blob operations and confirmation |
| Material | Open; Pin/Unpin; Copy Reference; Copy Path when known; Remove Source from Workspace | Existing native session/material-ID registration operations; protected source removal disabled |

Menu capability generation and execution use the same live policy. A forged or
previously enabled action is rechecked at execution, including a second event
before Svelte's next render. Display paths are for labels/tooltips/clipboard;
no display path reaches a new native mutation API. Search expansion does not
pretend that a folder can be collapsed while its matches must remain visible.

## Bookmark lifecycle

A remembered root is not a filesystem handle, grant, deletion target or promise
that storage is currently online. Reading bookmarks makes no filesystem calls,
does not prune unavailable roots, and does not rewrite their representation.
The existing localStorage key, root/title shape and 32-entry bound remain.

Identity is the exact stored root spelling, never a title or normalized basename.
Legacy `title === root` values display a basename without rewriting storage.
Explicit labels survive reopening. Duplicate display names gain the shortest
useful ancestor suffix, with a deterministic disambiguator for remaining
collisions. Exact stored paths remain available as tooltips and Copy Path.

Remove from Sidebar persists only the filtered bookmark array. It does not open,
close, save, checkpoint, delete, rename, reattach or otherwise mutate a project.
A missing root can therefore be removed without first trying to open it.
Explicit storage failures leave rename/removal unpublished and visible as errors.

Forgetting the active bookmark leaves the live session, editor instances, dirty
manuscript, recovery state and unsent chat mounted. Its existing document/source
tree remains reachable as an unbookmarked live group, not a synthesized bookmark.
Startup and session reattachment do not silently recreate that bookmark. Only an
explicitly opened and restored workspace adds one. A successful native opening
with refused bookmark persistence retains its live tree and reports the refusal
after document navigation, so navigation cannot silently clear that error.
Opening an unavailable root still takes the existing native prepare-before-close
path; the current editor is not abandoned to satisfy a failed navigation hint.
An unresolved recovery conflict does not add a new convenience bookmark.

## Selection, rename, popup and async lifecycle

Single-click selects; disclosure toggles; double-click, Space or Mod-Down/Mod-O
opens where supported. Return/F2 starts rename only for a row with that
capability. Arrow keys, Home/End, Delete or Mod-Backspace, ContextMenu and
Shift-F10 operate on the selected row. This is single-selection, not a Finder
clone, multi-selection implementation or batch filesystem operation interface.

Every handled row suppresses the default WebKit context menu. Text inputs keep
their native text-editing menu. Inline rename selects its text and shares the
existing Return/Escape/blur and IME (including legacy keyCode 229) rules. Blur
during composition defers commit until composition ends. Root actions explicitly
say Sidebar Label; they never imply that an external folder was moved.

Targets capture project and session. Document targets retain their existing
revision/blob identity. Root and material object leases reject remove/re-add or
replacement ABA; material snapshots also reject changed fields. Popup DOM is
keyed by the captured target, so a queued event from an old popup cannot become
an action on a new popup's row. Context actions, focus restoration, rename focus,
and async material receipts recheck their captured scope. A replaced material
row triggers refresh rather than deletion of a new projection with the same ID.
Native operations are serialized through the existing busy boundary.

Selection is independent of the open manuscript. A selected row disappearing
through search/collapse is reconciled after rendering without stealing focus.
The active, forgotten tree retains its real copy destination. An inactive root
being label-edited still has a root wrapper and no active copy destination, so a
drop onto its text field cannot fall through to copying into the active project.

Document rename/delete still use the existing captured-owner path: flush and
reacquire the current revision where required, locks, confirmation, immutable
command identity, receipt validation and uncertain-operation retry. This repair
does not replace those rules with the sidebar's UI lease.

## Editing ownership, not editor replacement

`textEditingInteractions.ts` centralizes composition ownership, native editing
chords and native-versus-ProseMirror history routing. Native input/textarea forms,
chat, source text and rich text remain distinct surfaces. Global chrome does not
reinterpret copy/cut/paste/select-all/undo/redo chords from text controls, or keys
owned by a composing control. Rich document/context editors retain their schema,
clipboard behavior, formatting and ProseMirror transaction/history owner.

Concrete repairs:

- Terminal selected Ctrl-C remains copy. Only collapsed-selection Ctrl-C during
  active work interrupts. Modified or selected-text arrow keys no longer replace
  input with command history; IME/229 keys do not submit or open global controls.
- Chat IME/229 and Alt-Return do not submit. The original WorkspacePane layout,
  composer and chat implementation remain intact.
- Source ordinary Tab/Shift-Tab uses a native insertText edit transaction rather
  than direct value/setRangeText mutation. It checks exact resulting bytes,
  preserves surrogate boundaries, and sends exactly one input notification.
  Unsupported/refused native edits do not force a second undo stack or direct
  value fallback. Actual WebKit grouping and platform behavior must be tested.
- Rich beforeinput historyUndo/historyRedo routes to ProseMirror's own history,
  including preventing unrelated native contenteditable history when read-only
  or when the rich history is empty. Native textarea history remains native.

This is not a claim that every programmatic insertion across the product now
shares one native undo mechanism. Completion acceptance/unconsume, import,
dictation and other existing programmatic insertion owners are not refactored.
Unconsume remains distinct from ordinary undo under the streaming contract.
Platform clipboard, native context-menu presentation and IME ordering require
real WebKit and signed application qualification; synthetic events are not proof.

## Explicitly staged filesystem boundaries

There is no safe general folder rename/delete API in this selected product, and
no material filesystem-rename API. Their menus do not offer those operations.
Material removal uses the existing native material registration/grant removal;
it is not deletion of original external source bytes or retained evidence.
Document deletion remains the existing specifically authorized manuscript action.

A future folder/material filesystem operation needs a separate native proposal:
opaque registered target bound to the active session, expected identity and
revision/metadata checks, native canonical containment and symlink/descendant
validation, collision handling, recoverable commit/receipt semantics, explicit
impact on descendants/open documents, and native race/failure tests. Do not add a
generic renderer-supplied path deletion function or mutate immutable histories.
External root label rename must remain separate from that native authority.

## Regression evidence and qualification

`tests/sidebar-interactions.node.mjs` transpiles actual production modules and
extracts actual named App/Terminal/WorkspacePane/SourceEditor handlers with the
TypeScript AST. Injected state, transport, focus objects and scheduling make the
boundary explicit: it does not compile or mount Svelte or run a filesystem.
Run it through `pnpm --filter @delysis/loom test:sidebar`; it is also included
before Vitest in the package's existing test command.

The focused execution has 37 passing cases, including four mutation negative
controls: old active-bookmark resurrection, forced path labels, removed forgetting
in the real App handler, and selected Ctrl-C theft in the real Terminal handler.
Coverage includes missing-root zero native mutation, dirty active-session
preservation, identity/label collisions, storage refusal, stale menu/scope/ABA,
revision delegation, keyboard/IME rename, focus, capability rejection, material
await races, native-vs-rich undo ownership and single input notification.
These finite tests are regression evidence, not universal proof.

`appSidebar.browser.test.ts` adds eight real-App cases with only native transport
injected. Additional cases exercise actual editor, terminal and chat components.
The existing identity, recovery and streaming regressions are retained; obsolete
fixed-index/document-only menu assertions are adjusted to the typed shared menu.
The browser tests are authored but NOT executed in the supplied Linux environment.

Available here: Node 22.16.0 and global TypeScript 5.8.3. Three pure interaction
modules strictly type-check; changed TS/Svelte scripts pass syntax transpilation
and balanced-block sanity checks only. Full sidebar type-checking is blocked by
missing ProseMirror dependencies. Svelte/Vitest/Playwright, Cargo and pnpm are
absent, and the bounded package-registry check failed EAI_AGAIN. The source pins
TypeScript 6.0.3; results from the available compiler do not replace pinned checks.

Before integration: read the missing hydration continuation, inspect the patch,
apply to the exact clean full checkout, then run pinned focused Node, Svelte,
Vitest and WebKit checks. Commit the reviewed result and run ONE unchanged-tree
`cargo run --locked -p xtask -- local-ci <new-evidence-directory>` gate with the
CONTRIBUTING toolchain/cache rules. Signed native validation remains separate:
missing/offline bookmarks; dirty document/chat preservation; real Finder/WebKit
keyboard/context/clipboard/IME/undo; source drop targeting; Visual/Source/Ghost/
Loompad streaming, acceptance/unconsume, joined shutdown and relaunch. Do not
substitute these local handler tests for that journey or delete failed evidence.

The separately archived obsolete v3 sidecar and Keychain repair are out of scope.
No reader, migration, credential repair, silent cleanup or schema relaxation is
introduced by this patch.
