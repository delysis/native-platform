# Shared model discovery and projector candidates

Compared Mom `models.rs`, Loom backend `discovery.rs` and `model.rs`, Loom plugin
model selection, `model_catalog.rs` and `model_download.rs`, and
`desktop-model-defaults` at native-platform `1cee70d53605ba3bfba6a479e15eda308066ad21`.

## Best-of-both decisions

| Concern | Mom | Loom | Replacement |
|---|---|---|---|
| Cache location | One standards-ordered HF cache root, including legacy HF variable, XDG, Windows home variants; ignores empty variables | Independently combined HF_HUB_CACHE, HF_HOME and HOME; an empty root could mean the current directory | Both discovery paths now consume the existing shared desktop cache precedence. Explicit discovery options can still name multiple authorized roots. |
| Filesystem work | Recursive depth/model-count bound, but no bound on non-model entries | Breadth-first visited-entry/depth bound, but collected an entire directory before enforcing it | `desktop-model-discovery` counts entries as soon as they are queued, including read errors. Directory enumeration, pending paths, metadata reads and diagnostics are all bounded. One sentinel read per directory detects truncation. |
| Candidate identity | Preserves selected/cache alias and canonical deduplication; selected or resident missing files remain visible | Explicit selected path plus canonical target and discovery source | Keep Loom's richer candidate type and user-choice precedence. Both production paths use the same scanner. File aliases are retained; directory symlinks are never traversed. |
| Container diagnostics | Listing did not inspect the header | Returns verified magic, invalid four-byte magic, or read failure | Preserve typed header diagnostics in the shared scanner and add an optional header record to Mom's model listing. `Verified` means magic only, not model readiness. |
| Primary model classification | Excludes projector and MTP names | General GGUF discovery, used for local catalogs and candidate inspection | Keep general discovery of GGUF artifacts. Share a primary-model predicate for Mom's picker, including non-prefix `mmproj` names. No filename is treated as architecture proof. |
| Projector pairing | Zero/one sibling candidate or typed ambiguity; a 256-entry bound | Exact pinned catalog filename beside the selected alias, followed by size/hash/native checks | Shared module exposes both narrow policies. Unique-sibling scanning rejects partial reads and ambiguity. Exact catalog path construction rejects separators and traversal. Neither policy claims filename matching proves compatibility. |
| Default identity | Uses shared immutable Gemma constants and snapshot lookup | Uses the same constants in richer catalog/license/download/compatibility projections | Already converged; keep one existing default identity source. No second catalog constant set. |
| Selection | Latest intent wins for default selection; version-checked conversation profile mutation; immutable Persona overrides | Single selected-model registry, staged verification, exact policy identity, previous-model preservation and explicit uncertain residency | Preserve these complementary transaction semantics. Sharing a scanner must not flatten per-conversation execution identity into one global mutable model. |
| Download | No product download implementation | Bounded native Rust HTTPS download, pinned artifacts, resumability, exact hashes, idempotent reservation, cancellation and private model library | Unique Loom capability, not duplicate code. Not copied into discovery or implicitly granted to Mom. |

## Replaced code and boundaries

The entire Loom backend discovery implementation moves into the shared crate and
its old module is removed. Mom's recursive cache scanner and independent
projector/primary-model recognition are removed. Product adapters retain only
projection and product-specific blocker wording. Loom's catalog sibling resolver
now uses the shared checked path constructor. The native engine and host remain
the only model loading and capability authority.

Mom still returns its existing model-list array for current consumers. Optional
container diagnostics enrich each row. Partial discovery is recorded in command
receipt next-actions; detailed per-path warnings remain available from the shared
scanner and Loom's existing report. No GUI change or graphical acceptance is
claimed. A bounded truncated scan is explicitly partial; the filesystem iterator
may choose a different subset when a directory is over budget. Complete scans
retain sorted deterministic output.

Directory symlink avoidance is preserved, including suffixes that look like GGUF
files. A file symlink remains a permitted local candidate and retains its named
snapshot alias. This is candidate discovery; native held-file identity, hashing,
projector compatibility and admission still have to be established before use.

## Validation

Shared filesystem tests cover a wide directory of non-model entries, canonical
alias deduplication with explicit-user precedence, skipped directory symlinks,
invalid GGUF headers, incomplete traversal, projector/MTP classification,
catalog path traversal, bounded sibling pairing and ambiguity. The inherited
real-cache test moves with its source and remains ignored with the same explicit
non-acceptance boundary. Product model/projector tests and compilation cover both
adapters. No network requests, real inference, model downloads, or UI acceptance
were performed for this change.
