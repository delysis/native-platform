# Loom preferences

Renderer-independent conveniences for a remembered local model path and per-project suggestion choices. Values never establish model verification or generation authority. Appearance continues to follow current system/workspace behavior.

The application supplies a settings directory. Each update reloads the current file under a stable sidecar lock and atomically replaces the one current schema. Contention returns `Busy`. Unix storage and lock entries must be regular files with one link; no-follow, nonblocking opens reject symlinks and FIFOs. This does not establish protection against hostile replacement of parent directories or non-Unix named pipes. Filesystem work belongs on a bounded, joined worker. Malformed, incompatible or oversized data is reported and preserved. No legacy readers or migrations are provided.

Storage is bounded to 128 KiB, 1,024 project choices and a 4,096-byte absolute model path. Compare-and-clear preserves newer model choices. Temporary acceptance models cannot become startup preferences. Rust revisions are `u64`; JavaScript DTOs carry decimal strings.

This reconciliation is not yet connected to product adapters or qualified.
