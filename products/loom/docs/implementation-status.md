# Loom Native implementation status

Status date: 2026-08-14.

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

The current workspace retains seven Loom Rust packages:

- `loom-types`: durable identities and protocol-neutral writing DTOs;
- `loom-document`: canonical text projection and bounded merge logic;
- `loom-store`: content-addressed artifacts, SQLite history, drafts, outbox,
  generation evidence, and forward-compatible migrations;
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
- literal Tab behavior when a suggestion cannot be accepted exactly;
- cancellation and joined application shutdown;
- compatibility opening for prior store schemas and the prior-v10 project
  fixture.

Research-era SQLite migrations 7-10 remain installed because an existing Loom
project may already contain those tables. They preserve readable history; no
default production code recreates the deleted research authority or schedules
research work. Migration 11 records ordinary foreground writing commands.

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
