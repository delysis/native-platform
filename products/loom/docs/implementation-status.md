# Loom Native implementation status

Status date: 2026-09-09.

This document describes Loom's lean shipping surface after W9. Historical
research engines, migration adapters, and candidate plans remain available in
Git history and the protected W8 tag; they are not part of the current default
build.

## Shipping product

Loom is a local-first macOS writing application. Its accepted foreground path
is:

```text
open or create project
    -> edit manuscript
    -> checkpoint durable source
    -> request a local continuation
    -> inspect or promote the exact suggestion
    -> quit and reopen without losing accepted work
```

The current workspace contains these Loom components:

- `loom-types`: durable identities and protocol-neutral writing DTOs;
- `loom-document`: canonical text projection and bounded merge logic;
- `loom-store`: content-addressed artifacts, SQLite history, drafts, outbox,
  generation evidence, and strict current-schema identity;
- `loom-host`: product admission, generation lifecycle, and cancellation;
- `loom-backend-llama`: the in-process `llama-native-kit` adapter;
- `loom-cli`: storage and reconciliation oracle;
- `tauri-plugin-loom`: Loom's product-owned desktop command boundary.

The `loom-app` binary and Svelte frontend compose those packages. The default
shipping graph contains no research scheduler, frontier search, evaluator,
benchmark, or research-inference package.

## Preserved behavior

The lean pass preserves these product contracts:

- byte-conscious UTF-8 manuscript storage with explicit prose and verse
  semantics;
- source-bound, idempotent checkpoints and conflict-preserving projection;
- bounded two-slot crash-recovery drafts;
- immutable content-addressed artifacts and causal revision provenance;
- exact local-model binding and fail-closed automatic-generation authority;
- caret-bound ghost suggestions that cannot mutate the manuscript until an
  explicit promotion command succeeds;
- a completion lens that preserves the visible ghost while cycling the same
  four cached candidates, and can be held transiently or pinned in the gutter;
- immutable, content-addressed completion attachments with document-wide or
  caret-prefix scope, bounded canonical text, and exact PNG/JPEG/WAV media
  bindings for Gemma 4 without OCR or transcription;
- literal Tab behavior when a suggestion cannot be accepted exactly;
- cancellation and joined application shutdown.

The unreleased store uses one current schema. Research tables and old upgrade
paths are retired. Opening an incompatible database reports a failure without
rewriting it; ordinary manuscript bytes remain available. Private project
storage requires Unix permissions and directory synchronization. Other
platforms return an explicit unsupported result before mutation.

## Completion recovery boundary

The renderer rebuilds its newest completion shelf through one project/session/
document-scoped `completion_snapshot` read. That response is a bounded join of
existing `loom-store` branch projections and the process-local generation
registry/supervisor: durable terminal and candidate rows remain terminal truth,
while active operation phase, cancellation, attempt identity, and progress
sequence remain supervisor facts. Previously observed active run IDs may be
included in the same read so a terminal that fell behind the newest page is
recovered exactly.

Generation events are lossy wakeups for that read. They do not supply renderer
text, candidate, or lifecycle authority, and this boundary adds no table or
second lifecycle state machine. The deterministic fixture coverage is
model-free characterization and recovery evidence; it is not a new real-model
or native desktop acceptance receipt.

## Attachment context boundary

The attachment-native document inspector remains the authority for detection,
canonicalization, and preparation plans. Loom requires every executed plan step
to stay local and forbids process, model, OCR, transcription, network, or
unreceipted transform steps. A partial inspection is accepted only when that
inspection produced safe canonical text; the UI labels it `excerpted` and keeps
the exact coverage reasons in the immutable receipt. Fully decoded PNG, JPEG,
and WAV payloads are stored byte-for-byte and bound into the native multimodal
request by digest, kind, MIME type, and size.

The toolbar's top-pane control opens a compact pop-down for persistent pasted
steering text and document-wide attachments. The context document switches
between the same lossless Markdown and visual editing idiom as the manuscript;
dropping or choosing a file inserts its visible attachment reference there.
Text-bearing references are locally canonicalized behind that node, while PNG,
JPEG, and WAV nodes retain exact native media payloads. Inline attachment markers are
visible editor content and apply only when they occur before the completion
caret. The sidecar persists pasted text, global selections, and immutable import
manifests; renderer paths never become inference authority. Canonical source
text is stored as a separate content-addressed object rather than inflated into
manifest JSON. Each generation ranks bounded source chunks against the recent
manuscript, admits only an exact text budget, and receipts the selected source
digests and byte ranges. The recent manuscript window is independently bounded,
so a large attachment cannot silently crowd the author's current prose out of
the model context.

The resident llama.cpp context is selected at model load rather than fixed at
8,192 tokens. Loom samples currently available and total system memory, reserves
the model, projector, conservative runtime overhead, and system headroom, then
selects the largest safe power-of-two tier up to the model's known or native-
clamped trained limit. Generation packing reserves all branches' output cells
and transport overhead first. If the remaining conservative byte budget cannot
hold the manuscript, it preserves the opening and the live tail, inserts an
explicit omitted-middle marker, and receipts the exact omitted byte range.
Attachment relevance is recalculated only at coarse manuscript-growth epochs,
so ordinary end-typing keeps the prepend bytes stable while long-document
growth eventually refreshes them. Across consecutive text families, the native
model worker retains sequence zero, computes an exact token LCP against every
new branch, crops the old tail, and copies the reusable prefix to sibling
branches. Media, caller-supplied caches, exact-token authority submissions,
errors, sequence-zero cancellation, cache mutation, worker replacement, and
fingerprint mismatch all fail closed to a fresh prefill. Receipts distinguish
resident-prefix reuse from newly decoded within-family sharing.
Formats that are only structurally inspected, including image-only PDF pages
and compressed audio without a complete payload decoder, remain blocked rather
than being silently OCRed, transcribed, or relabeled as direct media.

## Local verification

The final Rust source checkpoint for the W1 compatibility retirement is commit
`17c59f51ce55b86f4b5bbbe57d79ebe9a5963a7f`. On an Apple-silicon macOS host:

| Check | Result |
| --- | --- |
| `cargo check --locked --workspace --all-targets` | passed |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | passed |
| `cargo test -p xtask` | passed |
| focused prior-v10 open and exact suggestion promotion/reopen tests | passed |
| `pnpm --filter @delysis/loom test` | 31 files, 178 tests passed |
| `pnpm --filter @delysis/loom check` | 0 errors and 0 warnings |
| `pnpm --filter @delysis/loom build` | passed |

A final all-policy run reached compilation but exhausted the host volume while
creating Cargo artifacts. That is an environment result, not a passing full
policy receipt. The source-equivalent workspace check, strict Clippy, focused
behavioral tests, frontend suite, and an earlier broad workspace test run all
passed; W9's receipt records the exact boundary.

## Evidence limits

The local checks do not establish signed distribution, notarization, updater
behavior, long-duration performance, exhaustive crash injection, general
screen-reader or IME certification, or non-macOS runtime support. Linux and
Windows are compatibility follow-ups rather than W9 release blockers.

Real-model receipts in Git history remain useful provenance for the exercised
model and machine. They are not universal performance or hardware claims.

## Deliberately absent from the default product

- autonomous research campaigns and durable frontier search;
- evaluator, benchmark, qualification, or quality-diversity orchestration;
- generic speech integration;
- hosted inference or network model discovery;
- a compatibility promise for deleted W1 migration packages;
- background services that are not needed by the foreground writing journey.

Future work should add one user-visible capability at a product-owned boundary.
It should not restore migration-only packages or make historical research tables
active merely because their schemas remain readable.
