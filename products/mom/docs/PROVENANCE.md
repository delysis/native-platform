# Adopted source provenance

## Standalone product extraction

On 2026-08-08 the product-owned paths were history-preservingly filtered from
`delysis/llama-native-kit` commit
`d8696e323160c9dc6d5f286e68db7e671e7077e0` into this repository. The source
commit is retained by tag `mom-llama-extraction-source-2026-08-08`; exact
source-to-filtered commit mappings and the selected-tree inventory digest are
recorded in [`provenance/extraction.json`](provenance/extraction.json).

The split changes ownership, not data identity. Existing app-data directories,
environment variables, the Tauri identifier and the Keychain service remain
unchanged until an independently tested additive migration is available.

The initial standalone workspace was assembled on 2026-07-29 from three local
development branches. Adoption does not inherit their readiness claims.

| Source | Commit | Adopted material |
| --- | --- | --- |
| `delysis/capability-system-compiler` | `2c2f39d2ead80297758a3f8963a2f5d1291616ca` plus its inspected working tree | Native DTOs, llama.cpp worker, encrypted store, consult runtime, CLI, and Maud/Tauri application |
| `delysis/mom-llama-portkit-scaffold` | `48c4ec328d08a680556f5f72884423e6a086bc37` plus its inspected working tree | Product hardening requirements, parity inventory, visual reference, and consult labeling policy |
| `delysis/coop-forge` | `bda0017bd49d72c6ca9d384ad4aca1c37ba1f75b` plus its inspected working tree | Versioned command-evidence reducer |

The combined adopted Rust source inventory had SHA-256:

`f27104ac3b9ec51beed0b1891f4853407af5ecc88ca3d6433a09ee170362cc58`

All code is reverified in this repository. Historical receipts that are not
bound to this repository's source hash are informative only.

## Subsequent read-only uplifts

On 2026-07-29 the active `capability-system-compiler` Mom Llama worktree was
re-inspected as a read-only reference. The standalone runtime adopted its
bounded native-model/MCP tool-loop correction: advertised-tool and input-schema
validation, encrypted tool-message lineage, result hashing, iterative
same-tool turns, and fail-closed real-engine evidence. The standalone
implementation remains independently tested and does not inherit readiness
from that worktree.

On 2026-08-01 the official llama.cpp UI logo was also reused from that
read-only worktree. Its recorded upstream source is
`ggml-org/llama.cpp/tools/ui/src/lib/assets/logo.svg` at revision
`3018a11e79e489b657dbb77c95694889ccff92df`; the verbatim SVG SHA-256 is
`0a4955422e6affde4811e0c0915f506305d46d084283484970e337bb1282429a`.
The macOS PNG asset SHA-256 is
`4a208ee44cd2aed50dedc1958f45db5ec650059d02724fc9896f5a535c65413e`.
