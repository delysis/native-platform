# W4 gateway import evidence

W4 is accepted. Accepted `delysis/free-token-energy` main
`67814e76659688fef61f311db588d17eddee0a66` was imported beneath
`products/fte` with its history, integrated into the root Rust and pnpm
workspaces, and merged to native-platform main as
`c9dc619300eb78c47fd3a8b9dbfaacf2e7ae45d9`.

## Deterministic import

`git-filter-repo==2.47.0 --to-subdirectory-filter products/fte --force`
produced filtered head `c3f0bef8aac5770f42e4f57abc1d7cf0f33b088b`.
The authenticated 61-line map is
`migration/free-token-energy.commit-map`, SHA-256
`c3ee3e579668c1c6ca8275de2acf02832b8a0d7f35f1648ec211ce436c1b369e`.
It maps the accepted source head to the filtered head and maps phase-one
commit `c451e23244787fc8de7646a88a4fd4ae10f16f94` to imported commit
`f69dcf9b8b940e983f27faf16fabf47b133cc4e5`.

Raw import merge `8e5c9282314bc85140ac1c7f0421caaed2dc3e93`
has parents accepted W3 main `14cfcde2440b79fd734db040b691be8366688803`
and the filtered head. Cutover commit
`a76a13066936e219ca10ecc5fc0080395b725fcc` moved FTE packages into the
root workspaces, removed nested locks and workflows, and rebound native
dependencies to `crates/native`.

## Promotion

Candidate `b427fd7c5f4af6bd708c2509a010cc5017f06609` passed three-OS run
31655648292. It merged without squashing; main merge
`c9dc619300eb78c47fd3a8b9dbfaacf2e7ae45d9` has the same tree as the
candidate and exact parents W3 main and the candidate. Post-merge run
31657465002 passed on macOS, Windows, and Ubuntu.

Protected annotated tags
`w4-import-gateway-candidate-v0-2026-08-12` and
`w4-import-gateway-v0-2026-08-12` peel to the candidate and main merge.
Ruleset 20774104 prevents their deletion or retargeting.

## Product boundary

The imported product has one gateway runtime, preserves `LocalOnly` for exact
local requests, and makes automatic requests prefer a ready local model with
a configured hosted fallback. The accepted packaged application uses bundle
identifier `dev.delysis.free-token-energy` and macOS baseline 13.0.

Local acceptance used exact Qwen3 0.6B Q4_K_M bytes at SHA-256
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`.
Visible automatic routing selected local Qwen, active Cmd-Q exited cleanly,
and relaunch restored the local model. Disposable macOS Keychain tests covered
write, readback, replacement, deletion, and absence without recording secret
bytes. The supplied legacy-namespace SIGABRT remains negative evidence; the
old plaintext store was not modified.

No live paid hosted-provider request or scheduled Attachment run is required
or claimed. No signed or notarized distribution is claimed.

## Source freeze

Protected source boundary tag `native-platform-v2-horizon-b-2026-08-12`
peels to the imported source head. README-only freeze commit
`ecee06feb803fdcdbd2e917f4592697935c3c59a` has the imported source head
as its sole parent. Source runs 31657662783 and 31657662800 passed.
No-bypass ruleset 20775226 freezes all refs. The source repository remains
unarchived with issues enabled until two stable native-platform releases; no
such release is claimed here.
