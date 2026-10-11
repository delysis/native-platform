# Loom preferences

Renderer-independent conveniences for a remembered local model path and per-project suggestion choices. Values never establish model verification or generation authority. Appearance continues to follow current system/workspace behavior.

The application supplies a settings directory. Each update reloads the current file under a stable sidecar lock and atomically replaces the one current schema. Contention returns `Busy`. Unix storage and lock entries must be regular files with one link; no-follow, nonblocking opens reject symlinks and FIFOs. This does not establish protection against hostile replacement of parent directories or non-Unix named pipes. Filesystem work belongs on a bounded, joined worker. Malformed, incompatible or oversized data is reported and preserved. No legacy readers or migrations are provided.

Storage is bounded to 128 KiB, 1,024 project choices and a 4,096-byte absolute model path. Compare-and-clear preserves newer model choices. Temporary acceptance models cannot become startup preferences. Rust revisions are `u64`; JavaScript DTOs carry decimal strings.

The current Loom Tauri adapter and renderer consume this service. Native application admission covers accepted storage work. Product/native qualification remains separate from component checks. The current EASL presentation draft has no model/suggestion service consumer; no unused worker is added there.

Each successfully acquired transaction has one private, non-clone lock owner.
It attempts explicit unlock at lexical scope exit, after protected work, on
ordinary returns, errors and unwinding. A successfully unlocked transaction is
not prolonged by passive duplicated or inherited descriptors. `Drop` cannot
return an unlock error: it does not panic or replace the operation's publication
result. Normal file close remains the fallback, not a guarantee of prompt release
if unlocking fails while another descriptor survives. The sidecar is not unlinked,
and failed acquisitions never construct the unlocking owner. This is not a
crash/abort cleanup or a universal filesystem/platform qualification guarantee.
