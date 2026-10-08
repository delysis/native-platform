# Consolidation integration log — 2026-09-26

Source review base: `e03d52188204a9d44478a13355746c0b843eb07c` (tree
`32a3ee4904f9ad359785aac9fe7ecac17044ebd0`). The 16 dependent draft
commits end at PR #108. They remain based on main
`637e60b6b044230ed24ed3118615a2e5538cae83`. The supplied ZIPs, loose
source files and chat transcript are review inputs; the Git commit graph is the
implementation source. None of those inputs grants merge or release authority.

## Local repair disposition

- Format the authored Rust with the repository's pinned Rust 1.92 toolchain.
- Correct the owned-asset planner's disposable-repository fixture on macOS:
  `/var` and `/private/var` must resolve to the same root. Keep the new
  dependency-derived Loom and macOS selection for Mom's embedded assets.
- Keep the exact Mom storage release check after its move into the shared
  Mom/Loom helper, and load mention target resolution as a normal Rust module
  so the ignored-test source guard can inspect it.
- Correct the forked-Persona attachment fixture to remap its message owner as
  the production fork already does. Retain a negative witness that a foreign
  message cannot borrow the snapshot.
- Remove the test-only old context fitter. It did not exercise the new
  whole-message selector and left dead fields and imports in strict builds.
- Use the supplied checked-history adapter's applicable checks: reject invalid
  occurrence IDs, foreign conversation ownership and excessive retained text
  before selecting even an otherwise valid branch. Check unselected branches
  too, for direct chat and consultation. Reject an invalid new chat ID before
  it can create a stored conversation. Keep the published typed history API and
  exact message metadata.

## Remaining product gaps

| Priority | Gap | Evidence needed before promotion |
| --- | --- | --- |
| 1 | The normal Loom executable still chooses one of two retained Tauri shells. Mom's scoped consult dispatcher is not yet owned by one shared Loom runtime or exposed through one simple chat/document surface. | One real `@expert` call from the normal Loom window with a cited attachment, genuine cache receipts, exact source reopen, cancellation and joined quit/relaunch under one owner. |
| 2 | Native media markers are appended at the final user turn. A source map retains historical occurrence identity, but the model does not receive an image at its original historical chat position. Media with bound tools remains explicitly blocked; audio is transcription-first and video/opaque media remain unsupported. | Typed media placement and tool continuation design, followed by exact per-target privacy and native model checks. Do not erase media to make a tool call work. |
| 3 | A candidate check revalidates each selected attachment occurrence's conversation/message ownership, committed state, root, canonical manifest/text identity and current policy inside the final conversation/journal transaction. Deterministic unchanged, staged-current, removal and same-ID replacement tests exercise that boundary. A second candidate guard rejects a deletion built from a conversation snapshot older than a committed consultation, rather than overwriting its reply. This is not yet a real cited native consultation. | Qualify both guards through the exact clean-tree gate and a real consultation. Decide how a later source deletion affects retained citations before treating them as durable. |
| 4 | New encrypted consult input/output receipt namespaces have no completed retention and Persona-removal impact decision. | Review removal, preservation and read authority using a real retained invocation. |
| 5 | The frozen 77-head ledger proves ancestry and recovers one history validator. It does not semantically qualify the 20 divergent heads or the one ahead head. | Compare each unique delta to current main and this stack; integrate useful changes by exact source and feature journey. Preserve original heads. |
| 6 | The generic EASL service and its research history now live in the separate private `delysis/easl` repository. It is not yet a qualified Tauri view replacement, and the Loom-specific EASL frontend remains parked. | Keep EASL research and native accessibility qualification in that repository. Do not restore EASL to the production Loom frontend during chat/document consolidation. |
| 7 | The Gemma 4 Metal ZIP contains research shaders with a group-64 ABI that differs from the rvLLM campaign's current packed format; its short attention prototype uses contiguous rather than paged KV. | Port one role-specific candidate to current storage and paged-cache rules, compile, run guard-byte/correctness and checkpoint-quality gates, then controlled role and route timings. No default-route promotion from source inspection. |

## Single-surface cutover target

The intended Loom product is one normal Tauri window with one sidebar for
documents and encrypted chats, and one central work area that opens the selected
chat or notepad. `loom-app --mode document|chat` is a transitional shell selector,
not this product. Loom's current `WorkspacePane` chat path uses FTE, while the
scoped Mom consultation commands, encrypted conversation store and shutdown
owner remain in the separate Mom shell. Combining their controls without
reconciling native model ownership would merely keep two frontends inside one
process.

The elimination receipt must identify source actually deleted after parity.
At this tree, Mom's `view.rs` plus its `ui/*.js`, `ui/*.css` and `ui/*.html`
total 11,205 physical lines. They are a presentation-removal target, not a
current saving or a promise that every line can vanish. The old Mom executable,
the `desktop-launch` shell selector and any duplicate Loom chat renderer need
an explicit disposition too. Preserve Mom's typed chat, citation, tool,
approval, cache, store and cancellation behavior while replacing presentation;
preserve Loom's exact UTF-8 document and completion behavior. Only a real
one-window journey with the existing encrypted identity, a cited `@expert`
consult, cancellation and joined quit/relaunch can authorize old-shell removal.

### First implementation slice

The source-bound consultation design returned for the pre-log code commit
`6eaa13d8bb65ecad09461e7795744dedaa96df99` agrees with the current
code: the launcher enters a different Tauri root for chat, `WorkspacePane`
builds `User:`/`Assistant:` terminal prompts from completed document runs,
and Mom's `mom_llama_chat_dispatch` wrapper owns scoped admission, stream
events and approval observation. The current head changes documentation but
not those paths. This is a reviewed design input, not a compiled patch or
native acceptance receipt.

Make one real Mom consultation work from the normal Loom window before
combining sidebar rows. Extract one application-owned native host and shared
admission/final-exit authority; retain Mom's encrypted store, prefix-cache
policy and command wrapper, and Loom's document runtime contract. The embedded
adapters must not each construct or finalize a host. Preserve model residency
and joined worker evidence across both product domains. Then add a small chat
work area using the existing dispatch command, stream and cancellation
identities. Switching work areas must not redirect an in-flight result.

The first native journey must reopen an existing encrypted conversation, run a
real `@expert` consultation with a supported text attachment and genuine cache
receipt, reopen the exact cited source, cancel a second attempt, and quit and
relaunch with joined ownership. A selected attachment occurrence must be
revalidated in the final conversation transaction before its citation can be
committed. The candidate guard in `commit_generated_exchange_with_journal`
checks the prepared source against authoritative documents in that transaction;
the deletion writer also rejects a stale pre-mutation snapshot. Both still need
the real cited journey and later-deletion policy. Only after this
journey works should one sidebar select between document and chat identities.
Deleting the old Mom renderer remains gated on the full command and packaged
native journey ledger.

EASL view research now lives in the private `delysis/easl` repository at
`2369e0b18e4cdeb9d69b86725f81eefb7e07caf0`. Its filtered service history,
working accessibility snapshot and distinct research heads are retained there.
The current consolidation tree and `origin/main` already omit the EASL service
directory. The old detached checkout's uncommitted EASL work remains untouched;
the separate repository is not yet a qualified Tauri WebView replacement.

## Qualification boundary

Focused source, Node, Rust and lint results belong to the exact repaired tree.
The clean-tree `xtask local-ci` receipt, hosted CI and packaged macOS/model/GUI
acceptance must be reported separately. A passing component gate does not
qualify the simplified Loom product, historical media semantics or a release.

## 2026-10-08 Mom review: consultation settings and Keychain

Three retained Gabor consultation failures in the review profile report
`Host-level native settings changed; restart Mom Llama to apply them.` No
model engine was invoked. Typed settings defaulted to four slots while their
UI projection defaulted to one; an unrelated display update synchronized the
projection into native settings and invalidated the bound host identity.
Resolution now derives that projection from the typed value, and both defaults
agree. Explicit slot changes still apply and retain the host restart guard.
Failed consult dispatch exposes each retained target cause. A read-only CLI
`mention history` command returns the latest eight attempts for an exact host
without executing work or returning frozen tool continuations.

Fresh-store Keychain initialization now attempts create-only insertion directly
instead of first requesting a password lookup. Duplicate insertion recovers
an existing credential; it never replaces that credential. Existing databases
retain the single lookup path. The process cache serializes concurrent startup
lookups. A separate diagnostic CLI and distinct review profiles can independently
request OS access; they must not be counted as one app launch.

Validation: 246 runtime tests, 75 app tests, and 10 CLI integration tests pass
(one CLI test ignored). The macOS app build and bundle signature verification
pass. The preceding test run failed because a new test accidentally consumed an
existing platform attribute; the attribute placement was repaired and the full
runtime suite rerun. Logs are retained in the Oct 8 Mama review evidence folder.

Remaining native qualification: the rebuilt app is waiting for authorization to
its existing encrypted review store. Computer Use explicitly refuses Apple
SecurityAgent, so the user must operate that dialog. Neither the number of OS
dialogs on a completely fresh installation nor a real post-repair persona reply
is qualified by unit tests. These fixes are review-branch work, not main or
release acceptance. No additional interface layout changes were made.

### Keychain review correction

The user observed two permission dialogs in the rebuilt 81e3095b app, including
an existing-key access dialog. This falsifies prompt-count acceptance of the
fresh-store optimization. The store cache tests prove application arbitration,
not macOS dialog behavior.

The follow-up reads through the safe macOS `find_generic_password` wrapper,
which invokes `SecKeychainFindGenericPassword` once and returns password and item
together. This uses the same file-based Keychain family as create-only insertion;
the service, account, credential contents and authorization policy are retained.
No credential overwrite, ACL change or automatic permission approval is added.
Optional `LLAMA_NATIVE_KIT_KEYCHAIN_TRACE=1` logs PID, opaque account prefix,
operation and begin/end boundaries. It excludes passwords, keys and paths.
This distinguishes duplicate app requests from multiple dialogs within one OS
request. 246 runtime tests pass; real dialog-count acceptance remains pending.

The user also observed two prompts with the direct lookup build. Its native trace
contains exactly one read begin/end pair for one PID/account, followed by runtime
ready; changing the lookup API did not establish the requested fix. The delivered
review bundle was ad hoc signed. It has now been signed and strictly verified
with the available Apple Development identity, without changing store contents
or ACLs. Candidate packaging already supports `DELYSIS_SIGNING_IDENTITY`;
`docs/releases/macos.md` now specifies this for Keychain review. Native prompt
count for the signed build is pending user observation. Do not claim acceptance
from the single request trace alone.

The signed build also produced both dialogs against the existing review
credential: confidential-information authorization, then key authorization.
Stable signing alone does not repair that credential. No access control bypass
was attempted. The unsuccessful legacy lookup API experiment is removed;
the modern SecItem lookup and request tracing remain.

With the user's existing-data-disposable authorization, the old review directory
is preserved and a fresh encrypted review profile was created by the signed app
at `review-data-signed-fresh` in the Oct 8 evidence folder. Its first launch
recorded one create request and reached the visible interface with no permission
dialog. Reopen recorded one read request and also reached the visible interface
with no permission dialog. This qualifies those two native journeys, not the
old credential: its double authorization remains an unresolved legacy-store
limitation. Do not describe a data reset as a general existing-store repair.

### 2026-10-08 existing-credential root cause and repair

The macOS `securityd` log identifies two checks for the exact old Mom credential:
ordinary item ACL authorization (action 24) trusts the original ad hoc Loom
bundle's designated code hash `fb823aa99c83bb6858969416a463610877d7dcd7`; partition
integrity authorization (action 65538) rejects both later ad hoc hashes and the
Apple-signed Mom team. This is the direct cause of the two distinct screenshots,
not a second Rust key-provider call. The read/creation API experiments and a new
profile do not repair that old partition.

Boom's `App/Sources/Boom/Persistence.swift` uses one locked process session,
SecItem generic-password queries and creation, with WhenUnlockedThisDeviceOnly.
Loom's connected imports use the native keyring store. Neither provides a
special technique that bypasses partition integrity. Mom already has the
serialized process cache. Stable signing prevents new credentials being tied
to changing ad hoc hashes, but existing ad hoc partitions require a targeted
credential metadata repair.

`scripts/repair-mom-keychain.sh` validates the exact signed Mom bundle, stable
TeamIdentifier and existing database, selects one existing credential by the
runtime's exact data-directory hash, and applies Apple's partition-list repair.
It keeps trusted-application ACLs and key bytes intact. There is no broad
keychain mutation, credential read, replacement or deletion. Check mode passed
on the original encrypted review store; shell syntax passed. The actual repair
requires the user to enter the Keychain password interactively, as mandated by
Apple's tool. Computer Use refuses both Terminal and SecurityAgent. A prepared
local `Mom-Keychain-Repair.command` captures the repair status and before/after
ciphertext database hashes. Native acceptance remains pending that authorization
and the subsequent old-store dialog-count check; do not mark the goal complete.

### 2026-10-08 narrow native sidebar layout

The max-width 900px rule reset chat's left edge to zero while retaining the
visible fixed sidebar, placing the composer beneath it. Removed that reset and
the large sidebar overlay shadow. An open sidebar now reserves its width at
all supported window widths, and the toolbar uses the same sidebar background
up to the pane boundary. When narrow settings suppress the sidebar, the toolbar
also returns to the chat background. Removed a redundant desktop sidebar rule.
The app rebuild and strict signed-bundle verification pass; 25 frontend tests
pass. The live native window at its 640px minimum shows a fully visible composer
beside the sidebar and continuous sidebar color through the title bar. A wider
collapsed-sidebar view was also observed. A subsequent resize attempt was
interrupted by a user window change; no additional resize acceptance is claimed.

### 2026-10-08 title-bar alignment

The 38pt web toolbar centered controls at 19pt while the native window buttons
were centered at 16pt. Added 6pt bottom padding to that toolbar only in windowed
macOS mode, putting its controls on the observed 16pt axis and retaining the
pane edge. Fullscreen retains the existing centering. Rebuilt and strictly
verified the signed review app; the native screenshot shows sidebar and settings
icon centers aligned with the stoplights. The user's four-message conversation
reopened intact. No model work or additional layout changes were required.


## 2026-10-08 — One unsent chat draft and sidebar controls

- Every GUI New Chat or Persona start uses the same encrypted unsent draft.
  Navigation flushes text and attachments; restarting retains the draft and
  its selected Persona. First submission creates the chat and transfers staged
  attachment ownership in one store transaction. Empty/repeated submissions
  cannot create additional records. Existing saved history remains intact.
- Persona rows show full names and short primary handles. Catalog-managed
  handles use first names, with first-last for collisions; customized profiles
  retain their handles. Titlebar New Chat/Search replace sidebar navigation.
  Sidebar width can be dragged or changed with arrow keys on its separator.
- macOS native contextual menus provide Persona Chat/Edit/Remove and saved-chat
  Delete/Save as Persona. Search results preserve object kind so Persona actions
  cannot bypass the guarded removal flow. Branch replaces per-message freezing;
  saving a Persona binds the dialog to the target conversation.
- Focused results before consolidated gate: 248 runtime tests, 75 app tests,
  29 frontend tests passed. New storage regression exercises navigation,
  restart-bound data, simultaneous submissions, attachment transfer and immutable
  template preservation. Native review verified unsent text across repeated
  New Chat, Persona selection, saved-chat navigation and process restart; actual
  native menus and draggable width; first Persona submission produced one chat
  and a real local GGUF response `OK`.
- Review receipts are under the existing mama-llama-review-20261008T132539Z
  evidence directory. This slice does not qualify the old credential repair,
  hosted CI, main promotion or the full cross-platform/model release journey.

- The first consolidated gate passed workspace tests and doctests, then rejected
  unnecessary references and assertion `unwrap()` calls under strict Clippy.
  These were repaired without changing behavior; a pre-existing mention-error
  test also now uses `std::slice::from_ref`. Focused Clippy for Mom runtime, app
  and CLI passes. The first gate remains preserved at mom-single-draft-20261008/
  local-ci; the final unchanged-tree gate uses local-ci-final.


## 2026-10-08 — Global RAM planning and chat naming

- Removed editable Persona history/context limits and tool bindings. Ordinary
  invited responses use global context policy and cannot offer Persona MCP tools.
  Immutable historical profile fields remain provenance only; explicit global
  MCP and existing journal recovery remain separate backend capabilities.
- Model/projector weights must fit within half of physical RAM. Global context
  is derived from supported GGUF attention metadata and declared capacity, with
  weights, KV, workspace, sequence metadata and prefix copies budgeted inside
  two thirds of RAM. Manual legacy budgets are clamped to that ceiling. Unknown
  recurrent/MLA layouts fail admission rather than relying on a heuristic.
  Gemma 4 key/value metadata was checked against the pinned llama.cpp source;
  sliding-window/shared-KV savings are deliberately ignored conservatively.
  These are admission estimates, not hard RSS limits or all-hardware acceptance.
- Titles derive from the first user message until renamed; sidebar counts are
  removed. Selected-name click opens Rename, also available in native menus.
- Native inspection on the signed review profile verified no Persona context/tool
  controls, selected-name rename and native Rename/Delete/Save as Persona menu.
  The open Robert Miller profile edits were retained across update and saved.
  This exposed and repaired the frontend's invalid ChatTemplatePolicy JSON shape;
  its two supported tagged forms now have a frontend regression test.
  A real Gemma 4 request with the automatic plan returned `OK`.
- Focused receipts: runtime 251 passed, app 75 passed, frontend 30 passed,
  engine 157 passed/10 opt-in ignored, strict focused Clippy passed. An explicit
  real-GGUF metadata estimate test passed without loading tensors. Receipts:
  mom-memory-policy-20261008. The consolidated settled-tree gate is recorded
  separately there. Compact native review, old credential prompt repair and
  full release/model journey remain unqualified; no main promotion claimed.
