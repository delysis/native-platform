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

## Full task queue

Work through as many of these as the attached source can support. Group related
changes into small reviewable patches, and stop a patch at a real ownership or
missing-source boundary. A proposed patch may be accompanied by a precise
follow-up patch plan; do not turn missing execution evidence into a reason to
omit source review.

### P0 — correctness, privacy and replay

1. Finish the protected material-evidence/bindings path described above.
2. Inventory every `.loom/materials` write, temporary file, manifest, object,
   binding, folder, grant, search index and receipt path. Mark each as ordinary
   or private and identify the codec/namespace owner.
3. Check every private-sidecar caller for authenticate-before-parse,
   expected-length bounds, digest verification, symlink/final-component races,
   atomic publication, no-clobber behavior and cleanup after interruption.
4. Check protected-copy and migration behavior. Existing plaintext projects must
   remain readable and must not be silently rewritten, erased or “upgraded.”
5. Verify source and evidence IDs are derived from canonical plaintext, while
   encrypted representations remain randomized and authenticated.
6. Finish the `material_plan`/flattened `materials` schema separation. Add
   backward-compatibility or explicit rejection behavior for any evidence shape
   already capable of being written by the published branch.
7. Audit generation and terminal replay for frozen configuration, persona bytes,
   sampling, function recipe, material plan, retrieval evidence, media grants,
   model identity, server/native route, cancellation and request fingerprints.
8. Check every idempotency key for stable request identity, collision handling,
   retry semantics and exact replay. Add negative cases for same command IDs with
   changed source, settings, cursor, pane, model, materials or policy.
9. Audit failure atomicity: failed import, failed encryption, failed publication,
   cancellation, worker panic, shutdown and lost acknowledgement must not leave a
   false receipt or partial private artifact.
10. Check external-edit, stale-revision, deleted-document, lease and recovery
    behavior against ordinary source bytes and immutable history.

### P1 — current Loom interface and service ownership

11. Build a source-derived control/state matrix from current `App.svelte` and
    `app.css`: Add menu, titlebar, pane toggles, Ghost/Loompad, recording,
    materials, imports, settings, model status, errors, focus, disabled and busy
    states. Identify every native EASL input and missing state.
12. Port the actual Add menu actions and dismissal behavior. Do not label a
    direct-new-document button “Add.”
13. Port main/right/bottom pane visibility, fallback geometry, mounted-state
    preservation, pane-local selection/scroll and busy-pane cancellation.
14. Port the single conditional Ghost/Loompad control, including icon, label,
    tooltip, enabled state and service action. Keep it disabled when authority is
    absent; do not fake streaming.
15. Port theme tokens, titlebar dimensions, title truncation, hover/focus/pressed/
    disabled states, dark/light appearance and useful content-area sizing.
16. Replace positional SVG extraction with explicit keyed branch extraction and
    source/hash tests for every current conditional icon.
17. Audit App/Svelte merge behavior: imports, setup, materials, workspace panes,
    completion surfaces, settings refresh, error banners and callbacks. Remove
    stale props and keep browser fixtures aligned with real component contracts.
18. Keep all persistence, credentials, model lifecycle, acquisition, grants,
    cancellation and provenance in shared Rust/native-kit services. Identify any
    duplicate authority accidentally reintroduced in the view.
19. Complete document lifecycle gaps visible in the source: create, rename,
    delete, export, reveal, folder warnings, watcher/reconcile, large-folder
    behavior, fatal-worker recovery and exact source preservation.
20. Complete outline/search/tree presentation and keyboard traversal for long
    documents, including truncation, drag, selection and empty/error states.
21. Bind chat, terminal, page-preview, import, material, attachment, recording,
    retry and transcript controls to existing owners. A control may remain
    explicitly disabled, but its state and reason must be honest.

### P1 — reusable EASL text/editor ecosystem

22. Map the reusable EASL APIs and identify the one production Loom pane that can
    consume them without creating a second document/history owner.
23. Integrate EASL shaping, rich-style resolution, line/cell geometry, glyph
    placement, hit testing, caret, selection and decorations behind a comparison
    oracle. Publish replacement geometry only after validation against the current
    source/history boundary.
24. Keep `EditPlan` snapshot-relative: bind and revalidate document, revision,
    pane and selection before applying an asynchronous result.
25. Complete font loading/matching, fallback, color emoji, variable fonts,
    multi-page atlas packing, eviction, immutable texture residency and font
    lifecycle across platforms.
26. Cover grapheme-safe editing, combining marks, CJK, dead keys, IME preedit,
    bidi, script runs, ligature caret stops, tabs, discretionary hyphenation,
    negative advances, whitespace marks, entity/source mapping and malformed
    Unicode. Unsupported cases must preserve source and expose recovery.
27. Add retained multi-paragraph layout and edit invalidation with explicit
    resource limits. Measure compiler, VM, transfer, shaping, allocation and
    paint costs independently; do not call bounded specimens unlimited.
28. Make clipboard, word/line/granular selection, wheel/pointer behavior and
    generic input/accessibility transport reusable across EASL consumers.
29. Improve EASL module/API ergonomics and compiler startup for nested arrays,
    namespaces, large programs and repeated hot reloads. Keep product policy out
    of the reusable library.

### P2 — layout quality, differential behavior and performance

30. Compare the pinned Pretext revision against equivalent fonts, options, rich
    items, no-break behavior and actual line choices. Distinguish browser
    geometry equivalence from native invariant tests.
31. If claiming Butterick/TeX-level typography, specify and implement dictionary
    hyphenation, Knuth–Plass glue/fitness, optical margins, protrusion,
    microtypography and paragraph-breaking policy. Otherwise label the current
    implementation accurately as a foundation.
32. Design multilingual, variable-font, emoji, long-document, pathological-line,
    malformed-font and capacity-exhaustion corpora with accepting and rejecting
    cases.
33. Measure cold startup, initial layout, typing-to-display, navigation,
    selection, scroll, resize, palette opening, repeated clicks and sustained
    editing at multiple scales. Report p50/p95, allocations, cache hit rates and
    host/compiler boundary costs.
34. Reduce CPU repaint cost with damage caching or GPU composition only where
    measurements justify it. Preserve deterministic offscreen review images.
35. Add fuzz/property/roundtrip/failure-atomicity coverage for text, fonts,
    ranges, edits, reload, released resources, cache invalidation and malformed
    display-list input.

### P2 — native acceptance and packaging preparation

36. Define a signed macOS review-bundle procedure with source SHA, executable
    hash, bundle ID, embedded assets, signing identity, PID and current-only
    accessibility evidence.
37. Prepare foreground test scripts for pointer, keyboard, Tab/Shift-Tab,
    splitters, palette dismissal, Add menu, panes, copy/cut/paste, undo, Return,
    stale selections, long URLs, CJK/IME, dead keys, candidate positioning and
    Retina geometry. These remain proposed until George authorizes interaction.
38. Qualify native/Wry focus transfer, shortcuts, IME ownership and the combined
    accessibility tree. Keep platform-webview coexistence explicit.
39. Prepare signed save/quit/relaunch, encrypted history, external edit,
    recovery, lease, shutdown and model-cancellation acceptance scenarios.
40. Prepare Linux/Windows component/runtime qualification separately from macOS;
    never transfer native OS evidence between platforms.

### P3 — repository hygiene and release evidence

41. Re-run the forbidden-source/published-ancestry audit. Explain every remaining
    Studio/Cast/Hollow string as policy, provenance or documentation, and reject
    actual source/dependency imports.
42. Check manifests, permissions, generated schemas, lockfiles, vendor boundaries,
    feature unions and build guards for stale or duplicated authority.
43. Keep exact source SHA, patch SHA, toolchain, target, features, test counts,
    ignored/manual tests, warnings, environment blockers and artifact identity in
    machine-readable receipts.
44. Separate specification, correspondence, production binding, component tests,
    packaged runtime and live native acceptance. Never promote one evidence class
    into another.
45. Produce a final prioritized TODO ledger with owners, dependencies, exact
    files, proposed commands, acceptance evidence and a clear “not run here” list.

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
