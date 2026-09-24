# Ordinary Astra chat -> Git -> local Codex

Use ordinary ChatGPT 6 Pro Astra for substantive research, design, implementation
and review. Git carries exact-source proposals; a local Codex session closes the
build/test loop under one coordinator. This workflow uses no Work mode, Codex
cloud, API fallback, unattended scheduler or paid GitHub CI. GitHub Actions
currently cannot start steps because the account is out of credits. An unstarted
job is unavailable, not a test failure and not a passing check.

Native-kit's current implementation is `crates/native` in native-platform.
Frozen standalone repositories and historical handoffs are provenance, not other
destinations for fixes. Read the owning product contracts before changing behavior.

## Ownership and exact-source exchange

One local integrator owns the integration branch/worktree and its build directory.
Other tasks use their own worktrees and targets. Never reset, clean, rebase, switch
branches in, overwrite or apply patches to another task's checkout. The coordinator
serializes shared-target Cargo work, including baseline checks and packaged builds.
A runner lock is not a substitute for that ownership rule.

Send Astra the exact source SHA and tree, clean/dirty status, relevant complete
files, owning contracts, allowed paths, intended behavior and observed failures.
State selective-archive omissions explicitly. Astra returns a unified exact-base
patch or exact-base commits, the changed-file list, regression cases, reasoning
and explicit execution limits. Do not imply that an archive contains unprovided
product code, Swift sources, fixtures or Git history.

With the installed Chat Pro Bridge, submit one bounded job with a durable ID and
keep its conversation URL. Confirm ordinary Chat and the requested Pro model;
do not substitute a cloud task. An unconfirmed submission is not permission to
send another copy. If the existing draft requires a manual Send, send that draft
once. Continue independent local validation while Astra thinks, then collect the
completed proposal. The current bridge captures response text but can omit
generated download links: download the proposal bundle from that same chat and
provide its local path to the integrator. Verify its manifest and exact-base
file hashes before application. This unofficial adapter is a convenience, not a
supported OpenAI API or a quota exemption. Ordinary chat is not assumed to have
repository write access; the integrator commits and pushes the reviewed result.

In the owned worktree, the integrator verifies the agreed base and clean status,
reviews the entire proposal and its path allowlist, then checks applicability:

```sh
git rev-parse HEAD HEAD^{tree}
git status --porcelain=v1 --untracked-files=all
# Compare these with the agreed handoff; stop on a mismatch.
git apply --check --index /absolute/path/proposal.patch
git apply --index /absolute/path/proposal.patch
git diff --cached --check
git diff --cached
```

Do not blindly force an old patch onto newer work or silently resolve substantive
conflicts. Commit-based proposals likewise need base/ancestry and diff review.
A candidate ahead of main is not permission to merge the candidate's entire stack;
keep unqualified EASL and other feature drafts out of the accepted integration.

Run focused tests while editing. Commit the reviewed local candidate before the
full `local-ci` run: qualification requires a clean, stable commit. Use
CONTRIBUTING.md's actual tool pins and a fresh external evidence directory. Push
only the reviewed commit after inspecting its receipt and outstanding acceptance
obligations; pushing or creating a draft PR does not establish acceptance. Do not
post synthetic GitHub check success or bypass branch protection. Rebasing,
cherry-picking, squashing or amending changes the source identity and requires a
new final gate; do not relabel an old receipt.

## Source-bound failure handoff

Keep every failed run intact. Send the exact failing source SHA/tree, patch or
commit range, command/argv/cwd, tool versions, relevant complete stdout/stderr and
result records, reproducible symptoms and expected contract. Include initial and
final source checks, unexecuted plan steps, platform/feature/model assumptions and
what changed since the preceding run. Never call a zero-test filter or an ignored
test a successful runtime test. Share only relevant evidence after reviewing it
for private data; preserve the original locally, and label any redacted copy.

Local Codex may repair a directly evidenced build integration problem, formatting,
or a small diagnostic-supported defect with a regression test. It must report
its exact diff and commands/results. Architectural changes, unclear ownership or
lifecycle failures, invalid specifications, altered test meaning and substantive
design defects go back to Astra with the source-bound packet. Do not turn a narrow
repair into an unsupervised rewrite, add sleeps/retries to hide races, or weaken
checks to obtain a pass. After a repair, commit and use a new receipt directory.

## Evidence boundaries

Specification/model evidence, representation correspondence, production binding,
component tests and packaged runtime interaction establish different things.
Never infer a later boundary from an earlier one. A replacement backend earns its
own model/runtime evidence; shared interfaces do not transfer qualification.
Warnings in authored code remain fatal; generated/vendor exceptions must be
narrow and must not suppress consumer warnings. A digest binds bytes, not truth.

`local-ci` runs the full macOS **component** gate described in CI-POLICY.md.
Native packaged journeys remain separate through `scripts/release-macos.sh` and
`scripts/smoke-macos-app.sh`, using their current product-owned options and exact
artifact receipts. The local runner only compiles the existing Swift helpers and
executes their existing self-tests through `xtask macos-smoke-support`; it neither
launches products nor requests Accessibility/credential access.

Native failures, including focus and completion reversal, remain separate
product/harness investigations. Component or helper-self-test success does not
resolve them. Preserve manuscripts, credentials,
failed traces and test meaning. Native/model/hardware acceptance and Linux,
Windows, fuzz and dependency-audit qualification remain unproven until their own
current checks actually execute. No unavailable hosted check is fabricated.
