# Native autoresearch foundation checkpoint

## Current integration status

The historical checkpoint inventory below records the exact foundations used
while the native research crates were developed. The current unified Loom tree
has since merged the quiet manuscript editor and native autoresearch work and
pins every `llama-native-kit` consumer to
`2d69f086e922ed7bdfd6236baf5a1ad0ed568360`. That revision is the current
product boundary; the older native revisions remain listed only to explain the
evidence and decisions made at this checkpoint. Current static workspace and
frontend gates pass after the merge. Real-model results below remain historical
until the corresponding ignored tests are rerun against the current pin.

The first native autoresearch slice was based on clean, published component
checkpoints. These exact commits are provenance records, not floating branch names:

- Loom Native initial foundation parent: `82681fc120dbbc8ea0cfd9f9025db44e63e1e571`
- Loom Native current integration base: `1e39e05b31d04f70af50721f2225631b68587106`
- Loom Native latest clean product checkpoint awaiting integration rebase: `9aed804b32aeebf793090749732ed02dbb03fcbb`
- llama-native-kit inference baseline: `c61692d48b0768bb242bcecb7a80c3318fc476b4`
- withdrawn llama-native-kit interim revision: `9f0783eb57141685681fd3847623ef2b8fde653b`
- joined-shutdown native baseline: `b71dfaa16c77b7069259bd15add740b80f895017`
- published controlled-generation candidate: `6a82439ee449599f7a7e477e1150ae29efdb23d6`
- llama.cpp Rust wrapper: `01e48b7c1e7de39c3e5e8a67cd9efac498f8da1f`
- Mom Llama: `30ce93ff4d5f8f0ab5e39da98fd2359df5ac5c13`
- Free Token Energy: `9d98d6e0c079e5730cb8f5cd0a71cc89d22c96fe`
- legacy Python research evidence: `cacbcc9689101d5b774d25c0acbf6857ac719c27`

The `9f0783e` revision is retained here only because unpublished worktree
evidence was produced against it. It was withdrawn after a real loaded-Gemma
Metal Loom process aborted during macOS application teardown with a model
worker still alive. Its successful CPU inference runs are diagnostic, not
publication authority. `b71dfaa` replaces detached worker lifecycle with
host-owned joins, an affine exact-host graceful shutdown proof, and an
infallibly joined process-exit drain. `6a82439` builds on that lifecycle with
verified controlled generation and embeddings. It is published and
real-model-tested, but remains a candidate for this Loom branch until the
current Loom handoff is rebased and its complete consumer gates pass.

Serializable research records are declarations and diagnostic evidence. They
never regain live admission through deserialization. `loom-inference` alone
consumes the native backend's opaque generation seal and mints a
`VerifiedInferenceEnvelope`. `loom-store` may persist and adopt that envelope;
it must never recreate authority from receipt fields, JSON, hashes, or replay
of these serializable records.

The test-only Rust legacy translator pins the preserved invalidation ledger and
its referenced bytes. It reconstructs 111 historical assemblies exactly but
keeps them unbound and quarantined; 17 additional records are rejected because
at least one extracted model span is empty. The older planning estimate of 108
reconstructable records is therefore superseded by the audited 111/17 split.

The native consumer boundary is proven with the pinned 484,220,320-byte Qwen
0.6B GGUF whose SHA-256 is
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`.
The ignored real-model gates run direct in-process inference both through
`loom-backend-llama` and through `loom-inference`'s move-only verified batch
bridge. Fixture, compile-only, and pre-edit runs do not satisfy this gate.
The withdrawn `9f0783e` Loom run is not reused. At `6a82439`, the exact Qwen
artifact passed native controlled generation and per-token embedding isolation,
including byte-equivalent disabled-control output, batched same-model CFG,
bounded distribution evidence, JSON/GBNF constraints, cancellation rejection,
and joined-worker seal verification. The same tests passed on the exact
4,954,576,032-byte Gemma 4 E2B base Q8 artifact with SHA-256
`aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`.
These are native-substrate facts, not yet a Loom end-to-end campaign claim; the
small-GGUF frozen multi-movement trial must be rerun after Loom consumes the
published revision.

Free Token Energy and Mom Llama remain atomically pinned to their earlier
native stack while a coordinated `6a82439` consumer checkpoint is under
review. A dependency-only repin is not acceptable. The FTE checkpoint must
atomically snapshot its host and model registry, distinguish borrowed from
owned host lifecycle, close admission, cancel and join every actual native and
hosted bridge task, and complete plugin cleanup at Tauri's exit boundary. Mom
alone owns the final process-exit drain for its borrowed native host. Repinning
either consumer remains blocked until the same-resident chat/raw/cache proof,
active cancellation and gateway drain, loaded-model macOS quit, and immediate
relaunch all pass on immutable published commits.
