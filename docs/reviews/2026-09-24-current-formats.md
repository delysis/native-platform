# Current persisted formats only

The owner explicitly rejects legacy file-format support. Historical Git commits
preserve old implementations; the current product does not need their readers,
translators or migration paths. Ordinary imported formats such as PDF and EPUB
remain supported inputs. This decision concerns obsolete application schemas.

Removed:

- Loom's extra context header parser and historical mixed-text error variant.
  The single current parser rejects unknown fields and incompatible schemas.
- The one-time Python manuscript translator and its ignored-test registration.
  The ignored inventory consequently has 43 entries, down from 44.
- Mom's attachment-v2 import, schema-tag rewriting and `LegacyCommitted` state,
  including the corresponding preview and garbage-collection exceptions.
- Mom's conversation-load historical attribution repair and persisted model-path
  normalization. Reading now preserves stored bytes; presentation still resolves
  current model paths through the existing projection.
- Missing-field compatibility defaults for attachment lifecycle, message/draft
  attachment linkage and stored approval continuation collections.

Mom's attachment schema is represented by a single serde enum variant, so every
deserialization path, including transaction reads, rejects a different tag. Reads
no longer create or rewrite an attachment database. Missing current stores use an
empty in-memory value until an ordinary write. No user files or old database rows
are deleted by this change. Missing manifest authority still prevents blob GC.

Focused validation: Loom context tests passed 28 with two opt-in tests ignored;
Mom runtime passed 179 unit and 34 integration tests, with 12 opt-in tests ignored.
Negative cases cover unsupported schema tags, missing current fields, unknown
context fields, unchanged invalid bytes, and preserving current stored prose on
read. These checks do not establish packaged native acceptance.

The ordinary Pro attachment-cleanup delegation stopped before submission when
the bridge could not identify its model selector. The coordinator therefore
implemented these bounded removals locally. The separate Pro local-CI job was
confirmed submitted and remains independent of these product edits.
