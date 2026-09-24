# Integration map, 24 September 2026

This is a live-source review of the pending integration, not a product acceptance
receipt. Historical handoffs retain their original observations. Current work
starts from PR #77, not from those handoffs' older supervisor commits.

## Source and ownership

Repository: `https://github.com/delysis/native-platform`.
Native-kit lives in `crates/native`; the standalone repositories are provenance.

- Fetched main: `87939f3873977ef2670ddab9e869ca9d5d7d1739`.
- Reviewed #77: `96e00be4c6b4ada0f70073a723c3520d95e64d74`.
- Reviewed tree: `9d90dcd9709a151226efcd371d5914df99f90c92`.
- Distance: 45 commits ahead, zero behind main; 61 changed files.
- Local integration branch: `codex/local-integration-20260924`.

The main checkout contains uncommitted EASL work at a detached commit. It is not
the integration checkout. Neither its changes nor the separately preserved EASL
stash are inputs to this integration. One local integrator owns builds and the
landing ref; authors use separate worktrees or exact-base patches.

## Recent PR disposition

Counts below were fetched together. They are main-only / branch-only commits,
not measures of quality or remaining implementation effort. Re-fetch before
changing a remote ref.

| PR | Head | Distance | Disposition |
|---|---|---|---|
| #77 | `96e00be4` | 0 / 45 | Current non-EASL integration candidate. Continue here. |
| #75 | `2fb08be8` | 0 / 1 | Ancestor of #77; its original one-word premise was superseded by the current full-preview contract. No reapplication. |
| #76 | `84de38dc` | 0 / 3 | Compatible streaming and test changes already reconciled in #77. The four files changed by its last implementation/test commit have identical blobs in #77. No wholesale merge over newer observer work. |
| #78 | `49ec511d` | closed | Audit stack is an ancestor of #77 through its integration merge. |
| #73 | `54b95533` | 36 / 50 | Separate EASL frontend, roughly 1,442 changed files. Needs its own rebase/integration and native qualification. |
| #81 | `3fd6d84f` | 36 / 45 | Context-only EASL lockfile preparation. Its own description says not to run or merge it. Do not treat it as an implementation candidate. |
| #49 | `a7daf722` | 36 / 76 | Separate cabals/Signal/peer-compute feature stack. Preserve, then integrate independently after the current baseline. |
| #63 | `cdd47396` | 36 / 87 | Stacked on #49; peer shutdown and receipt hardening. Review as a delta to #49, not as another independent merge. |
| #65 | `7796c221` | 36 / 119 | Stacked on #63; bounded static peer batching. No local Cargo evidence was claimed in its description. |
| #59 | `d95b4c1d` | 36 / 47 | Separate connected-material/source-navigation feature with its own acceptance history. |
| #54 | `e8a23937` | 78 / 6 | Separate native residual-training research API and upstream binding dependency. Requires explicit model/evaluation qualification. |
| #47 | `a4323162` | 88 / 30 | Older product convergence/private-storage branch. Reconcile behavior against current products before selecting any delta. |

No open feature branch is certified by this inventory. In particular, stacking
counts include prerequisite work. These branches must not be accumulated into a
single giant merge just to make the PR list shorter. #75/#76 can be closed as
superseded when #77 is promoted; closing them is housekeeping, not qualification.

## What is actually pending

There is no `crates/native` source delta from main to #77. Main already contains
cache identity (#64), executor lifetime/cancellation (#68), cache linearization
(#69), and sampling compatibility (#70). The pending Rust changes are Loom's
acceptance WebKit store ownership, Mom's create-only installation-key arbitration,
FTE's private startup preparation, and a Speech supervisor regression that now
uses a real joined worker.

The Loom delta repairs full streaming previews, immutable-prefix continuation,
hydration before retries, checkpoint identity before admission, exact rendered
text observations, and shortcut caret/focus preservation. The native driver has
accumulated many small repairs; browser tests do not establish that those repairs
compose successfully in the packaged application.

Review the small current production modules before extracting more abstractions.
Keep runtime authority in the owning product and native request ownership in
native-kit. Do not introduce compatibility layers for the unreleased drafts.
For future App/editor decomposition, move one tested behavior boundary at a
time; a large structural rewrite would otherwise erase the failure's context.

## Local validation and promotion

GitHub runs `35937028966` and `35937028888` did not execute repository steps.
The account's billing limit is an infrastructure failure, not a source failure.
The user's local-only validation instruction supersedes the older PR text's
requirement to wait for paid Actions. Never manufacture a successful hosted
check or claim Linux/Windows qualification from a Mac run.

Retain a source-bound local component receipt and the separate native receipts.
Native Loom acceptance still requires exact generated/rendered bytes, full
streaming previews, stable four-choice mapping, acceptance/unconsume/undo,
stale-scope rejection, worker shutdown and persisted relaunch. A/B/A must use the
same delivered artifact and distinct persistent stores. Mom/FTE startup and
storage changes require their own exercised native paths. A failing journey is
an actionable product or harness defect, not a reason to rerun every portable
check unchanged or weaken the acceptance predicate.

Before promotion, compare the tested and landing trees. Preserve the original
failed attempts and all document/SQLite/WAL state. The receipt, not a stale root
handoff or a remembered test count, determines what has passed.
