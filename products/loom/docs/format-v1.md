# Loom project format v1

Status: initial versioned foundation. Project manifest schema version `1` is the stable on-disk compatibility boundary. The current branch reaches additive SQLite migration `10`; database migration numbers do not change the manifest version when they preserve the v1 contract. Migration 10 adds immutable, content-addressed exact token-piece bytes and cumulative boundary vectors for newly verified native calls while leaving historical migration-9 evidence replayable as diagnostic data without inventing missing boundaries.

## Authority split

- `manuscript/**` is authoritative for the active, readable UTF-8 manuscript.
- `.loom/loom.sqlite3` is authoritative for revisions, causal occurrences, receipts, generation evidence, branch state, and pending visible-file projection.
- `.loom/blobs/sha256/**` contains immutable payload bytes referenced by semantic history.
- `.loom/drafts/**` contains at most two mutable crash-safe draft slots per document. Draft slots are explicitly not semantic history or immutable content-addressed artifacts.

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

New documents use `create_document_if_absent`. Both the database registration and visible path must be absent. Its outbox predecessor is strictly `NULL`; projection uses a no-clobber install, so a file appearing after preflight is preserved and reported as a conflict.

Existing documents use `save_document_if_source` or `save_document_if_source_idempotent`. The caller supplies the exact source `RevisionId` and visible `BlobId` it edited. The store:

1. Validates bounded canonical UTF-8 and the project-relative path.
2. Verifies the active database revision and visible file against the caller's source identities.
3. Atomically installs immutable blobs.
4. In one immediate SQLite transaction, rechecks the source and appends artifacts, ordered operation edges, the revision, its provenance segments, the command receipt, and a pending outbox entry whose predecessor is the source artifact's blob.
5. Projects the target with the conflict-preserving outbox protocol below.
6. Acknowledges only after the target file is durable and the outbox is complete.

The idempotent form is keyed by caller-owned `CommandId` and a canonical request fingerprint. An exact retry returns the original receipt, revision, and outbox result. Reusing the ID with different path, kind, bytes, reason, source revision, or source blob fails with `IdempotencyConflict`.

The older convenience checkpoint/import APIs remain compatibility surfaces. Concurrent editor code must use the source-bound or create-if-absent APIs.

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

Source-bound human checkpoints run a deterministic Unicode-scalar Myers diff over the changed middle after stripping a common prefix and suffix. Every equal run retains the original artifact slices; inserted or replaced runs become a new `HumanContribution`. This preserves generated evidence through multiple disjoint edits, including generated text between two human changes.

Diffing is fail-closed and bounded:

- changed middle: at most 64 KiB combined UTF-8 bytes;
- changed middle: at most 16,384 combined Unicode scalar values;
- conservative quadratic work budget: 16,777,216;
- segment-visit budget: 1,000,000;
- final revision: at most 16,384 provenance segments.

Larger ambiguous edits require a future validated editor changeset or smaller semantic checkpoints. Text-only diff preserves evidence conservatively; it cannot prove whether a writer retyped text identical to the source. Claims about keystroke intent require editor transaction metadata, not textual equality.

## Generation and authority

Migration `2` adds immutable model environments, prompt/context recipes, authority policies, branches, generation runs/events/candidates/terminals, selection events, authorship attestations, and idempotent command requests.

- A generation run binds one source revision/blob, target byte range, model environment, exact prompt recipe, context recipe, authority policy, seed, and sampler description.
- Prompt bytes and optional exact token IDs are immutable blob references. No hidden chat template is implied by a completion recipe.
- Token observations distinguish raw-model, post-constraint, and post-sampler log probabilities. Unsupported observations remain absent.
- Raw event streams and optional backend/cache receipts are immutable references. `InferenceEvidenceKind` distinguishes live inference from fixtures, mocks, and historical receipts.
- Events have a per-run sequence. SQLite admits at most one terminal event and rejects later events. Completed terminals require one candidate; failed terminals require an error; cancelled, pruned, and rejected terminals cannot name a candidate.
- Generated spans refer to immutable output bytes and token traces. Editing a promoted span changes revision projection, never its original output or trace.
- Automation, generation completion, and `KeepAlternative` do not mutate the active manuscript. Migration `7` seals the legacy raw-candidate promotion path; those candidates are diagnostic-only and `PromoteCandidate` now fails closed until assembly-first promotion supplies a verified projection and explicit authority.
- Authority policies assign each environment exactly one writer or critic role. Critics may generate evidence for inspection but promotion is rejected unless the candidate's environment is a designated writer.
- Promotion records an immutable `SelectionEvent` and `AuthorshipAttestation` tied to the human command receipt.

The checked-in `generation-protocol-v1.json` golden fixture fixes the serialized model, recipe, command, trace, terminal, and promotion DTO shapes. It is protocol evidence only, never a claimed live inference receipt.

Migration `7` adds the clean-port research admission foundation:

- model-call declarations and exactly one append-only terminal;
- exact output partitions, non-empty generated-span occurrences, flat assemblies, pinned projections, mixed-authorship records, and normalized operation graphs;
- schema and adoption groundwork for audit-only admission records; downstream span, assembly, and projection methods require an opaque admitted-call token bound to the current `ProjectStore` open session before they can insert one;
- canonical, internally fingerprinted promotion requests and append-only, actor-bound, single-use user-presence events tied to one command and strictly increasing host-session index;
- immutable legacy review events. Pre-migration candidates and every new diagnostic legacy candidate receive a terminal quarantine record rather than implicit research eligibility.

`LiveBaseWriterClaim` and related values are declarations, not credentials. The raw receipt/event-stream replay mint is test-only, and no production constructor for `AdmittedModelCall` exists. `loom-inference` alone consumes the native backend's opaque generation seal and mints a move-only `VerifiedInferenceOutcome`. `loom-store` may adopt that value once and derive private downstream tokens for the current random store-session nonce; reopening or copying the project invalidates them. It must never recreate authority from receipt fields, JSON, hashes, record replay, or an admission row. Persisted fixture, mock, critic, historical, literal, or caller-declared live records cannot become strict assemblies.

Migration `8` makes verified batch adoption atomic and replayable:

- one immutable batch header binds the exact non-empty UTF-8 prompt, completion/no-BOS semantics, ordered token IDs, compiled prompt fingerprint, model binding, native request ID, and expected case count;
- every completed or cancelled branch occupies one contiguous request-order position and retains its independently verified terminal evidence;
- cancelled partial output is diagnostic-only and never receives an admitted-call lease;
- a final immutable seal is accepted only when every expected position is present and the completed/cancelled counts agree;
- an all-cancelled batch may be retained as diagnostic evidence, but cannot mint assembly authority.

Migration `9` adds the append-only autoresearch execution ledger:

- immutable campaign and trial specifications, separate campaign and stage DAG dependencies, attempts, event chains, whole-trial and per-stage reservations and charges, and search decisions;
- immutable story graphs and states, prompt masks, evidence-grounded backtranslation proposals and auditions, evaluation tasks and exact candidate evidence, quality-diversity archive snapshots, benchmark seals, qualification journals, and sealed results;
- one process-local exclusive session lease for each frozen campaign or trial. A durable journal writer consumes and retains that lease and the operating-system project lock until the writer is dropped;
- persist-before-reducer transitions. A failed SQLite append leaves the in-memory journal unchanged, while restart replay conservatively abandons an unstarted reservation or charges the full reservation for work that may have started;
- exact, bounded journal replay from content-addressed canonical records. Replayed rows are diagnostic facts and cannot recreate a stage terminal, evaluator, archive, benchmark, or promotion lease;
- separate diagnostic and qualification benchmark namespaces. A qualification result counts only the exact ordered members committed by its one sealed journal; diagnostic or failed alternative runs cannot contaminate that set;
- exact candidate projections and candidate blobs on qualified evaluation rows. An occurrence identifier alone is never evidence that a quotation came from that candidate.

Controller-token maxima may be zero for a controller-free treatment such as exact direct continuation. Writer-token, evaluation, and wall-time maxima for an executable trial remain nonzero. Stored zero controller use is an honest absence of controller inference, not an implicit free call.

Blob installation may leave harmless content-addressed orphans if a process dies before the SQLite transaction commits. No semantic row or live authority is partially committed.

## SQLite guarantees

The connection enables foreign keys, WAL, `synchronous=FULL`, a finite busy timeout, and `trusted_schema=OFF`. Semantic history tables are `STRICT`; most occurrence tables are also `WITHOUT ROWID`. Triggers reject updates and deletes on immutable blobs, artifacts, operations, ordered edges, revisions, segment manifests, receipts, model/recipe/policy records, generation records, selections, attestations, and idempotency records.

Migration `3` adds the deliberately mutable `transient_drafts` pointer table. Its revision/document relationship is trigger-validated. Mutability is limited to the current two-slot draft journal and is not available to semantic artifacts.
