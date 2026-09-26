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
| 3 | Attachment sources are selected and recorded before native generation, but the final conversation transaction does not revalidate all selected source occurrences against concurrent removal or replacement. The supplied `consult_sources.rs` sketches transactional retention but has not been compiled against this stack. | Integrate a transaction-bound source check and deletion impact rule, then test a source removal racing a consult commit. A retained source map alone is not citation validity. |
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
committed; the existing `commit_generated_exchange_with_journal` call path is
the integration point to review. Only after this journey works should one
sidebar select between document and chat identities. Deleting the old Mom
renderer remains gated on the full command and packaged-native journey ledger.

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
