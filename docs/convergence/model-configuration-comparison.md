# Mine model configuration

Mine keeps Loom's native model registry, bounded discovery, verified catalog downloads,
and transactional replacement of the selected resident. Mama supplies the useful
configuration surface: an explicit model/projector pair, device choice, context and
batch sizing, and sequence capacity. These choices now live in project-owned
`.mine.toml` and reach Loom's real model loading boundary.

```toml
version = 1

[model]
path = "../models/writer.gguf"
projector_path = "../models/projector.gguf"
# Optional SHA-256 assertions contain exactly 64 hexadecimal digits.
# expected_model_sha256 = "..."
# expected_projector_sha256 = "..."
device = "auto"
context_tokens = 8192
batch_tokens = 512
max_sequences = 4
gpu_layers = -1
```

Omit a setting to inherit the current memory-aware defaults. A relative asset path
resolves against the project directory; absolute paths are accepted. Paths are
literal, with no shell or tilde expansion. Reading settings never loads a model or
downloads assets. The next explicit load freezes one validated configuration.
Editing the file does not silently replace an active resident.
The frozen request includes its project root, so relative projector names cannot
accidentally reuse another project's pair. Repeating an unchanged load keeps the
original memory plan; the current resident's own RAM use cannot make an idempotent
request silently shrink its context.

| Setting | Applied behavior |
| --- | --- |
| `path` | Adds the explicit candidate to bounded discovery and rejects loading any different canonical file. A missing configured model cannot fall back to another model. |
| `projector_path` | Sets the exact native projector and includes its size in context planning. Catalog loads still require the catalog's projector bytes and capabilities. |
| `expected_model_sha256`, `expected_projector_sha256` | Native load-time content assertions. Projector hashes require an explicit projector path. A configured hash cannot override a pinned catalog or writer-policy hash. |
| `device` | `auto`, `cpu`, or `metal`, applied to the native configuration. CPU with omitted GPU layers selects zero offloaded layers. An explicit CPU/nonzero-layer combination or Metal/zero-layer combination is rejected. |
| `context_tokens` | Requested native context capacity, minimum 512. Omission uses Loom's conservative memory heuristic; explicit requests still face native memory admission and trained-model context limits. The inspected descriptor reports the effective capacity. |
| `batch_tokens` | Positive native processing batch size. |
| `max_sequences` | Native parallel sequence capacity from 1 through the native maximum (currently 4). |
| `gpu_layers` | `-1` requests all offloadable layers; nonnegative values request that many. Auto permits CPU fallback when GPU offload is unavailable. |

Unknown fields and invalid combinations fail before registry staging. There are no
decorative `threads`, `mmap`, `mlock`, or flash-attention switches: the current native
model configuration does not consume those controls. Host memory budgets remain
host-level immutable settings, not values a project can pretend to change after
the host has started. Sampling/personas are owned by the separate generation
profile configuration.

## What each implementation contributed

| Class | Mama strength | Loom strength | Mine decision |
| --- | --- | --- | --- |
| Model choice | Explicit model/projector pair retained in frozen run settings; no late sibling discovery for a frozen profile. | Named Hugging Face aliases remain separate from canonical blob paths; strict pinned catalog identity and native capability inspection. | Preserve the chosen alias, freeze the pair at load admission, retain canonical execution identity and every pinned assertion. |
| Runtime controls | Native device, context, batch, and sequence settings have real consumers. | Memory-aware context planning and a previous resident retained until a replacement is verified. | Apply validated dotfile requests before staging; native inspection remains the authority for what actually loaded. |
| Reload/teardown | Exact native configuration is used when resolving a resident. | Joined-worker evidence and explicit unknown-residency states prevent treating a missing slot as successful teardown. | Address the exact configuration and native worker when retiring a model; never retire all residents sharing a file path. |
| Error handling | Frozen profiles avoid later implicit projector changes. | Pinned content checks happen before native loading; failure paths restore previous registry authority. | Invalid settings, conflicting assertions, and missing assets leave the previous registry untouched. Failed acquisition of a new same-file configuration cannot release the old one. |

The integration exposed a path-only teardown defect: the old Loom release path
collected every slot whose model path matched. Different context/device requests
for the same GGUF therefore shared teardown authority incorrectly. The replacement
tracks exact configurations, asks the native host for the matching resident, finds
its slot by worker identity, and requires joined evidence for that worker. New
digest-only assertions are checked against the existing descriptor without staging
that same worker as its own replacement.

The baseline regression receipt is in
`evidence/model-config-release-baseline.txt`. It demonstrates the old runtime's
inability to distinguish an acquired prior configuration from a never-acquired new
configuration at the same path. Fixture tests exercise actual preparation,
configuration, and rollback boundaries; they do not claim a live model reload or
native UI acceptance. Large same-file replacements can still require enough memory
for both residents until verification succeeds; failed native admission retains the
old resident.
