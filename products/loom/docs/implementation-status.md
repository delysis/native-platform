# Loom Native implementation status

Status date: 2026-08-11.

This document audits the checked-out source tree. It separates implemented behavior from local evidence and from deferred work. It is not a release declaration, performance claim, or substitute for platform acceptance.

## Status vocabulary

- **Implemented and automated** means the behavior exists in the current tree and is covered by a passing automated test in the verification snapshot below.
- **Implemented, limited evidence** means code exists but integration or evidence is narrower than the product acceptance plan.
- **Deferred** means there is no complete product path in this tree. Types, disabled controls, or fixture tests do not make a feature implemented.

## Verification snapshot

The following was observed on one Apple-silicon macOS development machine with Rust/Cargo 1.95.0, Node.js 26.0.0, and pnpm 11.16.0. The full workspace also passed `cargo check --workspace --all-targets` with the declared Rust 1.88 minimum. The checked-in CI now repeats the current-toolchain workspace gates on Linux, macOS, and Windows and runs a dedicated Linux Rust 1.88 job; those remote results remain distinct from this local snapshot.

| Check | Observed result |
| --- | --- |
| `cargo test --workspace --all-targets` | Passed on exact head; environment-bound real-model/frontier tests remained explicitly ignored by the default suite; no failures |
| `pnpm --filter @delysis/loom test` | Passed: 29 files, 173 tests |
| `pnpm --filter @delysis/loom check` | Passed with 0 errors and 0 warnings |
| `pnpm --filter @delysis/loom build` | Passed; Vite produced the static frontend bundle |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed with no warnings |
| Real GGUF ignored tests | The generic Loom raw-family smoke and the feature-gated native writer bridge passed against the exact 484,220,320-byte Qwen artifact on the final native pin; the pinned Gemma 4 E2B base Loom acceptance and companion native-kit raw batch/cancellation acceptance remain separately qualified below |
| Native macOS real-model smoke | The corrected unique unsigned `app.delysis.loom.r4ux3.f7a693.acceptance` bundle removed the rejected skip/count chrome, inserted and reopened an exact literal tab, loaded the exact Gemma 4 E2B base Q8 model on M4 Max Metal, rendered and accepted a caret-local ghost, persisted an immutable promotion, and returned focus to Source mode for a second literal-tab edit. Exact identities and limits are in the UX receipt. |

Not run as part of this snapshot: signed packaging, updater tests, Windows/Linux launched-app workflows, CUDA/Vulkan certification, Playwright automation, screen-reader certification, exhaustive IME matrices, forced termination at every filesystem phase, full large-project latency measurement, and adapter-overhead benchmarking. Source CI nevertheless compiles and tests the portable workspace on Linux, macOS, and Windows.

### Qualified real-model evidence

The ignored test can be invoked without embedding a machine-specific path in source:

```sh
LOOM_GGUF_MODEL_PATH=/absolute/path/to/model.gguf \
  cargo test -p loom-backend-llama \
  adapter::tests::real_gguf_raw_family_acceptance \
  -- --ignored --exact --nocapture
```

The final pinned stack passed this test 1/1 using the independently rehashed 484,220,320-byte `Qwen_Qwen3-0.6B-Q4_K_M.gguf` (SHA-256 `9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`) on the CPU path and returned two candidates labeled by the adapter as live inference. The feature-gated `loom-inference` bridge then passed its disabled-control, embedding, and multi-call lineage proof against the same artifact. These remain generic smoke evidence rather than the base-model gate or throughput claims.

The stricter ignored test is:

```sh
LOOM_GEMMA4_E2B_BASE_PATH=/absolute/path/to/gemma-4-E2B-base-Q8_0.gguf \
  cargo test -p loom-backend-llama \
  adapter::tests::real_gemma4_e2b_base_raw_family_acceptance \
  -- --ignored --exact --nocapture
```

It passed 1/1 on the CPU path in 203.51 seconds using the 4,954,576,032-byte Gemma 4 E2B base Q8 GGUF whose required SHA-256 is `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`. The test fails closed unless native inspection reports architecture `gemma4`, raw completion support, chat as unsupported, and that exact digest. It asserts two distinct branches with seeds 41 and 42, nonempty generated token IDs, live-inference evidence, positive shared-prefix metrics for each branch, and an exact prompt-byte digest.

The companion real-model `llama-native-kit` acceptance also passed against the configured local GGUF. That separate test covers exact text/token completion inputs, ordered seeded raw-family outputs, measured shared-prefix reuse, exactly one terminal state per branch, independent cancellation of one branch while its sibling completes, and fail-closed unsupported FIM. This is evidence for the pinned dependency, not a Loom desktop end-to-end test.

The original desktop slice is recorded in [audit-receipts/2026-08-11-real-gemma-desktop.md](audit-receipts/2026-08-11-real-gemma-desktop.md). Direct author feedback rejected that build's Tab fallback and visible review chrome, so it is historical evidence rather than current product acceptance. The corrected exact-bundle exercise is recorded in [audit-receipts/2026-08-11-r4-quiet-editor-ux.md](audit-receipts/2026-08-11-r4-quiet-editor-ux.md). These combined results do **not** establish:

- Windows CUDA/CPU or Linux CUDA/Vulkan/CPU support, nor general macOS Metal certification beyond the exercised machine and model;
- throughput, responsiveness, thermal behavior, or less-than-5-percent adapter overhead;
- real-model per-branch cancellation through the desktop UI; cancellation remains covered by native/backend acceptance rather than the one desktop sequence; or
- signed-release readiness.

## Implemented surface

### `loom-types`

**Implemented and automated:**

- SHA-256 `BlobId` and `ModelEnvironmentId` value types.
- ULID occurrence identities for artifacts, operations, revisions, documents, projects, commands, branches, runs, candidates, events, and selections.
- Separate content identity and occurrence identity.
- Artifact, operation, revision-segment, document-revision, and command-receipt DTOs.
- Generation model/environment, authority, prompt/context recipe, token trace, generated-span, attestation, selection, terminal, command, and event DTOs.
- Explicit probability stages and inference-evidence classes; absence remains representable instead of being converted to confidence.
- Closed build-model policies with typed names, activations, writer profile identities, exact model digests/sizes, and source-order rank. The default desktop selection is the immutable `writer-gemma4-base-v2`/`quiet_default` contract; v1 retains `project_opt_in` semantics. Build artifacts contain the canonical policy and digest but no builder-local model path. A read-only IPC exposes the bound name/activation/digest identity; the renderer decoder rejects anything outside the exact checked-in triples, and preference derivation defaults off until a verified activation is supplied. Rust admits automatic generation only through a private move-only witness bound to the exact resident model and policy capabilities; automatic budget reservation borrows that proof, request construction preserves it, and native submission consumes the opaque authorized request. Arbitrary loaded models remain manual-only.
- The quiet writing surface presents exact cursor-bound continuations as non-document ProseMirror ghost decorations. Selection requires an exact UTF-8 Markdown witness, a flattened visible-text grapheme boundary, faithful parse/serialize projection, an onscreen caret and first fragment, and a renderer-side SHA-256 proof for the immutable branch body and run. A live keydown-time DOM witness authorizes Tab promotion; otherwise Tab inserts the same literal U+0009 in Visual and Source mode and never traverses into application chrome. Loom's documented visual Markdown dialect reserves every raw tab as manuscript indentation, including at a line edge; it intentionally does not claim CommonMark's conflicting tab-indented-code interpretation. Tabs inside unsupported fenced or inline code fail the exact visual round-trip gate. Escape, editing, IME composition, blur, and caret movement fail closed without changing candidate authority. Contextually unpresentable candidates are skipped so later exact suggestions or one bounded retry can proceed. Suggestion review is available only under Writing options, filtered by the active surface's presentation contract; technical evidence stays collapsed until requested.

**Deferred:** a frozen public compatibility promise beyond the checked-in v1 project/generation golden fixtures. The DTO set does not yet cover the entire planned search, evaluation, retrieval, source, replay-witness, and export domain.

### `loom-document`

**Implemented and automated:**

- Prose CRLF/bare-CR canonicalization at document projection boundaries.
- Exact byte preservation for UTF-8 verse.
- Hybrid block projection with byte-range metadata in memory.
- UTF-8-safe immutable artifact slicing.
- Deterministic scalar-aware three-way merge for visible prose and verse.
- Composition of non-overlapping changes, identical-edit deduplication, structured conflicts with base/app/external byte ranges and text, and deterministic work/output budgets.
- Hybrid merge refusal when block metadata is unavailable.

**Deferred:** persistence and editing of hybrid block manifests, grapheme-aware editor transaction mapping, move-aware/semantic merging, and automatic conflict resolution. The implemented product path always requires an explicit author choice before a reconciliation write.

### `loom-store`

**Implemented and automated:**

- Project initialization/open and manifest schema v1.
- Additive SQLite store migrations 1-9, `STRICT` semantic tables, foreign keys, WAL, `synchronous=FULL`, `trusted_schema=OFF`, and immutable-row triggers. Manifest schema remains v1; migrations 5-6 add idempotent generation-command evidence and a bounded monotonic branch index, while migrations 7-9 add fail-closed research admission, verified inference batches, and the research execution ledger.
- SHA-256 content-addressed blobs verified on read.
- Document registration, initial creation without clobbering an existing visible file, import, checkpoint, open, export, and command receipts.
- Source-bound and idempotent saves. The caller names source revision, source visible blob, and command ID; stale or ambiguous writes fail closed.
- Conflict-preserving outbox projection and recovery across tested boundary/crash states.
- Typed post-commit visible projection state. `Applied` is the only fully acknowledged save; projection-boundary races return a committed receipt plus `PendingConflict`, and projection processing failures return the same committed identity plus `PendingRetry`. Exact retries retain one revision/receipt and retry only the outbox projection.
- Read-only reconciliation snapshots containing base and external text.
- Idempotent explicit external reconciliation bound to the active revision/base blob and exact external-visible blob. It preserves the external bytes as an artifact, records import and merge operations, creates a provenance-preserving resolved revision, and rechecks the external predecessor at transaction and projection boundaries.
- Two-slot transient draft journaling with version compare-and-swap, bounded storage, and per-document monotonic versions that are not reused after clear.
- Atomic checkpoint consumption of an exact draft claim, including the narrowly validated exact successor used to recover a committed draft write whose reply was lost.
- Provenance-preserving human edit diffing. Equal ranges retain prior artifact slices while changed ranges become explicit human contributions.
- Immutable model, prompt/context recipe, authority-policy, generation-run/event/candidate/terminal, selection, and authorship records.
- Atomic generation-family creation bound to one idempotent command, exact-retry replay, command-fingerprint conflict rejection, and immutable command-event/terminal evidence.
- Exactly one terminal event per generation; duplicate output bytes may share a blob while keeping distinct candidate/run occurrences.
- Bounded cursor-based branch summaries and separately bounded body reads. Pagination remains stable when newer branches arrive, error metadata is truncated, and indexed/filesystem lengths are checked before allocation.
- Explicit atomic interrupted-generation recovery that remains idempotent and leaves terminal, cancelled, rejected, pruned, and failed occurrences inspectable.
- Writer/critic promotion authority. Generation, cancellation, rejection, pruning, and keeping an alternative do not mutate the active manuscript; explicit writer-candidate promotion does.
- Path confinement and document-symlink refusal.

**Limits and deferred work:**

- The store does not choose a merge automatically. CLI and desktop callers preview the bounded merge, obtain explicit human resolution, and submit the bound reconciliation command.
- Reconciled output provenance is conservative: unchanged base slices are preserved, the exact external file is retained as an input artifact, and the resolved delta is recorded as a human contribution. The store does not yet map final spans directly to app-versus-external conflict sides or infer author intent.
- Visible-file deletion is deliberately rejected by the reconciliation command; a distinct deletion policy/command is not implemented.
- No continuous filesystem watcher, rename reconciliation, or deletion policy exists. External changes are detected when project/document state is refreshed or a checkpoint observes a conflicting visible file.
- `.loom/backups/outbox` is recovery staging, not a general project backup system.
- `indexes/` exists in the project shape, but FTS5/embedding/source indexes are not implemented.
- Hybrid block metadata is not persisted.
- Crash tests cover selected boundaries, not every requested commit/blob/replacement/compaction/backup phase.
- No compaction or general backup implementation exists.
- No corruption repair UI or complete unknown-extension round-trip suite exists.

### `loom-context`

**Implemented and automated:** bounded ordered completion-prompt parts, deterministic prompt-byte assembly, token-cost metadata, and a validation rule that the live manuscript is the final bytes of a raw completion prompt. Control bytes after the manuscript boundary are rejected. The crate also has bounded exact-excerpt occurrences tied to source artifact/blob/range/hash, tokenizer-bound exact token counts, deterministic fixed-point hybrid ranking, craft-tag evidence, byte/token/count budgets, diversity-aware selection, and exact/fuzzy anti-copy evidence over every prompt excerpt. Semantic retrieval/copy values remain absent unless an external scorer supplies evidence for the exact excerpt occurrence.

**Deferred:** source ingestion, persistence, FTS5/embedding index adapters, actual tokenizer calls, model-assisted reranking, UI/automation integration, model-specific demonstrations, FIM recipes, poetry operations, durable source annotations, and tolerant base-model proposal parsing. The implemented retrieval and anti-copy code is a library primitive, not a source-library product path.

### `loom-search`

**Implemented and automated:** bounded project/global budget ledgers with atomic charging and resume-safe serialization; pause/resume state transitions; occurrence-preserving exact-content grouping; explicit semantic clustering that never fabricates missing observations; evidence-bound hard gates; fixed-point weighted rubrics with abstention; pairwise aggregation that normalizes candidate-order reversal and exposes position/rubric-permutation disagreement; deterministic Pareto extraction; and replayable quality-plus-seeded-novelty selection that does not let duplicate content occupy both slots.

**Deferred:** branch scheduling, breadth/depth generation integration, concrete domain validators/judges, embedding/scorer execution, persistence, desktop UI, local preference learning, overnight policies, and an inspectable durable frontier. Current modules aggregate supplied observations; they do not run models or autonomously search.

### `loom-host`

**Implemented and automated:** bounded job queues, cooperative cancellation, project-level automation opt-in, a focus gate that blocks both manual and automatic generation admission, and bounded generation-family registration/status/cancellation bookkeeping for the desktop lifecycle.

**Implemented, limited evidence:** after verifying the build-policy identity, the default desktop policy quietly enables the project automation gate when no explicit preference exists and uses it for idle nearby suggestions. The prior policy still requires explicit opt-in; an explicit opt-out persists. Turning Suggestions off cancels active session generations. The ordinary editor is already the distraction-free surface, so there is no separate locking Focus-mode control in the primary UI. **Deferred:** a complete background-garden lifecycle, persisted scheduler/restart state, thermal and resource arbitration, native decoding pause/resume, and dependency injection for all planned adapters.

### `loom-backend-llama`

**Implemented and automated or locally smoke-tested:**

- Direct in-process use of the raw and controlled batch APIs from exact `llama-native-kit` successor commit `f7a69316c64d857b99bd847dd44cd852fc5b4ca4`. The commit is not yet remotely published, so this candidate currently requires the audited local source rewrite described in the R4 receipt.
- Exact `GenerationInput::Completion` construction with no Loom-added system prompt, chat template, instruction, or suffix after the manuscript boundary.
- Multiple continuation cases with independent sampling records, bounded event forwarding, ordered output validation, branch-specific cancellation calls, and one Loom terminal event per branch.
- Generated token IDs and optional typed probability observations mapped into Loom token traces. Ordinary verified generation additionally persists the tokenizer-emitted raw piece bytes and cumulative boundaries under a v2 verification commitment; controlled and historical calls make no exact-piece claim.
- Preservation/digesting of native raw event streams and backend receipts.
- Fail-closed distinction between live inference and fixtures; fixture runtimes cannot label output as live.
- Runtime model inspection and required completion/token/cache capability checks.
- Bounded GGUF discovery in configured user paths and Hugging Face cache roots, with header verification and no completeness claim.
- Conservative RAM/VRAM fit calculations that leave unknown inputs and performance unknown.
- HTTPS-only verified GGUF download with credential-bearing URL rejection, bounded redirects and size, exact range-resume validation, cancellation, cold SHA-256 and GGUF checks, symlink/non-regular-file refusal, no-clobber installation, and redacted request diagnostics.

**Limits and deferred work:**

- Automated adapter and desktop-command tests still use fixture runtimes; the real CPU evidence is separately qualified above.
- The Gemma gate is a digest-bound raw-family backend recipe/test, not a curated downloadable catalog, quantization recommendation, or complete author-facing profile suite.
- Discovery verifies only the GGUF container header. Completion/FIM/output-token/logprob support remains unavailable until native load/inspection.
- The downloader requires an author-supplied HTTPS URL, exact SHA-256, and limit; it does not discover publisher catalogs or assert that a downloaded model is suitable.
- Real shared-prefix and independent-cancellation evidence exists at the backend/native-kit layers. The desktop receipt exercises real generation, persistence, promotion, and restart, but not independent branch cancellation through the UI.
- Acceleration diagnostics, measured backpressure/cancellation latency, throughput/overhead, and cross-platform device paths have not passed product acceptance.

### `loom-cli`

**Implemented and automated:** JSON-emitting commands for `init`, `open`, `checkpoint`, `import`, `export`, `recover`, `reconcile-preview`, and `reconcile-apply`. Preview is read-only and can merge an optional bounded app-draft input. Apply requires caller-supplied command ID plus the exact active revision, base blob, and external-visible blob identities; prose canonicalization and exact verse semantics match the store boundary.

**Deferred:** complete CLI parity for generation, cancellation, promotion, keeping alternatives, model management, search, retrieval, evaluation, settings, and structured recovery choices. It is therefore a useful storage/recovery/reconciliation oracle, not yet the planned complete command oracle.

### `tauri-plugin-loom`

**Implemented and automated:**

- Allowlisted commands for direct app-owned default-project open plus secondary project choose/create/open/current/recover/close; document open/checkpoint and transient draft/reconciliation lifecycle; model list/choose/load/unload; verified model-download start/cancel/status/list; bounded branch page/get/body; Weave start/status; project Suggestions opt-in; per-generation cancellation; keep/promotion; focus mode; and application close.
- A single explicit session state machine with project/session/document identity checks.
- Source revision/blob and command-ID binding on checkpoints.
- Draft-version binding and stale-draft handling.
- Existing default-manuscript collision refusal before project initialization.
- Conflict-aware close behavior and refusal to destroy a window with an active project session.
- Read-only external-change preview bound to project/session/document/revision/base identities, plus idempotent reconciliation apply bound to the exact raw external-visible blob.
- Serialized model load/unload/Weave admission, so model residency cannot change between source binding and native generation registration. Automatic starts must pass the distinct opt-in automation gate; switching or unloading is refused while generations are active.
- Exact command replay/fingerprint validation for Weave and model downloads, bounded in-memory active-command registries, status recovery when event delivery is missed, and branch snapshots rebuilt from the durable store.
- Source-revision/blob/cursor-bound raw Weave creation for one to four branches, independently derived seeds, private streaming events, per-run cancellation, immutable result/receipt validation, and fail-closed generation provenance checks before persistence.
- Candidate keep and two-step UI promotion commands. Promotion is source-bound, conflict-preserving, and the only command in this group that may change the active manuscript.
- Permission sets separate ordinary editing/status, local generation, verified network download, and manuscript promotion authority even though the desktop capability currently grants all four sets.
- Typed error codes crossing IPC.

**Deferred:** document create/import UI commands, continuous file watching and rename/delete policy, source/retrieval/settings/search/evaluation commands, persisted model-download history across app restarts, and the background-automation command surface. No attachment, speech, FTE, or hosted-provider commands exist.

### `apps/loom`

**Implemented and automated or locally smoke-tested:**

- Svelte 5 shell with a direct ProseMirror editor, source surface, outline, project selection, document switching, counts, save state, focus mode, status/live regions, reduced-motion/high-contrast CSS, and keyboard shortcuts.
- Normal desktop launch reattaches the live session or provisions/reopens an app-owned ordinary-file `My Writing` project and focuses its `Untitled.md` note. The explanatory landing page and open-versus-create fork are absent from the normal path; one-document projects hide the empty outline.
- Lossless-dialect gate for visual Markdown. Imported or reopened content that the current parser/serializer cannot round-trip exactly is held in source mode. The dialect's one deliberate CommonMark divergence is raw U+0009, which always means manuscript indentation in Visual mode; external tab-indented code must be handled in Source mode. Once a document has safely entered a visual session, transient trailing end-of-file whitespace no longer unmounts the editor mid-keystroke; the native regression was reproduced, fixed, and checked while preserving the exact visible file bytes.
- Exact verse codec for uniform LF, CRLF, or CR line endings. Mixed line endings are shown but editing is locked rather than normalized.
- IME composition gates around editor projection, navigation, checkpointing, and close.
- Serialized semantic saves, idempotent retry after uncertain acknowledgements, project/session/document/revision/blob validation, and stale-result suppression.
- Bounded transient draft journaling plus explicit stale-draft inspect/restore/discard flow.
- Old-source crash drafts are atomically rebound to the current revision before checkpoint; recovered-text deletion requires an explicit two-step permanent-discard confirmation.
- An external-change review surface showing immutable base, Loom side, and exact external file; structured conflict evidence; deterministic safe-merge selection; editable prose resolution; exact-side-only verse resolution; and identity-bound retry-safe apply.
- Reconciliation unmounts the stale editor, locks the captured resolution while apply/refresh is in flight, and re-journals any newer edit made during a checkpoint against the newly committed revision before opening conflict review.
- A secondary Suggestions/model manager for bounded cache discovery, native file selection, explicit native load/unload, exact inspected capability facts, base-versus-chat explanation, and an explicit verified-download form with progress, cancellation, exact-command retry/status recovery, and completed-model selection. A previously and explicitly loaded local model is remembered and may reload in the background after the project's saved Suggestions opt-in is restored; the editor opens first and never waits for weights.
- After explicit per-project opt-in, a quiet idle timer waits for autosave, then starts three private raw-completion branches from the exact Source caret or verified Visual EOF. New typing invalidates and cancels stale work. A matching ready suggestion appears without a generation button; Tab/click promotes only at the same exact boundary, Escape dismisses it, and the bounded alternative shelf remains collapsed until requested.
- There is no manual checkpoint or Weave button on the authoring surface. Semantic checkpoints remain automatic safety machinery; generation/cancellation/status/promotion retain the same typed IPC and durable receipts.
- Stale generation events are rejected by project/session/document/request identity; active manuscript mutation remains impossible until a candidate promotion receipt is explicitly confirmed.
- Safe window/project close choreography.
- Editing remains available without a model.

**Visible but intentionally unavailable:**

- Split mode is disabled until cross-view history is lossless.
- The new-document control is disabled.
- Project search filters document title/path only; it is not manuscript full-text search.
- Hybrid editing is locked.

**Deferred:** continuous filesystem watching, rename/delete reconciliation, notes/comments, revision/recovery timeline, full-text search, global graph/canvas, context/provenance/evaluation/token views, model catalogs/recommendations and acceleration diagnostics, accessibility/IME certification, automated visual regression, Playwright/Tauri workflows, compact-window certification, and production performance measurement. The native smoke above is useful interaction evidence, not certification.

## Storage and authorship boundary

- `manuscript/**` is authoritative for the readable active text.
- `.loom/loom.sqlite3` plus immutable blobs are authoritative for semantic history and provenance.
- `.loom/drafts/**` is bounded mutable recovery state, not semantic history.
- Equal bytes may share a blob but never collapse two causal occurrences.
- Promoted model output retains immutable generation and token evidence. Later human edits retain unchanged generated slices and mark changed spans as human contributions.
- Newly verified ordinary native calls retain content-addressed exact token-piece bytes and cumulative boundaries. Schema-9 and controlled receipts remain diagnostic without synthetic boundary reconstruction.
- Text-only provenance cannot prove keystroke intent when a human retypes bytes identical to the source. The store does not claim otherwise.
- Deleting `.loom/` leaves the visible manuscript readable and irreversibly removes sidecar-only history, branches, evidence, and drafts unless separately backed up.

## Safety and privacy boundary

### Enforced in the current tree

- Every project-owned Rust crate root uses `#![forbid(unsafe_code)]`.
- Normal Loom code contains no subprocess or loopback inference transport; local inference is called through the Rust native-kit API.
- No hosted-provider adapter, telemetry client, or automatic network fallback is present.
- No credential is required by implemented Loom paths.
- Editing and inference do not initiate network traffic. The sole current network client is an explicitly submitted model download, which requires HTTPS, refuses URL credentials, and requires an exact SHA-256 and hard byte ceiling before transfer.
- Tauri IPC is allowlisted and the desktop CSP does not allow arbitrary remote origins.
- Write commands are bound to the active project/session and, where applicable, document/revision/blob/draft/command identities.
- Document paths are confined under `manuscript/`; symlink traversal is refused.
- Visible-file races preserve or hold external bytes instead of using last-writer-wins.
- On Unix, newly created `.loom/` directories use owner-only `0700` modes and newly created sidecar files use `0600`; existing user-selected directory/file modes and visible manuscript modes are not rewritten by that privacy policy.
- Fixture and live inference evidence classes are separate and checked.
- Critics cannot promote prose.
- `.gitleaks.toml` and `.github/workflows/security.yml` define full-history Gitleaks scanning and pull-request dependency review; third-party actions are pinned to full commit SHAs.

### Not established

- `#![forbid(unsafe_code)]` does not cover native/FFI code in dependencies such as llama.cpp bindings.
- The checked-in security workflow has not been observed running remotely in this snapshot. Its repository/history scope is Loom Native only; it is not evidence that other family repositories or their histories were scanned.
- Remediation of any credential in another repository, including provider-side revocation/rotation or Git history cleanup, cannot be proven by this tree and must not be inferred from this status.
- Offline behavior has not yet passed a dedicated network-disabled desktop test.
- Equivalent owner-only Windows ACL creation and verification is not established; current Windows sidecar access follows inherited filesystem ACLs.
- A local `cargo audit` run found no vulnerability-class advisory, but it is not warning-clean: 17 informational warnings include the Linux GTK/Tauri path's `glib 0.18.5` RUSTSEC-2024-0429 unsoundness advisory and unmaintained GTK3/proc-macro/unic dependencies. This requires an explicit reviewed exception or upstream-stack update before Linux release.
- Path confinement still uses pathname check-then-open operations. A same-user concurrent adversary that can swap directories may race those checks; directory-handle/openat-style confinement remains platform-hardening work.
- Export redaction, successful remote security-workflow evidence, fuzzing, penetration testing, sandbox review on every platform, and supply-chain publication controls remain open.

## Phase checklist

### 1. Security and family stabilization — **incomplete**

- [x] Created the Rust 2024 Loom workspace and kept project-owned Rust free of `unsafe`.
- [x] Added a direct adapter against the product-neutral raw batch-family native-kit API.
- [x] Pinned every native-kit consumer to exact successor `f7a69316c64d857b99bd847dd44cd852fc5b4ca4`, including move-only polling and ordinary-generation token-piece traces, and reran the complete workspace plus real Qwen/Gemma compatibility gates. Publication of that native commit remains a promotion prerequisite.
- [ ] Complete and independently verify external Bloom credential revocation/rotation and any separately approved history remediation.
- [x] Add SHA-pinned Loom Native full-history secret scanning and pull-request dependency-review automation. A successful remote run is not yet evidenced here.
- [ ] Pin and integrate a verified `attachment-native-kit` release; no attachment dependency exists here.
- [x] The referenced autoloom task reports all four owned SQLite connection sites wrapped in explicit closing and 417 tests passing with `ResourceWarning` promoted to an error. This is external coordination evidence, not code contained in this Loom tree.
- [ ] Record and pin a published attachment/native-kit family state in Loom itself; successful work in another task does not make this checkout portable.

### 2. Crash-safe editor foundation — **substantial foundation, incomplete product**

- [x] Project folder, v1 manifest, SQLite migrations, content-addressed blobs, outbox, recovery CLI, prose/verse projection, and transient drafts.
- [x] Direct Svelte/ProseMirror shell with guarded visual/source editing, outline, autosave/checkpoints, focus/close choreography, and exact verse handling.
- [x] Keep an already-safe visual session mounted through transient trailing EOF whitespace while continuing to force unsafe imported/reopened bytes into source mode.
- [x] Standalone deterministic three-way merge, read-only store snapshot, and an idempotent explicit-reconciliation command that records external/import/merge causality.
- [x] Wire detected external changes through CLI and typed plugin/UI merge review with explicit identity-bound reconciliation submission.
- [ ] Add filesystem watching, rename/delete handling, full-text search, source/split parity, new-document/import UI, and hybrid metadata persistence.
- [ ] Complete crash-injection, corruption, migration, large-document, Unicode/RTL/IME, and end-to-end matrices.

### 3. Complete local base-model vertical slice — **real desktop suggestion slice accepted; broader certification incomplete**

- [x] Exact raw completion adapter, typed capability verification, ordered branch results, bounded events, cancellation calls, token/provenance conversion, fixture honesty tests, and local discovery/fit libraries.
- [x] Pin the generalized native branch-family implementation and pass its real ordered-batch, shared-prefix, independent-cancellation, and capability acceptance.
- [x] Connect desktop `Weave` through model choose/load/unload, source-bound generation persistence, streaming, bounded durable branch comparison, explicit promotion, independent cancellation, exact-command recovery, and restart-visible branch reconstruction.
- [x] Pass the digest-bound real Gemma 4 E2B **base** Q8 Loom backend acceptance with two independent seeds, exact raw prompt identity, generated tokens, live evidence, and positive shared-prefix metrics.
- [x] Exercise real Gemma model load, a three-candidate automatic suggestion, caret-local comparison affordance, explicit Tab promotion, durable selection/revision evidence, loaded-model quit, and immediate restart recovery through the desktop UI.
- [ ] Exercise independent one-branch cancellation through the desktop UI; it is currently proven only at the native/backend boundary.
- [ ] Establish portable macOS/Windows/Linux backends and measured responsiveness/overhead.

### 4. Automation, retrieval, and evaluation — **nearby suggestions integrated; broader automation/retrieval incomplete**

- [x] Opt-in/focus gates, bounded atomic budget ledgers/state, immutable recipe/authority/generation records, and promotion-only manuscript mutation.
- [x] Connect opt-in idle nearby suggestions to autosave, exact caret-bound generation, stale-work cancellation, collapsed alternatives, and explicit Tab/Escape accept/dismiss behavior without manual generation/checkpoint controls.
- [x] Implement bounded hard-gate aggregation, occurrence-preserving exact deduplication, explicit semantic clustering, fixed-point rubrics, pairwise order/rubric-permutation disagreement, abstention, Pareto extraction, and replayable quality-plus-seeded-novelty selection as pure Rust primitives.
- [x] Implement exact source-excerpt identity/range/hash, tokenizer-bound counts, deterministic hybrid ranking/diversity/budgets, craft-tag evidence, and exact/fuzzy plus externally evidenced semantic anti-copy checks as pure Rust primitives.
- [ ] Add source ingestion, FTS5/embedding/tokenizer/scorer adapters, immutable retrieval/evaluation persistence, and product UI; current primitives receive already-extracted observations.
- [ ] Add background garden, staged Weave, Studio/overnight schedulers, generation integration, concrete validators/judges, inspectable persistent frontier, and local preference learning.

### 5. Polished solo-writer release — **not reached**

- [x] Development editor remains usable without a loaded model.
- [x] Implemented paths have no hosted credential or silent cloud fallback.
- [x] Explicit HTTPS model download with resume, mandatory hash verification, cancellation/status recovery, and no-clobber installation.
- [ ] Attachment and speech adapters, publisher catalogs/tested hardware profiles, polished recovery/backups, and DOCX/EPUB export.
- [ ] Accessibility, IME, keyboard, visual, compact-window, latency, and offline certification.
- [ ] Signed macOS/Windows/Linux installers, updater, and platform smoke suites. An unsigned debug macOS app bundle built for native smoke testing; default release bundling remains disabled.
- [ ] Optional FTE/hosted composition behind an explicit visible cloud boundary.

### 6. Post-v1 — **deferred**

- [ ] Git synchronization, review/comments, collaboration, CRDTs, plugin SDK, renderer-independent UI state, server/web renderer, advanced token/logit/steering/interpretability views, and formally specified collaborative DAG movement.

## Definition of the current milestone

The current milestone is a crash-conscious local editor/storage foundation plus a quiet local-model suggestion slice: launch reaches an ordinary-file note, autosave/checkpoints are implicit, and local suggestions grow after an idle pause and remain private until exact-boundary acceptance. Model inspection, verified download, durable branches, cancellation, comparison, and promotion exist behind typed authority boundaries. The Gemma 4 E2B base backend gate and the exact Metal desktop suggestion/promotion/quit/relaunch slice passed. The native research engine now has persisted headless trial/campaign/evaluation/benchmark foundations, but the five-function qualification program, frozen profiles, Studio views, attachment/speech/FTE product composition, richer export, platform certification, signing, and release operations remain deferred. This is not a release candidate.
