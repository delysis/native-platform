# Loom Native

Loom Native is an early, local-first desktop writing environment for prose and poetry. Active manuscripts remain ordinary UTF-8 files; a hidden `.loom/` sidecar holds revisions, provenance, transient crash-recovery drafts, branch records, and visible-file recovery state.

This repository is an executable development foundation, not a finished release. Editing, storage, model inspection, and private local suggestions are implemented; the former research engine is archived. The desktop ingests bounded project images and has a separate completion-context path: text-bearing files are canonicalized locally, while fully decoded PNG, JPEG, and WAV payloads are retained byte-for-byte for direct Gemma 4 image/audio input. The historical verification snapshot records a real Gemma 4 E2B base Q8 Metal suggestion, caret-ghost acceptance, durable promotion, loaded-model quit, and immediate-relaunch exercise; that receipt does not certify every later build. There are no signed installers, hosted-provider adapters, or release-certified platform backends. Local dictation uses the sibling Speech service; its lifecycle is separate from native image/audio completion context.

See [Implementation status](docs/implementation-status.md) for the exact verified/deferred boundary and current schema policy. [Project format v1](docs/format-v1.md) records the format rationale.

Project storage currently requires Unix private permissions and directory sync.
Other platforms return `private_storage_unsupported` before creating or opening
a project. The unreleased database accepts only its current schema; old stores
are not silently migrated or erased.

## What works now

- Open-folder projects with readable Markdown prose and exact-whitespace UTF-8 verse.
- A `STRICT` SQLite sidecar with foreign keys, WAL, `synchronous=FULL`, immutable semantic records, SHA-256 content-addressed blobs, and a conflict-preserving visible-file outbox.
- Source-revision- and source-blob-bound checkpoints with idempotent command IDs.
- Typed visible-projection receipts: a semantic revision that committed before a projection race is reported as `pending_conflict` or `pending_retry`, never misrepresented as either a pre-commit refusal or a fully saved visible file.
- Two-slot transient draft journaling with monotonic, non-reused versions and atomic checkpoint consumption; it does not manufacture semantic history for every keystroke.
- Provenance-preserving human edits: unchanged generated slices retain their original artifact identity.
- Generation, candidate, terminal-event, selection, authorship, and writer/critic authority records. Private candidates cannot change the active manuscript; explicit promotion is the only implemented candidate-to-manuscript path.
- Idempotent, source-bound generation-family commands, bounded durable branch paging/body reads, explicit interrupted-run recovery, independently cancellable branches, keep-alternative receipts, and conflict-preserving candidate promotion.
- Deterministic UTF-8 three-way merge primitives for prose and verse. Conflicts are structured and byte-ranged; hybrid text is held until block metadata is available.
- An explicit external-change workflow across the store, CLI, typed Tauri IPC, and desktop: bounded three-way preview, structured conflicts, exact revision/blob binding, human resolution, and an idempotent provenance-preserving reconciliation receipt. Loom never chooses or applies a conflicting resolution automatically.
- A Tauri 2/Svelte 5 shell that opens directly into an app-owned ordinary-file note on first launch. One-document projects show the page rather than an empty outline; folder switching, source mode, focus, model setup, and recovery stay secondary until needed.
- Source editing, a lossless Loom visual-Markdown dialect over ProseMirror, literal-tab preservation in both surfaces, exact verse line-ending handling, IME-aware save boundaries, crash-draft recovery, external-change review, and a pinned or Option-held completion lens in the right gutter. The lens cycles the same four cached candidates without hiding the inline ghost or resizing the manuscript. Visual mode reserves raw U+0009 for manuscript indentation rather than CommonMark's ambiguous tab-indented-code form; use Source mode for that external syntax.
- A direct in-process `llama-native-kit` adapter for exact raw completion batches, model capability verification, bounded event forwarding, per-branch cancellation, and provenance conversion.
- Product-owned completion context with a compact text-first toolbar pop-down, persistent pasted steering text, immutable attachment receipts, and content-addressed bytes. Drop files onto the pop-down to apply them to the whole document, or into the editor to insert a visible attachment card whose context begins at that position. Text, Markdown, textual PDF, EPUB, DOCX, XLSX, and related inspected formats are canonicalized locally; each completion selects a small relevant excerpt set under an exact prompt budget and records its source digests and byte ranges. Safe text recovered from a partially inspected long or complex document remains usable and is labeled `excerpted`. PNG, JPEG, and WAV enter Gemma 4 as exact direct media with no OCR or transcription. Structure-only media and textless PDFs fail closed until a separately receipted transform exists.
- Desktop model choose/load/unload, bounded local GGUF discovery, native capability inspection, and conservative model-fit calculation. Model admission chooses a power-of-two context from currently available system memory using a deliberately high unknown-KV-cost estimate, retains system headroom, and lets native inspection clamp that request to the GGUF's trained limit. A GGUF header alone is never represented as proof that a model is loadable or completion-capable.
- An explicit verified-download path with HTTPS-only transport, mandatory SHA-256, a hard byte ceiling, safe partial resume, cancellation/status recovery, cold hash and GGUF verification, and no-clobber installation.
- Under the verified quiet-default build policy, idle autosave triggers local raw continuation from the exact Source or Visual caret unless the author has turned Suggestions off. The earlier build policy retains explicit per-project opt-in. Typing cancels stale work; unmodified Tab accepts ghost text only when it is bound to the current caret, immutable branch bytes, and a live visible editor presentation. Without that exact ghost, the same key inserts a literal tab and remains in the writing surface; Escape dismisses a visible ghost. Reviewable suggestions remain private and recoverable under Writing options, not in the quiet header. There is no manual generation or checkpoint button on the writing surface. Fixture-backed tests cover the command contracts, and the corrected exact-bundle UX receipt is preserved in [docs/audit-receipts/2026-08-11-r4-quiet-editor-ux.md](docs/audit-receipts/2026-08-11-r4-quiet-editor-ux.md).
- A JSON-emitting CLI for project initialization, open, import, checkpoint, recovery, export, read-only reconciliation preview, and identity-bound reconciliation apply.

## Native dependency

Loom uses root-workspace path dependencies in `crates/native`. The root `Cargo.lock` pins external native dependencies; there is no unpublished sibling-repository commit required to resolve the current workspace. Native changes require consumer compatibility checks and separately identified real-model evidence.

## Build and test

The root workspace pins Rust 1.92.0. Node.js and pnpm are also required. From the monorepo root:

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

Run the macOS development desktop app after locked dependencies have been fetched or are available in Cargo's cache:

```sh
pnpm --filter @delysis/loom tauri dev
```

The desktop build defaults to the checked-in `writer-gemma4-base-v2` policy: a local-only, raw-completion writer identity with quiet suggestions as the product default. At runtime Loom first reopens a model the author explicitly selected; otherwise it quietly prefers Google's official `gemma-4-12b-it-qat-q4_0.gguf` artifact when bounded local discovery finds its exact file name and byte length, then falls back to the older build-policy writer. Every candidate still passes native GGUF inspection before use. `writer-gemma4-base-v1` preserves the earlier explicit project-opt-in behavior, and `none-v1` builds without an automatic writer preference. Select one of those exact allow-listed contracts with `LOOM_BUILD_MODEL_POLICY`; arbitrary policy files and model paths are rejected at build time. Model files are always discovered or selected at runtime, so release binaries do not contain paths from the machine that built them.

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
| `crates/loom-store` | Project folders, current schema, blobs, revisions, drafts, outbox/reconciliation recovery, provenance, authority, idempotent generation families, bounded branches, and promotion |
| `crates/loom-host` | Opt-in agency/focus gates, cancellation token, and bounded job queues |
| `crates/loom-backend-llama` | Direct local raw-completion adapter, GGUF discovery/inspection, verified downloader, capability mapping, and fit estimates |
| `crates/tauri-plugin-loom` | Typed desktop IPC for direct default-project opening, editing/reconciliation, model lifecycle/downloads, immutable context attachments, opt-in automatic continuation, durable branches, cancellation/selection, focus mode, and safe close |
| `crates/loom-cli` | Storage, recovery, and external-reconciliation command-line oracle |
| `apps/loom` | Quiet Svelte 5/ProseMirror authoring shell, completion lens, attachment context UI, automatic private-suggestion interaction, and Tauri 2 application |

There is no `loom-backend-fte` crate yet. Generic speech processing, general source indexing, and hosted-provider composition are not implemented Loom product paths. Completion attachments deliberately use Gemma 4's native image/audio inputs where the local inspector can prove a complete payload decode; Loom does not OCR or transcribe them. W9 retired the unowned, unqualified research engine; the protected W8 HOME tag preserves it for archaeology or a future explicitly owned experiment.

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
- Root workflows are `ci-pr.yml`, `ci-full.yml`, and `release-macos.yml`. They enforce the configured policy, tests, and dependency-graph checks; this repository does not currently run a separate secret-scanning or dependency-review workflow. `.gitleaks.toml` is configuration, not evidence that a scan ran.
- General backups, signed update delivery, and platform hardening remain release work.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your option.
