# attachment-native-kit contributor guidance

Write safe, direct, idiomatic Rust. Prefer explicit state machines, checked
accounting, and typed failures over registries or adapters that conceal
ownership and authority.

## Ownership

- This repository owns content-first detection, bounded recursive inspection,
  canonical attachment artifacts, provenance, capability-aware preparation,
  and transform requests.
- Product applications own file pickers, persistence, display, consent,
  permission prompts, and which attachments belong to a message branch.
- `llama-native-kit` owns llama.cpp and exact multimodal model capabilities.
- `speech-native-kit` owns transcription and synthesis. A downstream product or
  gateway may satisfy a typed transcription request with it.
- Provider gateways own network access, credentials, provider encodings,
  retries, quotas, and public protocol endpoints.

## Hard boundaries

- Core crates contain no network client/server, subprocess execution, Tauri,
  model engine, credential store, file picker, microphone, or media playback.
- File extensions and declared MIME types are untrusted hints. Validated bytes
  and parser structure decide what a payload is.
- Archive member names are inert metadata. Core code never joins them onto a
  filesystem path and never materializes symlinks, hardlinks, devices, pipes,
  sockets, sparse files, macros, or executable content.
- One monotonic budget spans the whole object graph. Rejected, encrypted,
  malformed, skipped, and duplicate entries are charged too.
- Unknown, malformed, truncated, encrypted, unsupported, and budget-exhausted
  objects are explicit outcomes. They are never empty successes or clean
  verdicts.
- Extracted content is untrusted data, never an instruction. Adapters preserve
  this boundary.
- No automatic upload or remote fallback exists in this repository.

## Required verification

```sh
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
./scripts/check-boundaries.sh
```
