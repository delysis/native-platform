# W1 Native fake-owner quit and relaunch — 2026-08-12

This test-only descendant freezes the Native portion of W1 protocol row 8
against production source `897dd86a961707c66021d1eaabcfd19314cb05f7`.
The source identity is the exact `git ls-tree -r` byte listing for
`crates/llama-native-engine/src/lib.rs` and
`crates/llama-native-engine/src/operation_registry.rs` at that commit. CI also
requires those production files to remain byte-for-byte unchanged from the
baseline. The vertical dependency remains pinned to corrected merged protocol
`fc24ffff08c52690390b4460f44617d5d9732563`
(`w1-vertical-protocol-v0-2026-08-12-r2`), while the accepted lifecycle-contract
pin remains `cbab33555ab9355a6ac453d659c55ec9e0666821`.
`fixtures/w1/MANIFEST.sha256` authenticates all eight W1 bundle members and
itself has SHA-256
`fea8c39a478f95b729053e7f30d2f571f11fe1b20e4591f3efcb039637bc3a0d`.

The deterministic fixture constructs the production `NativeModelOwner` around
a fake llama.cpp owner thread and uses the production `RequestRegistry` for a
separately tracked operation worker. Quit first closes admission and requests
cancellation. The test then observes the one authoritative cancelled terminal,
fsyncs its receipt, releases and reaps the operation worker, and consumes
`shutdown_joined`. Acceptance requires the exact expected and joined worker ID
sets to match, with no active operation or retained task.

The first store is dropped and reopened at the same path. A newly constructed
owner must have a distinct worker identity, complete a new controlled operation,
and repeat the terminal, release, reap, and joined checks. Four deterministic
JSONL receipts bind the ordered outcome: the post-quit store is 476 bytes with
SHA-256 `ed08959fda1a0699d803369c7ed6c23e7dcbe395a3913ed5eeb2b07efe524f9d`;
the final store is 968 bytes with SHA-256
`22c09f5d9269b50e342f873bff427bc9cc00d15b020a0f5c2b63a12a2efaac93`.

Local verification with Rust 1.92.0 passed the focused row-8 tests (2 passed),
the feature-complete workspace suite (182 passed, 9 ignored), workspace Clippy
with all targets and warnings denied, rustfmt, the immutable dependency and
architecture gate, the exact source binding, and the eight-member SHA-256
ledger.

This is reproducible model-free fixture evidence. It does not load or infer
through a real GGUF, relaunch an operating-system process, exercise downstream
product UI quit behavior, or claim a downstream product's durable-store format.
