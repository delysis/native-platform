# Chat Pro follow-up: source review and patch authoring

Use ordinary ChatGPT **Chat** mode with a **Pro** model. Work mode is forbidden.

This task is intentionally designed for a remote review model. Do **not** ask
for access to George's Mac, `/Users/george`, Downloads, a local checkout, a
working tree, Cargo, Rust, pnpm, SSH, or a desktop connector. Those are not
available to you and their absence is expected. Do not claim to have run tests,
changed files, committed, pushed, or inspected local status. Your deliverable is
a source-grounded patch artifact and review that George can apply and verify in
his Mac checkout.

## Source and exact revision

The attached files are authoritative. The implementation branch currently
points to commit `30d2a2cdbf3388ebc1ebb07d1e707a2d7b4dba68` on
`codex/loom-easl-main-integration`. The immediately preceding handoff base was
`ec2fa72c59c667f85e83543f7bf00b22332e042f`; do not report that older SHA as the
current remote head. The tested standalone experiment remains
`78392f82762193b3c3f107d981c280f15d727b5d`.

Read these attached documents first:

1. `CONTINUATION.md`
2. `CHECKS.md`
3. `PROVENANCE.json`, `VALIDATION.json`, and `HUNK-CHECKS.json`
4. `VERIFICATION-2026-09-21.md`
5. `0001-repair-protected-original-tests.patch`
6. `SOURCE-EXCERPTS.json`

If a relevant implementation file is attached, inspect its complete contents.
If it is only represented by an excerpt, say exactly which range is missing and
do not invent surrounding code. You may use the public GitHub revision named
above for source lookup if it is accessible, but identify every fetched source
and do not pretend that a GitHub read is a local checkout or test run.

## First deliverable: close the real privacy defect

The continuation packet identified a source-established production write-path
problem. `MaterialEvidence` and material bindings appear to be serialized as
raw JSON under `.loom/materials`, bypassing the existing private-sidecar/vault
codec. This is the priority task.

Trace the complete path from evidence creation through `retain_evidence`,
`install_evidence`, `write_bindings`, folder/grant callers, readers, and the
private-sidecar/payload-codec boundary. Produce a bounded unified diff against
`30d2a2c` that:

- routes private-project evidence and bindings through the existing authenticated
  vault boundary;
- preserves ordinary-project behavior and existing immutable no-clobber rules;
- seals bytes under the final destination namespace, never a temporary filename;
- authenticates/decrypts before parsing or identity checks;
- preserves evidence IDs as canonical-plaintext digests, independent of random
  ciphertext;
- rejects plaintext payloads, tampering, cross-object ciphertext substitution,
  missing-vault descriptors, and downgrade attempts in secured projects;
- leaves external source bytes unchanged on failure; and
- does not globally alter a raw-file helper used by ordinary read-only consumers.

Do not weaken storage, expose a plaintext pathname, silently migrate existing
projects, or hide the issue behind a test-only fixture.

## Required tests in the patch

Add or extend focused secured-project tests using Unicode and mixed CRLF/LF
source. Cover retention, reopening, exact evidence IDs/text/source versions,
tampering, equal-length ciphertext substitution, plaintext downgrade, lost vault
descriptor, idempotent reimport, publisher collisions, and failure cleanup.
Keep synthetic keys confined to tests. Clearly separate ordinary-project tests
from secured-project tests.

Also preserve the already-fixed protected-original tests and the replay-key fix:
the flattened retrieval evidence owns the `materials` JSON key, while the richer
generation evidence must use `material_plan`. Do not reintroduce that collision.

## Secondary review, only where source supports it

Review the current App/CSS/native EASL mismatch described in `CONTINUATION.md`:
Add menu, main-pane collapse/show, one conditional Ghost/Loompad control,
three-way pane icons, theme tokens, titlebar dimensions, disabled/focus states,
and accessible labels. Produce a small source-grounded port plan or a separate
bounded patch only if the actual relevant source is attached. Do not merely
update reference hashes. Do not claim current native UI parity.

Keep Studio, Cast, Hollow, and `easl-native-host` out of all code and patch
proposals. Historical policy references and provenance metadata are not source
imports. Do not add vendor workspaces, dependencies, lockfiles, or a second
application owner inside EASL.

## Output format

Return these sections:

1. **Findings** — exact source paths and functions, with confidence and any
   missing source ranges.
2. **Patch** — a unified diff named `0002-protect-material-evidence.patch`, or
   an explicit statement that a safe complete patch is impossible from the
   attached source. A partial diff is useful only if it is independently
   applicable and its boundary is stated.
3. **Tests added** — exact test names and what each proves.
4. **Mac verification commands** — proposed commands only; do not report them as
   executed. Include Rust 1.92/native-view, frontend, Clippy, policy, formatting,
   and the native interaction checks that still require George's authorization.
5. **Remaining blockers** — current UI parity, native OS input/IME/accessibility,
   signed/relaunch lifecycle, Pretext differential coverage, and any storage
   paths not proven by the available source.

Do not stop with “the Mac worktree is inaccessible.” That is an expected
constraint of this handoff. Review the attached source and return the best
bounded patch/review you can, without claiming execution or publication.
