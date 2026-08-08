# Loom Native

Loom Native is an early, local-first desktop writing environment for prose and poetry. Active manuscripts remain ordinary UTF-8 files; a hidden `.loom/` sidecar holds revisions, provenance, transient crash-recovery drafts, branch records, and visible-file recovery state.

This repository is an executable development foundation, not a finished release. Editing and storage work today. The local llama adapter works as a Rust library and has one qualified real-model smoke test, but generation is not connected to the desktop `Weave` button or branch shelf. There are no signed installers, hosted-provider adapter, model downloader, or Gemma 4 acceptance result.

See [Implementation status](docs/implementation-status.md) for the exact verified/deferred boundary and live migration number. [Project format v1](docs/format-v1.md) records the format rationale.

## What works now

- Open-folder projects with readable Markdown prose and exact-whitespace UTF-8 verse.
- A `STRICT` SQLite sidecar with foreign keys, WAL, `synchronous=FULL`, immutable semantic records, SHA-256 content-addressed blobs, migrations, and a conflict-preserving visible-file outbox.
- Source-revision- and source-blob-bound checkpoints with idempotent command IDs.
- Typed visible-projection receipts: a semantic revision that committed before a projection race is reported as `pending_conflict` or `pending_retry`, never misrepresented as either a pre-commit refusal or a fully saved visible file.
- Two-slot transient draft journaling with monotonic, non-reused versions and atomic checkpoint consumption; it does not manufacture semantic history for every keystroke.
- Provenance-preserving human edits: unchanged generated slices retain their original artifact identity.
- Generation, candidate, terminal-event, selection, authorship, and writer/critic authority records. Private candidates cannot change the active manuscript; explicit promotion is the only implemented candidate-to-manuscript path.
- Deterministic UTF-8 three-way merge primitives for prose and verse. Conflicts are structured and byte-ranged; hybrid text is held until block metadata is available.
- An explicit external-change workflow across the store, CLI, typed Tauri IPC, and desktop: bounded three-way preview, structured conflicts, exact revision/blob binding, human resolution, and an idempotent provenance-preserving reconciliation receipt. Loom never chooses or applies a conflicting resolution automatically.
- A Tauri 2/Svelte 5 shell with project open/create/close, document outline, source editing, a lossless-subset ProseMirror visual editor, exact verse line-ending handling, IME-aware save boundaries, focus mode, crash-draft recovery, and external-change review.
- A direct in-process `llama-native-kit` adapter for exact raw completion batches, model capability verification, bounded event forwarding, per-branch cancellation, and provenance conversion.
- Bounded local GGUF discovery plus conservative model-fit calculation. Discovery does not claim that a model is loadable or completion-capable.
- A JSON-emitting CLI for project initialization, open, import, checkpoint, recovery, export, read-only reconciliation preview, and identity-bound reconciliation apply.

## Important development caveat

`loom-backend-llama` currently depends on three crates through paths that resolve, in this checkout, under `/Users/george/Documents/llama-native-kit`. The repository therefore does **not** build from an arbitrary checkout by itself. Replace those paths with a reviewed, pinned, published revision or an explicitly configured workspace dependency before calling the repository portable or publishing it.

## Build and test

The workspace declares Rust 1.88 as its minimum version, but this snapshot was verified with Rust 1.95 rather than an MSRV job. Node.js and pnpm are also required. From the repository root:

```sh
pnpm install --frozen-lockfile

cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings

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

Run the macOS development desktop app from a checkout with the local llama dependency available:

```sh
pnpm --filter @delysis/loom tauri dev
```

## Real GGUF smoke test

The real-model test is ignored by the normal suite. Supply an absolute local GGUF path explicitly:

```sh
LOOM_GGUF_MODEL_PATH=/absolute/path/to/model.gguf \
  cargo test -p loom-backend-llama \
  adapter::tests::real_gguf_raw_family_acceptance \
  -- --ignored --exact --nocapture
```

This is a developer smoke test, not a portable acceptance suite. The currently recorded local result used a Qwen3 0.6B Q4 model on CPU; it is not Gemma 4 base-model acceptance, acceleration certification, cancellation certification, or cross-platform evidence. Details are in the implementation status document.

## Workspace map

| Path | Current responsibility |
| --- | --- |
| `crates/loom-types` | Versioned identities, artifacts, operations, generation DTOs, commands, events, capabilities, and receipts |
| `crates/loom-document` | Prose/verse/hybrid projection, UTF-8 artifact slices, and bounded three-way text merge |
| `crates/loom-store` | Project folders, migrations, blobs, revisions, drafts, outbox/reconciliation recovery, provenance, authority, candidates, and promotion |
| `crates/loom-context` | Bounded exact-completion prompt recipes and manuscript-boundary validation |
| `crates/loom-search` | Search budgets/state and a small deterministic Pareto-frontier primitive |
| `crates/loom-host` | Opt-in agency/focus gates, cancellation token, and bounded job queues |
| `crates/loom-backend-llama` | Direct local raw-completion adapter, GGUF discovery, verified capability mapping, and fit estimates |
| `crates/tauri-plugin-loom` | Narrow desktop IPC for project/session/document/draft/reconciliation lifecycle, local discovery, focus mode, and safe close |
| `crates/loom-cli` | Storage, recovery, and external-reconciliation command-line oracle |
| `apps/loom` | Svelte 5/ProseMirror authoring shell and Tauri 2 application |

There is no `loom-backend-fte` crate yet. Speech, attachment ingestion, retrieval, evaluation, automation scheduling, and hosted-provider composition are not implemented product paths.

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
- Implemented product paths are local and require no credentials. There is no hosted-provider fallback or telemetry path in this tree.
- Tauri exposes an allowlisted plugin command set under a restrictive CSP. Session, project, document, revision, blob, draft-version, and command identities are checked at write boundaries.
- Project-relative path traversal and document symlinks are refused. External file changes are not overwritten silently.
- Newly created Unix sidecar directories/files request owner-only `0700`/`0600` modes while visible manuscript and pre-existing user permissions are preserved.
- Test fixtures are labeled as fixtures; the llama adapter rejects fixture output presented as live inference.
- `.gitleaks.toml` and a SHA-pinned GitHub security workflow define full-history secret scanning plus pull-request dependency review. No remote run is claimed here, and the workflow covers this repository rather than historical family repositories.
- General backups, signed update delivery, and platform hardening remain release work.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your option.
