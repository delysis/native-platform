# One logical document: comparison and replacement

This change establishes one Rust content document used by both products' real
paths. It is an ordered aggregate of exact UTF-8 parts, each with an occurrence
identity, source byte range, explicit text/message kind, and typed metadata.
It replaces independently written projection and branch-walking algorithms.
It does **not** yet replace both durable aggregate schemas or expose a new
cross-product editor UI. That distinction is deliberate and measurable below.

Comparison base: `1cee70d53605ba3bfba6a479e15eda308066ad21`. Source references
below identify files/functions in this change; use the base revision to inspect
removed code. The comparison covers content, identity, branches, revisions,
provenance, drafts, persistence, import/export, and recovery. Inference policy,
attachment parsing, scheduler implementation, and UI composition belong to
other convergence work.

## What is better in each

Mom's `conversation_store.rs` is unusually concise for a capable conversational
object: an explicit selected leaf, editable branches, roles, Persona attribution,
reasoning separation, model/token receipts, and attachment occurrence IDs are all
ordinary Rust records. Personas reuse the conversation object rather than
introducing a second template language. `personas.rs::build_persona_version`
binds the selected branch and execution profile into an immutable version hash.
Its encrypted `RuntimeStore` provides an existing secure payload boundary and
atomic cross-document mutations without giving the renderer database authority.
These features should become available to every workspace document.

Loom has substantially stronger authored-content and write-authority semantics.
Content digest, artifact occurrence, operation, revision, and document identity
are distinct types in `loom-types/src/lib.rs`. `loom-store/schema.sql` enforces
immutable records with triggers. `provenance.rs::save_document_if_source_idempotent`
checks the exact source revision and visible blob, records a request fingerprint,
preserves existing contribution slices, and commits an outbox entry with the
new revision. `draft.rs` keeps mutable autosave separate from semantic history,
rejects stale versions, recognizes an exact lost-ack replay, and stores only two
mutable draft files. Plain UTF-8 manuscripts remain useful without the sidecar.
Those semantics should become available to conversation editing and chat drafts.

Neither implementation should win wholesale. A conversation branch is not a
revision tree: parentage describes conversation context, while a revision
parent describes an edit to a document. Nor is a model candidate an authored
revision. These relations need separate typed identities inside one aggregate,
not two product classes and not one ambiguous `parent_id` field.

## Shared replacement now

`crates/workspace-document` has no file, SQL, keychain, network, inference, or
renderer authority. It depends only on Serde and thiserror.

- `Document<Source, Metadata>` owns a bounded ordered list of immutable-access
  `DocumentPart`s. It borrows existing chat text and copies only selected artifact
  bytes when a blob resolver needs to release its buffer. Parts retain exact
  source ranges and product-typed metadata rather than generic JSON.
- `MessageRole` is shared. A manuscript containing `System:` remains ordinary
  source text; a tool result remains a typed tool part. Rendering cannot promote
  either into a model instruction.
- `text()` is a byte-preserving content projection. `transcript()` is an explicit
  derived reading/writing view with role headings. It is not a prompt parser,
  persistence format, or export grant.
- `BranchIndex` validates duplicates, missing parents, cycles on **any** branch,
  missing selected heads, and a 65,536-node ceiling before traversal. Validation
  and traversal are iterative. Sibling and descendant indexes avoid repeated
  full-history scanning for ordinary branch operations.
- Document content is limited to 128 MiB and 65,536 parts, with checked capacity
  before copying selected bytes. Mom additionally validates all message content
  bytes, including inactive branches. These are rejection bounds, not truncation
  policies and not total database-size limits.

Real production consumers:

| Consumer | Replaced behavior | Shared behavior |
|---|---|---|
| Mom `conversation_document` / `active_path_messages` | Independent ancestry loop that silently broke on missing/cyclic parents | Validated branch index composes the common Document; metadata and exact text remain attached |
| Mom chat, composer, mentions, tool loop, Persona freeze/version/instantiate | Assumed projection cannot fail | Propagate validation failure before generation or a dependent write |
| Mom branch selection and deletion | Unbounded preferred-child loop and repeated scans for descendants | Validated iterative indexed traversal |
| Mom Markdown conversation export | Appended all stored messages, including inactive alternatives | Renders the selected Document transcript |
| Mom `ConversationDb` Serde boundary | Structurally decodable but potentially corrupt branch graph | Validate before typed persistence and after decoding; rejected writes cannot replace a valid stored document |
| Mom `load_db` | Could rewrite attributed assistant prose while merely reading a store | Reads preserve stored content; optional model path cleanup is only an in-memory projection |
| Loom `DocumentContent::project_visible` | Separate prose/verse/hybrid loops; hybrid prose normalized CRLF | Builds the same Document, then projects exact text and block ranges |
| Loom `project_artifact_slices` | Independent range/UTF-8 loop | Shared bounded source-slice validation/projection |
| Loom store `reconstruct_segments` / `reconstruct_revision` | A second independent artifact reconstruction loop | `revision_document` retains source occurrences, ranges, contributions; reconstruction projects that common Document |

`DocumentContent` remains Loom's existing command/wire representation for selecting
prose, verse, and hybrid blocks. `Conversation` remains Mom's existing durable
record. They now feed one logical content implementation; they have **not** been
renamed or hidden inside a universal enum and declared eliminated.

## Class-by-class remaining integration verdict

| Class | Best features to retain | Verdict and exact remaining work |
|---|---|---|
| Text and block projection | Loom exact authored bytes; Mom structural roles | **Shared replacement now.** Hybrid byte normalization and duplicate slice projection are removed. |
| Branch navigation | Mom selected leaf and sibling ordering; Loom's refusal to accept corrupt authoritative facts | **Shared replacement now** for Mom navigation. Loom immutable revision ancestry remains a distinct relation; no false claim that revision parent and conversation parent mean the same thing. |
| Durable document identity | Loom typed document/revision/artifact/blob identities; Mom stable conversation and source Persona IDs | **Real remaining replacement.** Introduce a canonical document/part/revision namespace that preserves existing IDs as explicit source identities. A same-looking UUID or text hash is not a cross-store identity mapping. |
| Edit provenance | Loom immutable source slices, operation/receipt records, explicit promotion; Mom alternate response branches and attribution | **Real remaining replacement.** Mom currently creates edited message occurrences but lacks Loom's contribution-slice ledger. Preserve the original generation receipt and add a human edit operation instead of treating an edited assistant-role message as untouched model output. Keep conversation ancestry distinct from edit ancestry. |
| Revision/current-state selection | Loom source revision + visible-blob compare; Mom explicitly selectable chat leaf | **Real remaining replacement.** A document revision needs an explicit selected conversation head in its state, and a manuscript layout needs a selected reading order. Selecting a head should not mutate immutable nodes. |
| Drafts/autosave | Loom expected version, exact replay, non-reused sequence and bounded two-slot storage; Mom encrypted attachment-aware draft ownership and pre-conversation composer | **Real remaining replacement.** Port Loom's version/source/content compare-and-swap semantics into an encrypted Mom draft codec, including attachment-set identity and atomic consumption on send. Keep pre-conversation draft identity independent of a saved manuscript. A shared payload type alone would not prevent stale draft overwrite. |
| Physical payload storage | Mom XChaCha20-Poly1305 with namespace AAD and OS-backed key; Loom readable manuscript plus private sidecar | **Complementary adapters retained.** One document can choose encrypted-only or an authorized visible-file projection. Encryption/storage policy must belong to the document, never to whether Chat or Write is selected. Do not decrypt a chat to a manuscript file on a template switch. |
| Transaction boundary | Mom `mutate_documents` commits conversation/draft/attachment/receipt changes together; Loom source-bound revision + outbox + exact draft consumption | **Complementary adapters now; real shared repository later.** Common commands must commit one domain fact once. Two databases cannot be called one transaction. Choose one durable owner per document; cross-store references require an idempotent transfer operation, not dual writes. |
| Schema/opening | Both refuse incompatible schema identities before mutation | **Complementary adapters retained.** Share behavioral requirements/tests when consolidating a repository; there is no reason to merge unrelated application IDs or normalize SQL purely for line deletion. No migration machinery is added here. |
| File lifecycle | Loom no-follow bounded descriptors, path leases, recoverable rename/delete capture, exact visible source binding | **Complementary adapter retained.** An encrypted-only chat has no external manuscript filename to rename or reconcile. Reuse this adapter when an authorized document gains a visible file. |
| Crash recovery/outbox | Loom distinguishes committed semantic revision from applied visible bytes | **Complementary adapter retained.** A future encrypted document with a visible projection needs this same outbox. Mom currently has no ordinary visible transcript file, so a second outbox there would be unused machinery. |
| Attachments | Mom draft/branch occurrence lifecycle and atomic GC; Loom artifact-slice provenance | **Part metadata retained; attachment service already shared.** Persist attachments as exact part/source references and preserve independent draft versus committed ownership. Parsing/transforms are not document-core responsibilities. |
| Personas/templates | Mom frozen branch + versioned execution profile and allowed tools; Loom layouts and document references | **Real remaining replacement.** Freeze any document revision as context and pair it with a separately versioned execution profile. Layout templates must never grant tools or storage access. A writing document can benefit from Persona context without becoming a Persona catalog row. |
| Export/interchange | Mom explicit JSON/full branch export and readable Markdown; Loom ordinary UTF-8 and immutable source receipts | **Shared reading projection now; durable interchange remains.** Typed export must include roles, hidden branches, parts and evidence identities. Markdown alone is an intentionally lossy projection, not a round-trip document package. |
| Search | Mom searches encrypted decoded title/message text; Loom has project/sidecar resources | **Real remaining replacement.** Search common document parts through the document's privacy-scoped repository. Never index private chat prose into an unencrypted global index. Search result byte spans need exact part/revision identities. |
| Cache corruption vs user-data corruption | Mom quarantines disposable cache and returns a miss, but authoritative records fail; Loom source/blob integrity failures preserve visible work | **Complementary policy retained.** A shared store helper must not turn corrupt user content into an empty document because the cache path can do so. |

## What making Document durable actually requires

The current aggregate is deliberately an in-memory projection. Making it the
only durable domain object is a second, material change, not a serialization
derive or an alias:

1. Move a typed `DocumentId`, `PartId`, `RevisionId`, `OperationId`, and source
   reference into the shared domain. Preserve occurrence identity separately
   from a hash. IDs in existing stores remain readable as exact source IDs;
   do not silently reissue or coalesce them.
2. Give `DocumentRevision` one immutable ordered part set and an explicit
   selected conversation head. A part carries authored text ranges, role when
   applicable, reasoning/provenance/attachments, and contribution metadata.
   Edit parentage and conversation-context parentage are separate fields/types.
3. Define shared commands: append a turn, revise a part, change the selected
   branch, checkpoint a writing edit, promote a candidate, and consume an exact
   draft claim. Commands require an expected revision and stable operation ID.
   A template change issues none of these mutations.
4. Implement that command state machine once over a narrow repository
   transaction. Keep payload protection and optional visible-file projection
   as strategies. The encrypted strategy must encrypt content-bearing parts,
   receipts, drafts and indexes; file strategies use the existing outbox.
5. Route both product command surfaces through this owner, then delete their
   former mutation implementations. Storage adapters can preserve current
   formats during explicit read-only import or refuse them intact. Do not
   invent automatic migrations, run old and new writers concurrently, or
   promote a partial importer as an established data-preservation proof.
6. Prove the cross-layout operations: edit chat response as prose while retaining
   original role/receipt; discuss a manuscript selection bound to its revision;
   branch and return without losing attachments; recover an exact draft after a
   lost acknowledgement; change layout without changing content/storage policy;
   reopen both privacy modes with all immutable parent evidence intact.

The limiting work is shared mutation semantics and ownership, not inability to
represent both sorts of content. The replacement here makes that next step use
an already exercised common content projection rather than a third toy schema.

## Validation and scope

Focused tests exercised both production consumers. Tests explicitly cover
transcript-as-writing preserving roles/tool receipts/attachments and excluding
an inactive branch; manuscript-as-context preserving exact text, source artifact
ranges and contribution metadata; hybrid CRLF persistence; cycle/duplicate/
missing-parent/head rejection; a 50,000-node iterative history; and rejection of
an invalid update against a real encrypted Mom test store without replacing the
original value.

No production stores were opened. No native model inference, GUI behavior, new
file format, or cross-template promotion is claimed by these tests. The patch
introduces a shared core and removes superseded algorithms; it is a foundation
for eliminating product types, not evidence that both old durable records are
already gone.
