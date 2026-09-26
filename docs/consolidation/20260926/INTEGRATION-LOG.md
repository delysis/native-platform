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
- Correct the forked-Persona attachment fixture to remap its message owner as
  the production fork already does. Retain a negative witness that a foreign
  message cannot borrow the snapshot.
- Remove the test-only old context fitter. It did not exercise the new
  whole-message selector and left dead fields and imports in strict builds.
- Use the supplied checked-history adapter's applicable checks: reject invalid
  occurrence IDs, foreign conversation ownership and excessive retained text
  before selecting even an otherwise valid branch. Check unselected branches
  too, for direct chat and consultation. Keep the published typed history API
  and exact message metadata.

## Remaining product gaps

| Priority | Gap | Evidence needed before promotion |
| --- | --- | --- |
| 1 | The normal Loom executable still chooses one of two retained Tauri shells. Mom's scoped consult dispatcher is not yet owned by one shared Loom runtime or exposed through one simple chat/document surface. | One real `@expert` call from the normal Loom window with a cited attachment, genuine cache receipts, exact source reopen, cancellation and joined quit/relaunch under one owner. |
| 2 | Native media markers are appended at the final user turn. A source map retains historical occurrence identity, but the model does not receive an image at its original historical chat position. Media with bound tools remains explicitly blocked; audio is transcription-first and video/opaque media remain unsupported. | Typed media placement and tool continuation design, followed by exact per-target privacy and native model checks. Do not erase media to make a tool call work. |
| 3 | Attachment sources are selected and recorded before native generation, but the final conversation transaction does not revalidate all selected source occurrences against concurrent removal or replacement. The supplied `consult_sources.rs` sketches transactional retention but has not been compiled against this stack. | Integrate a transaction-bound source check and deletion impact rule, then test a source removal racing a consult commit. A retained source map alone is not citation validity. |
| 4 | New encrypted consult input/output receipt namespaces have no completed retention and Persona-removal impact decision. | Review removal, preservation and read authority using a real retained invocation. |
| 5 | The frozen 77-head ledger proves ancestry and recovers one history validator. It does not semantically qualify the 20 divergent heads or the one ahead head. | Compare each unique delta to current main and this stack; integrate useful changes by exact source and feature journey. Preserve original heads. |
| 6 | Generic EASL text/editing history is preserved but not extracted into a separate, dependency-closed research line. The Loom-specific EASL frontend remains parked. | Separate build and source/provenance review; no change to the production Tauri frontend during extraction. |
| 7 | The Gemma 4 Metal ZIP contains research shaders with a group-64 ABI that differs from the rvLLM campaign's current packed format; its short attention prototype uses contiguous rather than paged KV. | Port one role-specific candidate to current storage and paged-cache rules, compile, run guard-byte/correctness and checkpoint-quality gates, then controlled role and route timings. No default-route promotion from source inspection. |

## Qualification boundary

Focused source, Node, Rust and lint results belong to the exact repaired tree.
The clean-tree `xtask local-ci` receipt, hosted CI and packaged macOS/model/GUI
acceptance must be reported separately. A passing component gate does not
qualify the simplified Loom product, historical media semantics or a release.
