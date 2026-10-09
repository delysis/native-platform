# Branch inventory and promotion policy

The supplied audit enumerated 77 branch heads, 84 PR records (10 open), 69
feature rows, 110 Mom application commands, and the attachment/service/native
boundaries. Those inventories are retained in `inventory/` with their original
provenance. They are an inventory, not a claim that every divergent line of
code has been independently examined in this pass.

Three previously unmeasured heads were checked here: `codex/w8-sqlite-prep`
(0 ahead, 429 behind), `codex/w9-lean` (0 ahead, 416 behind), and
`codex/production-release-gates` (0 ahead, 338 behind) are ancestors of the
pinned main. Re-merging them supplies no new commits. History may still contain
useful code later removed, so "ancestor" alone does not prove feature survival.

## Dispositions

| Family | Disposition | Required evidence before promotion |
|---|---|---|
| PR #47 Mom/Loom convergence | Selectively salvage common projections now; retain rest | Reconcile current schemas, keys, SQLite pin, live dispatch/personas/tools, and native ownership; do not merge whole stale branch |
| PR #73/#81 EASL and companion heads | Preserve separately; park product frontend | Text/IME/AX/layout tests, vendor notices, coherent lockfile, generic Tauri renderer boundary |
| PR #49 Cabals / #63 peer hardening / #65 batching / continuous admission | Preserve, off by default; not imported | No default Internet listeners/dials/relays; explicit grants; exact remote assertion semantics; cancellation and local workload isolation |
| PR #59 connected collections | Preserve pending source-bound bridge | Collection occurrence/version/citation and credential/grant boundaries; no ambient data acquisition |
| PR #54 hidden-state training | Preserve experimental/off-by-default | Capability separation, resource budgets, leakage/provenance and reproducible model-specific acceptance |
| PR #85 automatic-v3 | Preserve as candidate, no acceptance substitution | Existing default-family and exact native render/accept/reversal evidence; no token/deadline/guard inflation |
| PR #76 and surviving PR #77 handoffs | Compare current-main overlaps before cherry-pick | Demonstrate a still-failing regression against current main and retain complete negative controls |
| Historical import/release/audit branches | No blind replay; inspect unique deltas and removed feature history | Ancestry/patch-equivalence plus retained feature and acceptance evidence |

`tools/inventory-heads.mjs` computes local branch, remote-tracking and fetched
PR-head relationships with full Git history, without the 300-file REST compare
limit. It writes all changed paths, patch-equivalent/unique commit markers and
source-tree identities. It does not auto-merge or call a branch correct based
on its name, ahead count, PR state, or passing unrelated CI.

Local branches, unpushed work and refs not present in the connected repository
were not visible here. Fetch missing refs explicitly on the local machine,
rerun the inventory, and keep every unresolved head until its useful behavior
is either retained and tested, or deliberately archived with exact provenance.
