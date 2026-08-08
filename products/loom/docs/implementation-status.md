# Loom Native implementation status

Status date: 2026-08-08.

This document audits the checked-out source tree. It separates implemented behavior from local evidence and from deferred work. It is not a release declaration, performance claim, or substitute for platform acceptance.

## Status vocabulary

- **Implemented and automated** means the behavior exists in the current tree and is covered by a passing automated test in the verification snapshot below.
- **Implemented, limited evidence** means code exists but integration or evidence is narrower than the product acceptance plan.
- **Deferred** means there is no complete product path in this tree. Types, disabled controls, or fixture tests do not make a feature implemented.

## Verification snapshot

The following was observed on one Apple-silicon macOS development machine with Rust/Cargo 1.95.0, Node.js 26.0.0, and pnpm 11.16.0. The manifest declares Rust 1.88, but no Rust 1.88-specific run was performed. This snapshot does not cover Windows or Linux.

| Check | Observed result |
| --- | --- |
| `cargo test --workspace` | Passed: 102 Rust tests; 1 real-GGUF test ignored by default; no failures |
| `pnpm --filter @delysis/loom test` | Passed: 6 files, 20 tests |
| `pnpm --filter @delysis/loom check` | Passed with 0 errors and 0 warnings |
| `pnpm --filter @delysis/loom build` | Passed; Vite produced the static frontend bundle |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed with no warnings |
| Real GGUF ignored test | Passed separately with the qualified local evidence below |
| Frontend browser smoke | Landing shell rendered at the available desktop viewport with its headings, controls, skip link, and privacy boundary exposed; native IPC, the active editor, and compact-window workflows were not exercised in this browser-only pass |

Not run as part of this snapshot: signed packaging, updater tests, Windows/Linux builds, Metal/CUDA/Vulkan certification, Playwright/Tauri end-to-end workflows, screen-reader certification, exhaustive IME matrices, fuzzing, forced termination at every filesystem phase, full large-project latency measurement, and adapter-overhead benchmarking.

### Qualified real-model evidence

The ignored test can be invoked without embedding a machine-specific path in source:

```sh
LOOM_GGUF_MODEL_PATH=/absolute/path/to/model.gguf \
  cargo test -p loom-backend-llama \
  adapter::tests::real_gguf_raw_family_acceptance \
  -- --ignored --exact --nocapture
```

Two local development runs passed 1/1 using `Qwen_Qwen3-0.6B-Q4_K_M.gguf` (Qwen3 0.6B, Q4_K_M) on the CPU path and returned two candidates labeled by the adapter as live inference. Observed wall times were 81.47 seconds and 206.43 seconds; the slower rerun shared the machine with concurrent build/test work, so neither duration is a throughput claim. The test asserts the two-candidate result and live-evidence classification.

That result is only a local smoke test. It does **not** establish:

- Gemma 4 base-model acceptance;
- macOS Metal, Windows CUDA/CPU, Linux CUDA/Vulkan/CPU support;
- real-model independent-cancellation behavior;
- throughput, responsiveness, thermal behavior, or less-than-5-percent adapter overhead;
- desktop generation, persistence, promotion, or restart recovery; or
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
- Additive SQLite store migrations 1-4, `STRICT` semantic tables, foreign keys, WAL, `synchronous=FULL`, `trusted_schema=OFF`, and immutable-row triggers. Manifest schema remains v1; migration 4 adds monotonic draft-generation identity.
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
- Exactly one terminal event per generation; duplicate output bytes may share a blob while keeping distinct candidate/run occurrences.
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

**Implemented and automated:** bounded ordered completion-prompt parts, deterministic prompt-byte assembly, token-cost metadata, and a validation rule that the live manuscript is the final bytes of a raw completion prompt. Control bytes after the manuscript boundary are rejected.

**Deferred:** retrieval, excerpt ranking, tokenization-backed budget calculation, model-specific demonstrations, FIM recipes, poetry operations, anti-copy evidence, source annotations, and tolerant base-model proposal parsing.

### `loom-search`

**Implemented and automated:** a bounded search budget, pause/resume state transitions, usage accounting, and deterministic Pareto-frontier extraction over quality and novelty points.

**Deferred:** branch scheduling, breadth/depth search, generation integration, validators, exact/semantic deduplication, clustering, pairwise judges, rubric permutation, abstention/disagreement handling, local preference learning, overnight policies, and inspectable frontier persistence.

### `loom-host`

**Implemented and automated:** bounded job queues, cooperative cancellation, project-level automation opt-in, and a focus gate that blocks both manual and automatic generation admission.

**Deferred:** a complete background-job lifecycle, persistence/restart, thermal and resource arbitration, typing-sensitive inference pause, dependency injection for all adapters, and GUI event delivery.

### `loom-backend-llama`

**Implemented and automated or locally smoke-tested:**

- Direct in-process use of the locally checked-out `llama-native-kit` raw batch API.
- Exact `GenerationInput::Completion` construction with no Loom-added system prompt, chat template, instruction, or suffix after the manuscript boundary.
- Multiple continuation cases with independent sampling records, bounded event forwarding, ordered output validation, branch-specific cancellation calls, and one Loom terminal event per branch.
- Generated token IDs and optional typed probability observations mapped into Loom token traces.
- Preservation/digesting of native raw event streams and backend receipts.
- Fail-closed distinction between live inference and fixtures; fixture runtimes cannot label output as live.
- Runtime model inspection and required completion/token/cache capability checks.
- Bounded GGUF discovery in configured user paths and Hugging Face cache roots, with header verification and no completeness claim.
- Conservative RAM/VRAM fit calculations that leave unknown inputs and performance unknown.

**Limits and deferred work:**

- The three native-kit dependencies are local relative paths that resolve under `/Users/george/Documents/llama-native-kit`; they are neither vendored nor pinned to a portable published revision.
- The backend's automated generation tests use fixture runtimes; the one real Qwen smoke test is separately qualified above.
- No Gemma 4 base recipe or acceptance result exists.
- No desktop command currently starts this adapter.
- No model loading/selection UI, resumable download, hash-verified catalog, quantization recommendation, or acceleration diagnostic exists.
- Capability discovery in the desktop model list verifies only the GGUF container header. It deliberately reports completion/FIM/output-token/logprob support as unavailable until native inspection.
- Real-model cache reuse, independent cancellation, bounded-backpressure loss policy, and cross-platform device paths have not passed product acceptance here.

### `loom-cli`

**Implemented and automated:** JSON-emitting commands for `init`, `open`, `checkpoint`, `import`, `export`, `recover`, `reconcile-preview`, and `reconcile-apply`. Preview is read-only and can merge an optional bounded app-draft input. Apply requires caller-supplied command ID plus the exact active revision, base blob, and external-visible blob identities; prose canonicalization and exact verse semantics match the store boundary.

**Deferred:** complete CLI parity for generation, cancellation, promotion, keeping alternatives, model management, search, retrieval, evaluation, settings, and structured recovery choices. It is therefore a useful storage/recovery/reconciliation oracle, not yet the planned complete command oracle.

### `tauri-plugin-loom`

**Implemented and automated:**

- Allowlisted commands for project choose/create/open/current/recover/close, document open/checkpoint, transient draft upsert/clear, bounded reconciliation preview/apply, local model listing, focus mode, and application close.
- A single explicit session state machine with project/session/document identity checks.
- Source revision/blob and command-ID binding on checkpoints.
- Draft-version binding and stale-draft handling.
- Existing default-manuscript collision refusal before project initialization.
- Conflict-aware close behavior and refusal to destroy a window with an active project session.
- Read-only external-change preview bound to project/session/document/revision/base identities, plus idempotent reconciliation apply bound to the exact raw external-visible blob.
- Typed error codes crossing IPC.

**Deferred:** document create/import UI commands, continuous file watching and rename/delete policy, inference streaming, branch commands, source/retrieval/model-management writes, settings, search, and evaluation commands. Generation permissions are not granted by the current default capability.

### `apps/loom`

**Implemented and automated or locally smoke-tested:**

- Svelte 5 shell with a direct ProseMirror editor, source surface, outline, project selection, document switching, counts, save state, focus mode, status/live regions, reduced-motion/high-contrast CSS, and keyboard shortcuts.
- Lossless-subset gate for visual Markdown. Content that the current parser/serializer cannot round-trip exactly is held in source mode.
- Exact verse codec for uniform LF, CRLF, or CR line endings. Mixed line endings are shown but editing is locked rather than normalized.
- IME composition gates around editor projection, navigation, checkpointing, and close.
- Serialized semantic saves, idempotent retry after uncertain acknowledgements, project/session/document/revision/blob validation, and stale-result suppression.
- Bounded transient draft journaling plus explicit stale-draft inspect/restore/discard flow.
- Old-source crash drafts are atomically rebound to the current revision before checkpoint; recovered-text deletion requires an explicit two-step permanent-discard confirmation.
- An external-change review surface showing immutable base, Loom side, and exact external file; structured conflict evidence; deterministic safe-merge selection; editable prose resolution; exact-side-only verse resolution; and identity-bound retry-safe apply.
- Reconciliation unmounts the stale editor, locks the captured resolution while apply/refresh is in flight, and re-journals any newer edit made during a checkpoint against the newly committed revision before opening conflict review.
- Safe window/project close choreography.
- Editing remains available without a model.

**Visible but intentionally unavailable:**

- Split mode is disabled until cross-view history is lossless.
- The `Weave` button cannot become active because discovered GGUF files are not loaded or capability-verified by the desktop path.
- The branch shelf renders only if in-memory branch cards exist; no backend command populates it.
- The new-document control is disabled.
- Project search filters document title/path only; it is not manuscript full-text search.
- Hybrid editing is locked.

**Deferred:** continuous filesystem watching, rename/delete reconciliation, notes/comments, revision/recovery timeline, full-text search, global graph/canvas, context/provenance/evaluation/token views, polished model manager, accessibility/IME certification, visual regression, Playwright/Tauri workflows, compact-window certification, and production performance measurement.

## Storage and authorship boundary

- `manuscript/**` is authoritative for the readable active text.
- `.loom/loom.sqlite3` plus immutable blobs are authoritative for semantic history and provenance.
- `.loom/drafts/**` is bounded mutable recovery state, not semantic history.
- Equal bytes may share a blob but never collapse two causal occurrences.
- Promoted model output retains immutable generation and token evidence. Later human edits retain unchanged generated slices and mark changed spans as human contributions.
- Text-only provenance cannot prove keystroke intent when a human retypes bytes identical to the source. The store does not claim otherwise.
- Deleting `.loom/` leaves the visible manuscript readable and irreversibly removes sidecar-only history, branches, evidence, and drafts unless separately backed up.

## Safety and privacy boundary

### Enforced in the current tree

- Every project-owned Rust crate root uses `#![forbid(unsafe_code)]`.
- Normal Loom code contains no subprocess or loopback inference transport; local inference is called through the Rust native-kit API.
- No hosted-provider adapter, telemetry client, or automatic network fallback is present.
- No credential is required by implemented Loom paths.
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
- [x] Added a direct adapter against the locally evolved raw batch-family native-kit API.
- [ ] Replace local native-kit paths with reviewed portable pinned dependencies.
- [ ] Complete and independently verify external Bloom credential revocation/rotation and any separately approved history remediation.
- [x] Add SHA-pinned Loom Native full-history secret scanning and pull-request dependency-review automation. A successful remote run is not yet evidenced here.
- [ ] Pin and integrate a verified `attachment-native-kit` release; no attachment dependency exists here.
- [x] The referenced autoloom task reports all four owned SQLite connection sites wrapped in explicit closing and 417 tests passing with `ResourceWarning` promoted to an error. This is external coordination evidence, not code contained in this Loom tree.
- [ ] Record and pin a published attachment/native-kit family state in Loom itself; successful work in another task does not make this checkout portable.

### 2. Crash-safe editor foundation — **substantial foundation, incomplete product**

- [x] Project folder, v1 manifest, SQLite migrations, content-addressed blobs, outbox, recovery CLI, prose/verse projection, and transient drafts.
- [x] Direct Svelte/ProseMirror shell with guarded visual/source editing, outline, autosave/checkpoints, focus/close choreography, and exact verse handling.
- [x] Standalone deterministic three-way merge, read-only store snapshot, and an idempotent explicit-reconciliation command that records external/import/merge causality.
- [x] Wire detected external changes through CLI and typed plugin/UI merge review with explicit identity-bound reconciliation submission.
- [ ] Add filesystem watching, rename/delete handling, full-text search, source/split parity, new-document/import UI, and hybrid metadata persistence.
- [ ] Complete crash-injection, corruption, migration, large-document, Unicode/RTL/IME, and end-to-end matrices.

### 3. Complete local base-model vertical slice — **library slice only**

- [x] Exact raw completion adapter, typed capability verification, ordered branch results, bounded events, cancellation calls, token/provenance conversion, fixture honesty tests, and local discovery/fit libraries.
- [x] One qualified CPU-only Qwen3 0.6B Q4 real-GGUF smoke test.
- [ ] Connect desktop `Weave` through model load, generation persistence, streaming, branch comparison, explicit promotion, cancellation, and restart recovery.
- [ ] Prove real shared-prefix reuse and independent cancellation in the Loom acceptance path.
- [ ] Pass a real Gemma 4 **base** model recipe; instruct-model or Qwen evidence does not satisfy this item.
- [ ] Establish portable macOS/Windows/Linux backends and measured responsiveness/overhead.

### 4. Automation, retrieval, and evaluation — **domain primitives only**

- [x] Opt-in/focus gates, search budgets/state, simple Pareto frontier, immutable recipe/authority/generation records, and promotion-only manuscript mutation.
- [ ] Background garden, staged weave, Studio/overnight scheduling, clustering, validators, quality-plus-novelty selection, and persistent frontier.
- [ ] Source ingestion, exact-excerpt retrieval, FTS5/embeddings, craft tags, reranking, and anti-copy evidence.
- [ ] Evaluation artifacts, blind/pairwise comparison, judge robustness tests, abstention/disagreement, and local preference learning.

### 5. Polished solo-writer release — **not reached**

- [x] Development editor remains usable without a loaded model.
- [x] Implemented paths have no hosted credential or silent cloud fallback.
- [ ] Attachment and speech adapters, model downloads, tested profiles, polished recovery/backups, and DOCX/EPUB export.
- [ ] Accessibility, IME, keyboard, visual, compact-window, latency, and offline certification.
- [ ] Signed macOS/Windows/Linux installers, updater, and platform smoke suites. Tauri bundling is currently disabled.
- [ ] Optional FTE/hosted composition behind an explicit visible cloud boundary.

### 6. Post-v1 — **deferred**

- [ ] Git synchronization, review/comments, collaboration, CRDTs, plugin SDK, renderer-independent UI state, server/web renderer, advanced token/logit/steering/interpretability views, and formally specified collaborative DAG movement.

## Definition of the current milestone

The current milestone is a crash-conscious local editor/storage foundation plus a directly callable raw-model adapter. It is suitable for continued development and targeted local experiments. It is not yet a complete Loom vertical slice because the desktop cannot load a model, run `Weave`, persist streamed candidates, or promote them through the UI. It is not a release candidate.
