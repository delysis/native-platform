# Loom project format v1

Status: unreleased current format. Project manifest version `1` identifies the
container; SQLite store version `15` and its application/schema identity identify
the current database. Old schemas are rejected before mutation. There is no
migration chain. A single schema defines current writing, generation, revision,
rename/delete recovery, and draft constraints. Research tables are retired.
Project storage requires Unix private permissions and directory synchronization.


## Authority split

- Ordinary `.md`, `.markdown`, and `.txt` files anywhere below the chosen folder are authoritative for active, readable UTF-8 writing. No `manuscript/` hierarchy is required.
- `.loom/loom.sqlite3` is authoritative for revisions, causal occurrences, receipts, generation evidence, branch state, and pending visible-file projection.
- `.loom/blobs/sha256/**` contains immutable payload bytes referenced by semantic history.
- `.loom/drafts/**` contains at most two mutable crash-safe draft slots per document. Draft slots are explicitly not semantic history or immutable content-addressed artifacts.
- A document rename records `prepared`, atomically captures and flushes the exact source into `.loom/renames/`, records `captured`, links a private inode-identity anchor, installs the capture at the no-clobber target, then commits its mutable catalogue title/path and `committed` state in one SQLite transaction. Recovery never treats equal bytes at an unrelated visible target as ownership. Once an operation is terminal (`committed` or `aborted`), its redundant anchor is removed best-effort immediately and on reopen; cleanup can never turn a terminal outcome into an error, and live-operation evidence is retained. The extension, immutable revisions, and blob identity are preserved.
- Document deletion first records the renderer command and exact document/revision/blob/path fingerprint as `prepared`, then atomically captures and flushes the held ordinary file into its deterministic `.loom/deleted/` recovery name. An exact private capture is sufficient for reopen to commit the immutable tombstone and operation state together without renderer replay; absent recovery aborts the intent, while wrong/symlink/non-regular evidence remains fail-closed. Immutable revisions and blobs remain registered. Exact committed replay survives recovery-file loss, and recreating the old visible name does not resurrect the document.
- Rename and delete admission require no pending visible-file outbox for that document; delete also refuses a recoverable transient draft. Tombstones and live lifecycle operations independently block every ID-based mutation and outbox recovery suppresses stale projections, so neither candidate promotion nor crash recovery can resurrect a deleted pathname.
- Current registrations (including tombstones) and live lifecycle endpoints reserve portable pathname keys using Unicode normalization plus full case folding. The same owner may perform a case-only rename; a different document cannot claim a case/normalization alias. A committed rename releases its old source spelling.
- `generation_weave_commands` binds each run to at most one immutable weave command; cancellation command events cannot become completion-family authority.

Visual prose uses Loom's byte-preserving Markdown dialect over the checked-in
ProseMirror schema. It matches the admitted CommonMark subset except for raw
U+0009: Visual mode always treats that byte as manuscript indentation, including
at a line edge, so Tab has one exact meaning in both Visual and Source surfaces.
It does not claim CommonMark's conflicting tab-indented-code interpretation.
Authors who need that external syntax must use Source mode; fenced or inline code
containing tabs remains outside the exact visual round-trip gate. Ordinary files
retain the literal tab byte and no sidecar-only reconstruction is required.
- `.loom/backups/outbox/**` temporarily preserves the exact file displaced at an outbox projection boundary or the bytes involved in a projection conflict.

A missing `.loom/` sidecar means lost history and drafts, not a lost active manuscript.

## Identity and occurrences

- `BlobId` is the lowercase SHA-256 digest of exact stored bytes.
- `ModelEnvironmentId` is a SHA-256 identity for a canonical model environment description.
- `ArtifactId`, `OperationId`, `RevisionId`, `DocumentId`, `ProjectId`, `CommandId`, `BranchId`, `GenerationRunId`, `GenerationEventId`, `CandidateId`, and `SelectionId` are ULIDs.
- Equal payloads may share a blob. They never share artifact, generation, operation, revision, command, or candidate occurrence identity.

This distinction is required: two branches producing identical bytes retain distinct seeds, calls, events, token traces, selection histories, and authorship evidence.

## Concurrent create and save protocols

Opening a folder creates only its private `.loom` sidecar when absent and registers
existing writing files in place, preserving their exact bytes and relative paths.
Reopening and filesystem hints discover newly added files; existing registrations,
including deletion tombstones, are retained. Discovery skips symbolic links,
hidden directories, `node_modules`, and `target`, with a 50,000-entry and 64-level
bound. Document authority excludes hidden paths and traversal outside the folder.
Unsupported encoding and oversized unregistered files remain untouched and are
listed in folder warnings without blocking readable documents.
An unreadable existing sidecar is never replaced with an empty history store.

New documents use `create_document_if_absent`. Both the database registration and visible path must be absent. Its outbox predecessor is strictly `NULL`; projection uses a no-clobber install, so a file appearing after preflight is preserved and reported as a conflict.

Existing documents use `save_document_if_source` or `save_document_if_source_idempotent`. The caller supplies the exact source `RevisionId` and visible `BlobId` it edited. The store:

1. Validates bounded canonical UTF-8 and the project-relative path.
2. Verifies the active database revision and visible file against the caller's source identities.
3. Atomically installs immutable blobs.
4. In one immediate SQLite transaction, rechecks the source and appends artifacts, ordered operation edges, the revision, its provenance segments, the command receipt, and a pending outbox entry whose predecessor is the source artifact's blob.
5. Projects the target with the conflict-preserving outbox protocol below.
6. Acknowledges only after the target file is durable and the outbox is complete.

The idempotent form is keyed by caller-owned `CommandId` and a canonical request fingerprint. An exact retry returns the original receipt, revision, and outbox result. Reusing the ID with different path, kind, bytes, reason, source revision, or source blob fails with `IdempotencyConflict`.

Concurrent editor code uses the source-bound or create-if-absent APIs. File adoption
records a human import without normalizing or replacing the visible bytes.

## Conflict-preserving outbox projection

Projection does not perform a check followed by an unconditional rename. For an existing predecessor it:

1. Durably prepares the target beside the visible path.
2. Atomically moves the exact visible predecessor to `.loom/backups/outbox/<outbox>.previous`.
3. Hashes the captured bytes and compares them with the outbox predecessor.
4. Installs the prepared target with a hard-link create-if-absent operation, which cannot replace a file an external editor created in the boundary window.
5. Rehashes the installed target, removes the predecessor staging file, and completes the outbox.

If bytes changed at any boundary, the external bytes remain visible when possible and the captured bytes remain in the backup slot. The outbox stays pending and recovery reports a conflict. Recovery can resume a crash after predecessor capture or recognize a target installed before SQLite completion.

## Transient drafts

Continuous typing is durable without manufacturing a semantic revision on every debounce interval.

- Each document alternates between two `.loom/drafts/<document>.<slot>.draft` files.
- SQLite stores source revision, exact draft hash, active slot, monotonically increasing version, and update time.
- The inactive slot is durably replaced while an immediate database transaction holds the version compare-and-swap. A crash before commit leaves the active slot unchanged; a crash after commit leaves the newly referenced slot readable.
- Storage remains bounded at two full draft texts per document across arbitrary update counts and crash phases.
- Version `0` means the caller expects no draft. An exact retry of a committed `(document, source revision, expected version, canonical bytes)` write replays the recorded version; different stale bytes fail.
- Clearing a draft deletes the mutable row and both slots. Draft writes never create artifacts, operations, revisions, receipts, outbox entries, or immutable CAS history.

## Projection and human-edit provenance

- Prose canonicalizes CRLF and bare CR to LF.
- Verse preserves exact UTF-8 bytes, including line endings, trailing spaces, empty stanzas, and combining marks.
- Hybrid projection concatenates explicit prose/verse blocks and emits exact byte-range metadata. Full persistence of hybrid block metadata remains an additive follow-up.
- Revision slices use UTF-8 byte ranges and may not split a code point.
- Empty revisions have zero segments. Non-empty revisions must have at least one reconstructing segment.

Source-bound human checkpoints first strip an exact common UTF-8 prefix and suffix. A contiguous insertion or deletion is planned directly in linear time, retaining the original artifact slices on both sides. Other edits run a deterministic Unicode-scalar Myers diff over the changed middle. Every equal run retains the original artifact slices; inserted or replaced runs become a new `HumanContribution`. This preserves generated evidence through multiple disjoint edits, including generated text between two human changes.

The ambiguous diff search is fail-closed and bounded. Contiguous insertions and deletions do not enter that search; the document byte limit and final segment limit still apply:

- changed middle: at most 64 KiB combined UTF-8 bytes;
- changed middle: at most 16,384 combined Unicode scalar values;
- conservative quadratic work budget: 16,777,216;
- segment-visit budget: 1,000,000;
- final revision: at most 16,384 provenance segments.

Larger ambiguous edits require a future validated editor changeset or smaller semantic checkpoints. Text-only diff preserves evidence conservatively; it cannot prove whether a writer retyped text identical to the source. Claims about keystroke intent require editor transaction metadata, not textual equality.

## Generation and authority

The current schema contains immutable model environments, prompt/context recipes, authority policies, branches, generation runs/events/candidates/terminals, selection events, authorship attestations, and idempotent command requests.

- A generation run binds one source revision/blob, target byte range, model environment, exact prompt recipe, context recipe, authority policy, seed, and sampler description.
- Prompt bytes and optional exact token IDs are immutable blob references. No hidden chat template is implied by a completion recipe.
- Token observations distinguish raw-model, post-constraint, and post-sampler log probabilities. Unsupported observations remain absent.
- Raw event streams and optional backend/cache receipts are immutable references. `InferenceEvidenceKind` distinguishes live inference from fixtures, mocks, and historical receipts.
- Events have a per-run sequence. SQLite admits at most one terminal event and rejects later events. Completed terminals require one candidate; failed terminals require an error; cancelled, pruned, and rejected terminals cannot name a candidate.
- Generated spans refer to immutable output bytes and token traces. Editing a promoted span changes revision projection, never its original output or trace.
- Automation, generation completion, and `KeepAlternative` do not mutate the active manuscript. Caller-declared candidates cannot grant promotion authority; automatic suggestions require the current exact revision and explicit command.
- Authority policies assign each environment exactly one writer or critic role. Critics may generate evidence for inspection but promotion is rejected unless the candidate's environment is a designated writer.
- Promotion records an immutable `SelectionEvent` and `AuthorshipAttestation` tied to the human command receipt.

The checked-in `generation-protocol-v1.json` golden fixture fixes the serialized model, recipe, command, trace, terminal, and promotion DTO shapes. It is protocol evidence only, never a claimed live inference receipt.

Blob installation may leave harmless content-addressed orphans if a process dies
before the SQLite transaction commits. Semantic records commit atomically.

## SQLite guarantees

The connection enables foreign keys, WAL, `synchronous=FULL`, a finite busy timeout, and `trusted_schema=OFF`. Semantic history tables are `STRICT`; most occurrence tables are also `WITHOUT ROWID`. Triggers reject updates and deletes on immutable blobs, artifacts, operations, ordered edges, revisions, segment manifests, receipts, model/recipe/policy records, generation records, selections, attestations, and idempotency records.

The current schema includes the deliberately mutable `transient_drafts` pointer table. Its revision/document relationship is trigger-validated. Mutability is limited to the current two-slot draft journal and is not available to semantic artifacts.
