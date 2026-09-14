# One logical document: comparison and replacement

Loom is the sole destination after the user's final steering; Mama Llama is a
readable import source, not a second application to keep in parity. This change
establishes one document core for exact writing and typed conversations, makes
its owned snapshot the durable content record for **new Loom revisions**, and
adds an explicit conversation snapshot export/import path. Existing Mama Llama
encrypted payloads keep their format. No product paths are mechanically renamed.

The work does not yet port Loom's active chat pane to typed durable chat. A
transcript imported as writing retains the complete typed source as evidence;
that is useful interchange, not a claim that chat execution has been ported.

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
renderer authority. It depends only on Serde, serde_json, and thiserror.

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

`DocumentContent` remains Loom's command/wire representation for prose, verse,
and hybrid blocks. New Loom revisions are durably represented by the common
`DocumentSnapshot<DocumentKind, ContributionKind>` inside their immutable artifact
metadata. `Conversation` remains Mama Llama's old source format; its normal store
codec is unchanged. The export adapter is not another production document store.

### Durable snapshot and explicit import

`workspace-document/src/snapshot.rs` owns the serializable schema: immutable
parts, exact source occurrence/range, typed role/kind, conversation parentage,
selected head or sequence policy, separate edit-revision lineage, source lineage,
and owner-typed document/part metadata. Constructors and deserialization validate
all branches, selected heads, schema, IDs (512 bytes, no control characters),
content (128 MiB), part count (65,536), and serialized metadata (16 MiB). Resolving
external bytes requires an explicit callback; no path or ambient file read
capability exists in the snapshot. Occurrence identity is never a content digest.

Loom `document_snapshot.rs::seal/load` persists this record at all six revision
creation paths: create, adopt visible file, ordinary save (`store.rs`), source-bound
save (`provenance.rs`), candidate promotion (`generation.rs`), and external
reconciliation (`reconciliation.rs`). Each part references an immutable artifact
occurrence and byte range instead of duplicating manuscript text in JSON.
`load_revision_segments` now obtains authoritative parts from the validated
snapshot, checking SQL segment indexes against it. Reconstruction, provenance,
and source-bound edits therefore cannot silently bypass the durable record.

Mom `conversation_snapshot.rs::freeze`, used by `conversation_export` with the
`document` format, retains every branch, role, selected head, Persona attribution,
reasoning, execution-profile data, receipt IDs, and attachment occurrence IDs.
It does not fabricate receipt bodies or attachment blobs that are not present
in the conversation record. Edited assistant text already has its generation
receipt cleared by Mom's existing `message_edit`; the exporter preserves that
fact rather than asserting the original model generated the revised bytes.

Loom `ProjectStore::import_document_snapshot_if_absent` and CLI `import-snapshot`
accept only bounded, self-contained snapshots. The exact supplied snapshot is
stored as an immutable evidence blob. The selected transcript is explicitly
created as a new UTF-8 document with `Source` contribution, not human or generated
attribution. `imported_document_snapshot(revision_id)` retrieves the complete
source, including hidden branches and private metadata, after reopen. Imported
settings and tool grants are retained as data and never installed as authority.

Concrete CLI path (the export is an intentional plaintext copy of private data):

```sh
mom-llama conversation export --conversation ID --format document --json | jq -r '.result.content' > conversation.document.json
loom import-snapshot /path/to/project conversation.document.json --to transcript.md
```

Set `LLAMA_NATIVE_KIT_DATA_DIR` to the original Mom data directory when needed.
The normal Mom encrypted payload is neither migrated nor rewritten. Preserve the
original store: this is a content/metadata/reference transfer, **not** an attachment,
Persona catalog, credential, or full tool-receipt bundle migration.

The current Loom store schema advances from **15 to 16**. Older stores refuse
opening at schema validation, before recovery or any semantic write can mix old
and new revision formats. A fixture compares the original database and ordinary
UTF-8 manuscript bytes after refusal. A current-version revision whose required
snapshot is missing also refuses reconstruction/source-bound edits. Preserve the
original project, copy its ordinary manuscript files (or use its original binary
to export them), and explicitly import into a new project. This recovers visible
writing, not the old immutable history. No automatic rewrite, inferred lineage,
or historical provenance migration is implemented.

## Class-by-class remaining integration verdict

| Class | Best features to retain | Verdict and exact remaining work |
|---|---|---|
| Text and block projection | Loom exact authored bytes; Mom structural roles | **Shared replacement now.** Hybrid byte normalization and duplicate slice projection are removed. |
| Branch navigation | Mom selected leaf and sibling ordering; Loom's refusal to accept corrupt authoritative facts | **Shared replacement now** for Mom navigation. Loom immutable revision ancestry remains a distinct relation; no false claim that revision parent and conversation parent mean the same thing. |
| Durable document identity | Loom typed document/revision/artifact/blob identities; Mom stable conversation and source Persona IDs | **Shared durable replacement now.** Snapshot IDs distinguish document, part occurrence, revision, and exact source occurrence. Loom retains typed UUID wrappers at its repository boundary; imported Mom IDs remain explicit source identities and new Loom documents get new IDs. |
| Edit provenance | Loom immutable source slices, operation/receipt records, explicit promotion; Mom alternate response branches and attribution | **Real remaining replacement.** Mom currently creates edited message occurrences but lacks Loom's contribution-slice ledger. Preserve the original generation receipt and add a human edit operation instead of treating an edited assistant-role message as untouched model output. Keep conversation ancestry distinct from edit ancestry. |
| Revision/current-state selection | Loom source revision + visible-blob compare; Mom explicitly selectable chat leaf | **Shared durable replacement now.** Snapshot stores an explicit selected head or sequence policy and separate revision lineage. Loom active chat append/select commands remain to be ported; template selection must not mutate immutable parts. |
| Drafts/autosave | Loom expected version, exact replay, non-reused sequence and bounded two-slot storage; Mom encrypted attachment-aware draft ownership and pre-conversation composer | **Real remaining replacement.** Retain Loom version/source/content compare-and-swap and two-slot storage, port Mom attachment-set identity and atomic send consumption into Loom. Preserve pre-conversation draft ownership independent of a saved manuscript. The runs agent owns this port; no new Mom draft abstraction is needed. |
| Physical payload storage | Mom XChaCha20-Poly1305 with namespace AAD and OS-backed key; Loom readable manuscript plus private sidecar | **Remaining Loom port.** Retain Mom as an encrypted read source; port an encrypted-only destination policy if private chats must keep that guarantee. One document can choose encrypted-only or an authorized visible-file projection. Encryption/storage policy must belong to the document, never to whether Chat or Write is selected. Do not decrypt a chat to a manuscript file on a template switch. |
| Transaction boundary | Mom `mutate_documents` commits conversation/draft/attachment/receipt changes together; Loom source-bound revision + outbox + exact draft consumption | **Loom owner retained; Mom feature port remains.** Common commands must commit one domain fact once through Loom. Its source-bound revision/outbox transaction remains authoritative. Attachment-aware chat send/receipt/draft consumption must join that same transaction; dual writes to Mom are unnecessary. |
| Schema/opening | Both refuse incompatible schema identities before mutation | **Complementary adapters retained.** Share behavioral requirements/tests when consolidating a repository; there is no reason to merge unrelated application IDs or normalize SQL purely for line deletion. No migration machinery is added here. |
| File lifecycle | Loom no-follow bounded descriptors, path leases, recoverable rename/delete capture, exact visible source binding | **Complementary adapter retained.** An encrypted-only chat has no external manuscript filename to rename or reconcile. Reuse this adapter when an authorized document gains a visible file. |
| Crash recovery/outbox | Loom distinguishes committed semantic revision from applied visible bytes | **Complementary adapter retained.** A future encrypted document with a visible projection needs this same outbox. Mom currently has no ordinary visible transcript file, so a second outbox there would be unused machinery. |
| Attachments | Mom draft/branch occurrence lifecycle and atomic GC; Loom artifact-slice provenance | **Part metadata retained; attachment service already shared.** Persist attachments as exact part/source references and preserve independent draft versus committed ownership. Parsing/transforms are not document-core responsibilities. |
| Personas/templates | Mom frozen branch + versioned execution profile and allowed tools; Loom layouts and document references | **Real remaining replacement.** Freeze any document revision as context and pair it with a separately versioned execution profile. Layout templates must never grant tools or storage access. A writing document can benefit from Persona context without becoming a Persona catalog row. |
| Export/interchange | Mom explicit JSON/full branch export and readable Markdown; Loom ordinary UTF-8 and immutable source receipts | **Shared durable replacement now for self-contained content.** Explicit document export and Loom import preserve roles, hidden branches, metadata and evidence identities. Full attachment/receipt bundle transfer remains; Markdown is an intentionally lossy projection. |
| Search | Mom searches encrypted decoded title/message text; Loom has project/sidecar resources | **Real remaining replacement.** Search common document parts through the document's privacy-scoped repository. Never index private chat prose into an unencrypted global index. Search result byte spans need exact part/revision identities. |
| Cache corruption vs user-data corruption | Mom quarantines disposable cache and returns a miss, but authoritative records fail; Loom source/blob integrity failures preserve visible work | **Complementary policy retained.** A shared store helper must not turn corrupt user content into an empty document because the cache path can do so. |

## Remaining destination work

The durable representation is now exercised by Loom writes, reopens, provenance,
and source-bound edits. The remaining work is active command and UI ownership:

1. Replace Loom workspace chat's string-concatenated history with typed snapshot
   roles/branches and source-revision binding. Preserve model template dispatch
   and use native chat framing for instruct models; raw writer continuation is a
   separate execution policy. Import-as-prose does not satisfy this.
2. Add append-turn, revise-part, and select-head commands over Loom's existing
   expected-revision and stable-operation-ID transaction boundary. Selecting a
   layout issues none of these mutations. Human edits must retain original proof
   as source evidence without claiming revised bytes are original generation.
3. Port Mom's frozen Persona attribution, attachment ownership and draft/send
   consumption; keep tool authority separately issued by the current operation.
   The preset and runs agents own their bounded portions.
4. Decide and implement destination chat payload protection. A hidden chat pane
   is not encryption. Never write an ordinary transcript merely because a layout
   changes; the explicit import command here is the only new visible projection.
5. Build a reviewed full import package for blobs, Persona versions and receipt
   bodies before deleting the source product/data. This patch deliberately does
   not claim a full-fidelity application migration from IDs alone.

No parallel universal storage backend, automatic migrator, or second active
conversation writer is added. The immutable SQL rows remain useful indexes and
transaction receipts, not another independently interpreted content document.

## Validation and scope

Focused tests exercised both production consumers. Tests explicitly cover
transcript-as-writing preserving roles/tool receipts/attachments and excluding
an inactive branch; manuscript-as-context preserving exact text, source artifact
ranges and contribution metadata; hybrid CRLF persistence; cycle/duplicate/
missing-parent/head rejection; a 50,000-node iterative history; and rejection of
an invalid update against a real encrypted Mom test store without replacing the
original value.

New tests cover durable JSON validation, metadata bounds, exact source range
resolution, full branch/metadata import and reopen, source-only contribution,
external-reference import refusal, snapshot/index disagreement blocking edits,
and old-format refusal without rewriting original manuscript or metadata.
Import rendering follows typed parts: a sequence of messages retains role
headings, while a selected branch of writing retains exact concatenated bytes.
Selection policy never substitutes for content kind.

No production stores were opened. No native model inference, GUI acceptance,
active chat port, full bundle migration, or encrypted Loom destination is claimed.
The format change is explicit: new Loom revisions contain document snapshots;
Mama Llama's encrypted conversation codec remains unchanged.

Final local gate on 2026-09-14, using the shared convergence Cargo target:

```sh
cargo test --locked -p workspace-document -p loom-store -p loom-cli -p mom-llama-runtime -p mom-llama-cli
cargo clippy --locked -p workspace-document -p loom-store -p loom-cli -p mom-llama-runtime -p mom-llama-cli --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Results: **383 passed, 14 explicitly ignored, zero failures**. This includes
140 Loom store unit tests, 3 store integration tests, 10 Loom CLI tests, 174 Mom
runtime unit tests, 34 runtime integration tests, 10 Mom CLI integration tests,
and 12 shared-document tests. The selected crates' doctest targets contain no
tests and completed successfully. Clippy, formatting, and whitespace checks
passed. Ignored tests include real-engine/desktop/legacy-fixture opt-in work;
they were not promoted to acceptance. Root integration still needs to exercise
the final combined tree and its Tauri consumers.
