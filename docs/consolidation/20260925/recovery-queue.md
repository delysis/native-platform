# Original remote heads: accounted for, not blindly merged

This review is pinned to main `637e60b6b044230ed24ed3118615a2e5538cae83`
and the 77 original heads retained in `../20260924/branches.csv`.
`head-review.json` records each exact head, its comparison, the source of that
comparison, and its disposition. No original head or main was moved or deleted.
New consolidation and temporary source-assembly branches are not falsely
presented as original feature branches.

## Ancestry result

- 55 original heads are ancestors of the pinned main; they have no unique
  commits to merge. This does not prove that every historical feature survived
  later edits or remains exposed in the product.
- 20 heads diverge and one (#85 Automatic V3) is ahead. These need semantic
  comparison and qualification, not automatic merge approval.
- The remaining head is the pinned main itself. No ancestry relation remains
  unknown within this frozen original inventory.

The ledger attributes 46 exact comparisons to the supplied audit, 28 to this
implementation pass, and three to earlier exact comparisons in the conversation.
Previously unknown comparisons were not filled by guessing from branch names.
The live branches were also inspected; this ledger remains a dated review,
not a claim to cover arbitrary future branch creation or movement.

## Work actually recovered

PR #103 recovered `workspace-document::BranchIndex` from #47's exact head
`a43231626f5deb4ad8f6f08beb36dca40a236270`, preserving source blob
`d3c20b65b1e1329f2bd6d82fece7de2324b6c168` and its three original tests.
The actual consult path consumes the checked branch adapter in #105.

This is selective recovery, not a merge of #47's older SQLite pins, encryption
codec, application UI or configuration redesign. Its remaining document,
sampling and vault work stays visible in the recovery queue.

## Remaining semantic queues

| Work | Disposition and acceptance boundary |
| --- | --- |
| #47 Mom/Loom convergence | Reuse compatible shared types, selection and policy modules; rebase storage and application ownership explicitly. Do not flatten typed conversation history into a Markdown transcript. |
| Connected collections and material references | Compare exact immutable source/page/representation and draft-isolation deltas against the new shared grammar and media identity. Preserve missing/ambiguous/partial evidence. A large file-stat list is not complete code review. |
| Cabals/Signal and three peer branches | Review unique admission, batching and provenance deltas in dependency order. Keep them off by default; no implicit listener, remote disclosure or trust promotion. |
| Four EASL heads | Retain generic rendering, shaping, text/editing and history in a separate research line; park the Loom-specific frontend. No dependency-closed extraction or native Tauri replacement is claimed complete. |
| Native hidden-state training | Keep as opt-in research with its own data, runtime and weight evidence. Opening a document does not authorize training. |
| #85 and older acceptance alternatives | Qualify #85 separately. Prefer current #86 controller authority over old broad replacements. Do not resurrect numeric-only rejection or relax idle/focus/byte-correlation gates. |

The remaining small documentation, raw-output and streaming/focus heads have
explicit ledger families as well. Nothing is silently discarded because the
newer main has overlapping files.

## Executable evidence checks

```sh
node scripts/consolidation/verify-head-review.mjs
node --test scripts/ci/consolidation-head-review.test.mjs
```

The existing consolidation CI entry imports the new test suite. No workflow,
action pin, permission, acceptance condition or package/dependency is added.
The verifier checks coverage, pins, numeric relation consistency and declared
provenance. It does not make Git requests, mutate refs, or certify the semantic
correctness of recorded comparisons. The existing `inventory-heads.mjs` remains
the tool for a fresh local Git inventory rather than a duplicated second census.

Executed here: Node 22.16.0, 16/16 tests passing, no skipped tests, plus the actual
bounded verifier command. Negative controls cover missing/duplicate/moved heads,
unknown relations, false original-audit attribution, malformed CSV, wrong pins,
and attempted automatic merge/deletion or semantic approval.

Historical recovery identity is an import record, not a perpetual byte freeze
on the current history module. Later reviewed fixes can change its source
without forging the original import record.

Rust compilation, Rust tests, rustfmt, Clippy, the full existing CI suite,
packaged application, model, native UI and Keychain qualification were not run
in this authoring runtime. The common normal-Loom frontend/runtime cutover and
full semantic integration of these divergent heads remain unfinished.
