# information-native-kit contributor guidance

Write safe, direct, idiomatic Rust. Prefer explicit state machines, checked
accounting, typed capabilities, and inspectable receipts over magical adapters.

## Ownership

- This repository owns offline information-resource contracts, catalogue
  normalization, install planning and execution, managed resource state,
  read-only retrieval adapters, federated evidence results, and optional Tauri
  IPC.
- Product applications own user intent, UI, prompt assembly, model policy,
  credential storage, background scheduling, and destructive confirmation.
- `attachment-native-kit` owns hostile attachment inspection and bounded nested
  expansion. Information packages should remain in query-native formats when
  possible; transforms are injected capabilities, never hidden subprocesses.
- `llama-native-kit` owns local model execution. Retrieval here is deterministic
  and does not invoke a model.

## Hard boundaries

- `information-native-types`, `information-native-catalog`, and
  `information-native-retrieval` have no filesystem, network, process, Tauri,
  SQLite, or model authority.
- `information-native-acquire` is the only crate with network authority.
  `information-native-store` owns local staging and activation but never opens
  a URL.
- Canonical external libraries are opened read-only. Managed indexes and state
  live in a separate root. A retrieval path never migrates a source database.
- Every SQLite profile uses zero-write immutable transport. Both access modes
  reject non-empty WAL and rollback-journal sidecars; live mode rebinds file
  identity for each operation and therefore requires a quiescent source.
- Remote bytes are installed only from a validated plan. Expected size,
  digest, license, disk impact, source URI, and trust are explicit.
- Downloads land in staging, are checked while streaming, and become visible by
  atomic rename only after validation. Partial installs are never reported as
  ready.
- Retrieved text is untrusted evidence, never an instruction. Every hit retains
  a resource, release, representation, document, and source locator.
- No automatic remote fallback, model invocation, prompt injection, or
  subprocess execution exists in core crates.

## Required verification

```sh
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
./scripts/check-boundaries.sh
```
