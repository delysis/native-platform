# Loom Native

Loom Native is an early, local-first desktop writing environment for prose and poetry. Active manuscripts remain ordinary UTF-8 files; a hidden `.loom/` sidecar holds revisions, provenance, transient crash-recovery drafts, branch records, and visible-file recovery state.

This repository is an executable development foundation, not a finished release. Editing, storage, model inspection, private local suggestions, and the headless native fiction-research kernel work today. The exact quiet desktop has completed a real Gemma 4 E2B base Q8 Metal suggestion, caret-ghost acceptance, durable promotion, loaded-model quit, and immediate-relaunch exercise. There are no signed installers, hosted-provider adapter, attachment or speech adapters, or release-certified platform backends.

See [Implementation status](docs/implementation-status.md) for the exact verified/deferred boundary and live migration number. [Project format v1](docs/format-v1.md) records the format rationale.

## What works now

- Open-folder projects with readable Markdown prose and exact-whitespace UTF-8 verse.
- A `STRICT` SQLite sidecar with foreign keys, WAL, `synchronous=FULL`, immutable semantic records, SHA-256 content-addressed blobs, migrations, and a conflict-preserving visible-file outbox.
- Source-revision- and source-blob-bound checkpoints with idempotent command IDs.
- Typed visible-projection receipts: a semantic revision that committed before a projection race is reported as `pending_conflict` or `pending_retry`, never misrepresented as either a pre-commit refusal or a fully saved visible file.
- Two-slot transient draft journaling with monotonic, non-reused versions and atomic checkpoint consumption; it does not manufacture semantic history for every keystroke.
- Provenance-preserving human edits: unchanged generated slices retain their original artifact identity.
- Generation, candidate, terminal-event, selection, authorship, and writer/critic authority records. Private candidates cannot change the active manuscript; explicit promotion is the only implemented candidate-to-manuscript path.
- Idempotent, source-bound generation-family commands, bounded durable branch paging/body reads, explicit interrupted-run recovery, independently cancellable branches, keep-alternative receipts, and conflict-preserving candidate promotion.
- Deterministic UTF-8 three-way merge primitives for prose and verse. Conflicts are structured and byte-ranged; hybrid text is held until block metadata is available.
- An explicit external-change workflow across the store, CLI, typed Tauri IPC, and desktop: bounded three-way preview, structured conflicts, exact revision/blob binding, human resolution, and an idempotent provenance-preserving reconciliation receipt. Loom never chooses or applies a conflicting resolution automatically.
- A Tauri 2/Svelte 5 shell that opens directly into an app-owned ordinary-file note on first launch. One-document projects show the page rather than an empty outline; folder switching, source mode, focus, model setup, and recovery stay secondary until needed.
- Source editing, a lossless-subset ProseMirror visual editor, exact verse line-ending handling, IME-aware save boundaries, crash-draft recovery, external-change review, and a temporary alternatives dialog that never resizes the manuscript.
- A direct in-process `llama-native-kit` adapter for exact raw completion batches, model capability verification, bounded event forwarding, per-branch cancellation, and provenance conversion.
- Desktop model choose/load/unload, bounded local GGUF discovery, native capability inspection, and conservative model-fit calculation. A GGUF header alone is never represented as proof that a model is loadable or completion-capable.
- An explicit verified-download path with HTTPS-only transport, mandatory SHA-256, a hard byte ceiling, safe partial resume, cancellation/status recovery, cold hash and GGUF verification, and no-clobber installation.
- Under the verified quiet-default build policy, idle autosave triggers local raw continuation from the exact Source or Visual caret unless the author has turned Suggestions off. The earlier build policy retains explicit per-project opt-in. Typing cancels stale work; Tab accepts only ghost text bound to the current caret, immutable branch bytes, and visible editor presentation, while Escape dismisses it. Alternatives remain private and recoverable. There is no manual generation or checkpoint button on the writing surface. Fixture-backed tests cover the command contracts, and the exact real-model desktop receipt is preserved in [docs/audit-receipts/2026-08-11-real-gemma-desktop.md](docs/audit-receipts/2026-08-11-real-gemma-desktop.md).
- A safe-Rust, headless fiction-research stack with bounded pack/manifests, exact prompt and source evidence, verified writer admission, assemblies and projections, immutable research storage, hard gates and pairwise evaluation, frozen trials, resumable campaigns, sealed benchmarks, QD archives, deterministic learned heads, and a diagnostic-only frontier-critic adapter. It is not yet a complete product campaign UI or a set of empirically qualified frozen profiles.
- A JSON-emitting CLI for project initialization, open, import, checkpoint, recovery, export, read-only reconciliation preview, and identity-bound reconciliation apply.

## Pinned native dependency

The inference, evaluation, and trial crates pin their `llama-native-kit` dependencies to published commit `2d69f086e922ed7bdfd6236baf5a1ad0ed568360`. Builds therefore require fetching that exact Git revision unless dependencies are already cached. The pin includes the product-neutral controlled-generation and embedding APIs, executor-owned operation leases, immutable build identity, exact artifact verification, NativeHost residency policy, and joined shutdown authority used by the desktop lifecycle. Changing it requires complete consumer compatibility and real-GGUF revalidation.

## Build and test

The workspace declares Rust 1.88 as its minimum version. The full workspace check passed locally with Rust 1.88, and CI runs the same minimum-version check on Linux alongside current-toolchain Rust jobs on Linux, macOS, and Windows. Node.js and pnpm are also required. From the repository root:

```sh
pnpm install --frozen-lockfile

cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings

pnpm --filter @delysis/loom test
pnpm --filter @delysis/loom check
pnpm --filter @delysis/loom build
```

The strict Clippy command is a release gate. Consult the verification snapshot in [docs/implementation-status.md](docs/implementation-status.md) rather than assuming every command above has passed on every platform.

Run the storage/recovery CLI:

```sh
cargo run -p loom-cli -- --help
cargo run -p loom-cli -- init /absolute/path/to/Novel --name "Novel"
```

Run a frontend-only preview, which has no native IPC:

```sh
pnpm --filter @delysis/loom dev
```

Run the macOS development desktop app after the pinned native dependency has been fetched or is available in Cargo's cache:

```sh
pnpm --filter @delysis/loom tauri dev
```

The desktop build defaults to the checked-in `writer-gemma4-base-v2` policy: a local-only, raw-completion writer identity with quiet suggestions as the product default. `writer-gemma4-base-v1` preserves the earlier explicit project-opt-in behavior, and `none-v1` builds without an automatic writer preference. Select one of those exact allow-listed contracts with `LOOM_BUILD_MODEL_POLICY`; arbitrary policy files and model paths are rejected at build time. Model files are always discovered or selected at runtime, so release binaries do not contain paths from the machine that built them.

## Real GGUF acceptance tests

The real-model test is ignored by the normal suite. Supply an absolute local GGUF path explicitly:

```sh
LOOM_GGUF_MODEL_PATH=/absolute/path/to/model.gguf \
  cargo test -p loom-backend-llama \
  adapter::tests::real_gguf_raw_family_acceptance \
  -- --ignored --exact --nocapture
```

This runtime-only variable is deliberately not a build input. The generic developer smoke test is not a portable acceptance suite. Its historical local runs used a Qwen3 0.6B Q4 model on CPU; by itself it is not the Gemma 4 base-model gate, acceleration certification, cancellation certification, or cross-platform evidence. Details are in the implementation status document.

The stricter pinned Gemma 4 E2B base Q8 test binds the exact expected model digest and verifies raw completion capability, two independent branch seeds, generated token IDs, live-inference evidence, exact prompt identity, and measured shared-prefix reuse:

```sh
LOOM_GEMMA4_E2B_BASE_PATH=/absolute/path/to/gemma-4-E2B-base-Q8_0.gguf \
  cargo test -p loom-backend-llama \
  adapter::tests::real_gemma4_e2b_base_raw_family_acceptance \
  -- --ignored --exact --nocapture
```

That test passed locally on CPU. A companion real-model test in the pinned native-kit also passed its ordered raw batch and independent per-branch cancellation checks. The separate desktop receipt exercised Metal inference, a caret-local suggestion, promotion, persistence, and quit/relaunch; none of these results is throughput, signed-release, or cross-platform acceleration certification.

## Workspace map

| Path | Current responsibility |
| --- | --- |
| `crates/loom-types` | Versioned identities, artifacts, operations, generation DTOs, commands, events, capabilities, and receipts |
| `crates/loom-document` | Prose/verse/hybrid projection, UTF-8 artifact slices, and bounded three-way text merge |
| `crates/loom-store` | Project folders, migrations, blobs, revisions, drafts, outbox/reconciliation recovery, provenance, authority, idempotent generation families, bounded branches, and promotion |
| `crates/loom-context` | Bounded exact-completion recipes, exact-excerpt retrieval/ranking, and anti-copy evidence primitives |
| `crates/loom-search` | Bounded budgets/state, hard gates, candidate grouping/clustering, rubrics, pairwise disagreement, and quality-plus-novelty selection |
| `crates/loom-research-types` | Bounded research manifests, graph/state, treatment, assembly, archive, benchmark, and evidence contracts |
| `crates/loom-inference` | Backend-neutral writer/controller contracts and move-only verified inference admission |
| `crates/loom-eval` | Evidence-bound gates, rubrics, blind pairwise trials, aggregation, uncertainty, and N-curves |
| `crates/loom-learning` | Deterministic safe-Rust projection rankers, reward heads, calibration, and safetensors interchange |
| `crates/loom-trial` | One frozen treatment, immutable attempts, exact dependencies, and budget reconciliation |
| `crates/loom-campaign` | Resumable exploratory scheduling, successive halving, pressure curves, and MAP-Elites archives |
| `crates/loom-benchmark` | Search-independent sealed confirmatory execution and qualification journals |
| `crates/loom-eval-codex` | Optional subprocess frontier-critic diagnostics with pinned identity and fail-closed JSONL/evidence validation |
| `crates/loom-host` | Opt-in agency/focus gates, cancellation token, and bounded job queues |
| `crates/loom-backend-llama` | Direct local raw-completion adapter, GGUF discovery/inspection, verified downloader, capability mapping, and fit estimates |
| `crates/tauri-plugin-loom` | Typed desktop IPC for direct default-project opening, editing/reconciliation, model lifecycle/downloads, opt-in automatic raw continuation, durable branches, cancellation/selection, focus mode, and safe close |
| `crates/loom-cli` | Storage, recovery, and external-reconciliation command-line oracle |
| `apps/loom` | Quiet Svelte 5/ProseMirror authoring shell, automatic private-suggestion interaction, and Tauri 2 application |

There is no `loom-backend-fte` crate yet. Speech, attachment ingestion, general source indexing, and hosted-provider composition are not implemented Loom product paths. The research engine is headless-first and persistable, but it has not yet run the complete five-function qualification program or produced the three required frozen frontier profiles and Studio views.

## Project layout

```text
Novel/
  manuscript/
  sources/
  assets/
  .loom/
    project.json
    loom.sqlite3
    blobs/sha256/...
    drafts/...
    indexes/
    backups/outbox/...
```

Visible files are authoritative for the active manuscript. `.loom/` is authoritative for history, evidence, drafts, and recoverable alternatives. Removing `.loom/` leaves active manuscript files readable but destroys those sidecar-only records.

## Safety and privacy boundary

- Project-owned Rust crates use `#![forbid(unsafe_code)]`. Native inference still depends on external FFI-bearing dependencies outside that unsafe-code boundary.
- Editing and inference are local and require no credentials. The model manager contacts the network only for an explicitly submitted HTTPS download with an author-supplied digest and limit. There is no hosted-inference fallback or telemetry path in this tree.
- Tauri exposes an allowlisted plugin command set under a restrictive CSP. Session, project, document, revision, blob, draft-version, and command identities are checked at write boundaries.
- Project-relative path traversal and document symlinks are refused. External file changes are not overwritten silently.
- Newly created Unix sidecar directories/files request owner-only `0700`/`0600` modes while visible manuscript and pre-existing user permissions are preserved.
- Test fixtures are labeled as fixtures; the llama adapter rejects fixture output presented as live inference.
- `.gitleaks.toml` and a SHA-pinned GitHub security workflow define full-history secret scanning plus pull-request dependency review. No remote run is claimed here, and the workflow covers this repository rather than historical family repositories.
- General backups, signed update delivery, and platform hardening remain release work.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your option.
