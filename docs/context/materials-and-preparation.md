# Explicit document materials

A document's context stores authored instructions and an ordered list of selected
materials separately. The manuscript remains the ordinary versioned document.
Each material binds an attachment ID to a digest of its complete persisted
manifest. That digest includes the canonical text identity, media identities,
coverage and preparation receipt. An optional edited excerpt belongs to that
material; it never becomes instructions. Saving a source excerpt does not rewrite
the original or alter unrelated instructions.

Selecting an import records a reference instead of copying its text into an
instruction editor. Every selected source, including plain text, has a material
card. Removing a card removes the one source selection and therefore both its
text and its media. The generation evidence records the context revision, source
versions, edited excerpts and exact selected byte ranges. Excerpt hashes describe
the actual selected representation, which may differ from the original input.

The existing context-snapshot command accepts explicit material records when
editing excerpts or restoring a co-writer. Instruction-only saves preserve the
current source versions and excerpts. The sidebar exposes instructions and
material cards for the active document, including when a custom pane occupies the
main area. Importing alone still does not select material or edit a manuscript.

## Preparation and cache authority

Each request reloads the selected-source metadata and checks pinned manifest
revisions and source availability before looking up prepared text. The cache key
contains the project path, document, context revision, ordered source identities,
query/rebalance identity, byte budget and rendering-policy version.

A cached value is a bounded immutable derivative of bytes that were verified when
the value was created. Reusing it does not read a replaceable source path and does
not import changed bytes from disk. A cache miss reads and hashes the canonical
payload. Missing source objects or changed source metadata fail admission. If a
same-length source object is corrupted after preparation, a warm request can
still use the already verified derivative; a cold request rejects the corruption.
This is snapshot reuse, not a claim that filesystem metadata proves content
integrity. No mtime is used as an integrity or authorization witness.

Native media preparation has its own consumer of that same admitted selection.
It reads and hashes required media, but does not prepare and discard source text.
Media decoding and model capacity checks remain the existing native boundary.

## Focused evidence

The context tests exercise independently authored instructions that repeat source
text, Unicode/CRLF excerpts, source removal including mixed text/media, source
replacement and revocation. A warm/cold test uses 4 KiB, 64 KiB and 1 MiB sources
with a fixed output budget. Its counters sit at canonical payload reads/digests
and prepared-cache copies; they are not process-wide allocation or filesystem
cache measurements. The expected warm work is zero canonical payload bytes read
or hashed, with a bounded prepared-string copy. Actual counters are printed by
the focused test rather than inferred from a cache-hit flag. The executed test
reported the following byte counts (canonicalization accounts for source-size
differences):

| Input size | Cold payload read | Cold payload hashed | Warm payload read/hashed | Warm prepared copy |
| --- | ---: | ---: | ---: | ---: |
| 4 KiB | 4,102 | 8,204 | 0 / 0 | 2,371 |
| 64 KiB | 65,548 | 131,096 | 0 / 0 | 2,501 |
| 1 MiB | 1,048,586 | 2,097,172 | 0 / 0 | 2,383 |

The browser check edits a material excerpt and verifies that matching authored
instructions remain unchanged, preserves the source version in the save request,
and removes the exact selected source. It also verifies that a context-format
error leaves the ordinary writing surface independent.

## Existing mixed context

Earlier mixed-text context records cannot prove the origin of edited words. They
are rejected without rewriting their bytes or promoting imported material to
instructions. The materials UI identifies the saved context file and explains how
to retain it separately before selecting sources afresh. No project-wide reset,
automatic migration, source deletion or manuscript rewrite is performed.

These checks do not establish exact native token budgeting, reduced model latency,
process-wide allocation counts, or packaged product acceptance.
