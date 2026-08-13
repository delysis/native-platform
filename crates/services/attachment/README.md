# attachment-native-kit

A safe-Rust attachment ingestion boundary for native AI applications.

It turns caller-provided bytes into a content-addressed object graph, bounded
canonical artifacts, and a capability-aware preparation plan. It does not read
arbitrary paths, open the network, spawn converters, run macros, invoke models,
or decide which conversation owns an attachment.

## Crates

- `attachment-native-types`: versioned graph, budget, artifact, capability,
  plan, blocker, and receipt contracts.
- `attachment-native-inspect`: content-first detection and iterative bounded
  expansion of safe archive/compression formats.
- `attachment-native-document`: bounded conversion of validated documents to
  canonical Markdown or plain text.
- `attachment-native-plan`: deterministic direct-media versus transform
  planning for a concrete model/backend capability profile.
- `attachment-native-host`: immutable processor registry and orchestration.
- `attachment-native-cli`: inspection and planning oracle for tests and apps.

## Default safety posture

- Inputs are bytes already granted by the embedding application.
- Network and subprocess authority do not exist in core crates.
- Magic and parser validation outrank extensions and declared MIME types.
- Recursion uses an iterative queue and one checked cumulative budget.
- Container metadata and decoder windows are preflighted before allocation.
- Archive paths are metadata only; special filesystem entries are reported and
  skipped.
- Duplicate content is hashed and analyzed once while all parent edges remain.
- Partial coverage can never be reported as complete.
- Image, audio, and video policy depends on the selected target capability.
  Unsupported audio can request transcription; unsupported video can request
  audio extraction and frame sampling. Those transforms must be explicitly
  supplied by a downstream adapter.

See [ARCHITECTURE.md](docs/ARCHITECTURE.md), [SECURITY.md](SECURITY.md), and
[FORMAT_SUPPORT.md](docs/FORMAT_SUPPORT.md). The adversarial boundary and
optional decoder interface are specified in [THREAT_MODEL.md](docs/THREAT_MODEL.md)
and [TRANSFORM_ADAPTERS.md](docs/TRANSFORM_ADAPTERS.md).

## CLI oracle

```sh
attachment-native inspect ./sample.pdf
attachment-native plan ./photo.png --target ./target.json
```

Both commands emit versioned JSON contracts. Product apps should call the Rust
host directly; the CLI exists for tests, receipts, and debugging, not as a
runtime subprocess adapter.

## Distribution

The workspace is distributed as a versioned Git repository. Embedding apps pin
an exact commit revision; development checkouts may use a temporary Cargo path
override. The crates are deliberately not published independently to crates.io,
because publishing only part of this security boundary would create an
ambiguous and potentially incompatible dependency graph.
