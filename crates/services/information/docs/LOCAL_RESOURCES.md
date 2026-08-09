# Local archive profiles verified on 2026-08-08

This is a public-safe verification summary, not an install registry. Exact
paths, corpus counts, and operator inventory remain outside version control.
No canonical database was modified during the survey or the final real-corpus
smokes.

| Resource class | Verification result | Runtime plan |
|---|---|---|
| Two Alexandria block/FTS libraries | Both real read-only smokes passed | Generic Alexandria backend with per-document rights and immutable read-only registration |
| Community Archive v28 | Real private read-only smoke passed | Compiled message/FTS adapter, private-use ceiling, and explicit trusted-host model opt-in |
| Mixed-origin encyclopedia | Real article/FTS smoke passed | Compiled adapter with strict origin-specific use policy |
| Scripture citation index | Real occurrence/passage smoke passed | Compiled adapter linked by Alexandria block ID |
| Esoterica/OCR holdings | No complete compatible index was verified | Ingestion candidate; do not advertise as installed or searchable |
| Page-preserved research documents | No compatible FTS database was verified | Normalize into block/page records while preserving page IDs |

## Compiled SQLite profiles

| Profile | Local role | CLI root |
|---|---|---|
| `alexandria.blocks.v1` | Christian and MPC Alexandria block/FTS sources | `alexandria` |
| `community-archive.messages.v28` | Community Archive v28 messages/FTS | `community-archive` |
| `encyclopedia.articles.v1` | Encarta/Britannica/Wikipedia articles/FTS | `encyclopedia` |
| `alexandria.scripture-references.v1` | normalized passages and citation occurrences | `scripture` |

The exact search and stable-record lookup forms are in [CLI.md](CLI.md). No
other local schema is accepted through runtime-supplied SQL identifiers.

## Adapter rule

Do not create a “generic SQL template” that accepts catalogue-provided table or
column names. Alexandria is a known compiled profile. Community Archive,
encyclopedia, scripture, media, graph, and geospatial sources get adapters whose
queries and policy semantics can be reviewed in Rust.

## Rights rule

Corpus-level labels are insufficient. Alexandria includes private and
heterogeneously licensed records. Community posts, public web media, and
copyrighted encyclopedia DVDs are locally searchable without thereby becoming
redistributable. Every evidence result therefore carries a `UsePolicy` in
addition to source rights/attribution statements.
