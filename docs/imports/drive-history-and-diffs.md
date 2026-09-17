# Drive histories and readable revisions

Design and first implementation slice, 2026-09-14. This extends the import work
merged in [PR #44](https://github.com/delysis/native-platform/pull/44).

## The opportunity

Teach Loom how a writer revises in a particular situation. A finished document
shows an outcome; a reviewed before/after pair can show a choice. Preserve the
pair, its surrounding passage, the document's purpose and audience, and the
writer's explanation when available. Retrieve a few relevant, approved examples
for local suggestions and use separate examples to evaluate those suggestions.
Fine-tuning is a later experiment, not the first ingestion action.

An edit alone is an observation. It is not necessarily a preference: a number
may change because the facts changed, a paragraph may be cut for a word limit,
and someone else may have made the edit. A later version is not automatically
better. A reversion is useful evidence to review, not a universal negative label.
Keep observed changes, author-confirmed judgments, and model explanations distinct.

Loom already has immutable revision/blob identities, human/generated contribution
provenance, explicit candidate promotion, and a bounded three-way merge. Extend
those mechanisms. Do not revive the retired research database or introduce a
second version store for the same manuscript.

## What else is worth taking from Turbo Danielle

Inspected upstream `b8c3ad0687dfa92830567b386b61f878a8a0eb0a`, still upstream main
at inspection. These are patterns to adapt, not imported production source.

| Donor evidence | Useful direction for Loom | Judgment required |
| --- | --- | --- |
| `src/routes/sections.rs`, `src/routes/export.rs`: approval snapshots bound to draft/content identity | Bind every review or learned example to exact before/after bytes; preserve those bytes when the source changes | Approval of one revision never approves a later revision |
| `migrations/017_trace_and_preference_logging.sql`: model traces and human input events | Keep an explicit edit reason with the relevant comparison and model suggestion | The donor defaults many events to preference signals. An upload, silence, and a regeneration click do not establish a pairwise preference |
| `migrations/034_human_authored.sql` and `docs/danielle_voice_drafting_plan.md`: separate voice anchors from AI-approved text | Keep examples of a writer's own prose separate from accepted model output and factual sources | Imported Drive files are assigned `human_authored=1, human_reviewed=1` in `src/drive.rs`. Do not copy that automatic classification; a Drive account is not proof of authorship |
| `src/routes/sections.rs`: base candidates, minimum-edit refinement, targeted repairs | Offer distinct alternatives while preserving the selected draft's wording; make the smallest requested change | An additional model pass can flatten voice or add facts; verify actual results |
| `src/optimizer.rs`: document/case-level training and evaluation split | Keep every revision and derivative of the same document family on one side of an evaluation split | Its optimizer scores mechanical prose measures. A high score is not proof of author preference or factual fidelity; reserve a final untouched test set |
| `src/drive.rs`: bounded supported document ingestion | Preserve available source versions and detect updated content | Donor deduplication is by source URL and skips existing documents; it does not import edit history |

The highest-value next loop is **review an edit → preserve the judgment → retrieve
the relevant example → measure whether suggestions improve**. Broad automatic
prompt optimization should follow demonstrated improvement in that loop.

## What Google actually exposes

- `revisions.list` can omit older revisions, especially for frequently edited
  Docs, Sheets, and Slides. Finishing pagination is not complete-history proof.
  Access also requires an appropriate file role; a read-only OAuth scope alone
  does not grant revision-history permission.
  [Manage file revisions](https://developers.google.com/workspace/drive/api/guides/manage-revisions)
- A revision reports its ID, MIME type, modification time, optional last
  modifying user, and possible export links. This is revision metadata, not
  authorship at each text span or a keystroke log.
  [Revision resource](https://developers.google.com/workspace/drive/api/reference/rest/v3/revisions)
- Earlier Docs content uses the selected revision's export link. The current
  file export endpoint cannot stand in for historical content. Blob revision
  downloads have additional retention constraints; this read-only importer
  must not change a revision to “Keep Forever” as a hidden side effect.
  [Download and export files](https://developers.google.com/workspace/drive/api/guides/manage-downloads)
- The Docs API `revisionId` is an opaque update-control token, only guaranteed
  valid for 24 hours and not shareable across users. Do not use it as a durable
  content hash or conflate it with Drive revision IDs.
  [Document resource](https://developers.google.com/workspace/docs/api/reference/rest/v1/documents)

An acquisition receipt should retain the account scope, file/revision IDs, source
MIME and export format, retrieval time, exact raw and extracted-text hashes, and
extraction coverage. Export URLs can contain temporary credentials: resolve them
only inside a separately reviewed constrained downloader, never echo them into
logs or treat them as unrestricted bearer-token destinations. Missing exports,
permission failures, and unavailable revisions need visible outcomes.

## Choosing the diff representation

Keep three responsibilities separate:

1. **Evidence:** immutable original snapshots and metadata. A diff never replaces
   the source documents.
2. **Comparison:** deterministic ordered spans with exact UTF-8 byte ranges in
   both inputs. No text normalization, inferred intent, or hidden patch application.
3. **Presentation:** inline, before/after, or exact-source views of those spans.
   An explanatory model summary, if added, is labeled and points back to them.

Recommended initial presentation: retain paragraph context, highlight changes at
Unicode word boundaries, and show replacement deletions before insertions. Use
strike-through and underline as well as color. Offer before/after for large edits
and line detail for verse. Surface source syntax and whitespace when they matter.
Do not claim that reordered text is a move without an unambiguous match.

For future rich-document comparison, align structural blocks and keep text,
formatting, link targets, lists, table cells, footnotes, and tab boundaries
distinct. Compare the same extraction format and extractor version. A text-only
match cannot establish that document formatting or embedded objects are equal.

Choose defaults using comprehension, not a vote on colors. Begin with the eight
synthetic cases in the workbench, then 12–20 author-selected real examples with
permission. Counterbalance the order of layouts and ask the same questions:

- What changed? Which facts or sources changed?
- Did wording change, or was material only reordered?
- What action would you take, and how sure are you?
- How long did the review take; what did you miss?

Prefer the view with fewer factual/attribution mistakes and faster correct
answers, then use comfort and preference to resolve ties. Keep layout as a view
choice, so a display preference never alters stored evidence. Compare model
suggestions separately through blind writer choices, factual preservation, and
targeted-edit success; do not optimize solely for edit distance or acceptance.

### Diffs supplied to a model

Human display and model input need separate format decisions. Keep lossless
snapshots and structured ranges as the common source. For the first retrieval
experiment, give the local writer a small reviewed example with explicit fields
for task/audience, surrounding passage, before text, chosen after text, and the
writer's reason. Present it as untrusted example data. Do not substitute a guessed
reason or treat every earlier draft as a rejected answer.

Compare three encodings on the same held-out document families: full local
before/after passages, structured replacements with expected old text, and
unified text diffs. Hold available evidence and model settings fixed; report
input-token cost alongside factual errors, valid targeted changes, and blind
writer preference. A compact encoding that loses the relevant context is not a
win. No format is established as best before this experiment.

If model output eventually proposes patches, bind each proposal to exact source
revision/hash and verify the expected old text and UTF-8 boundaries before the
writer can promote it. Never apply a model's guessed offsets or silently search
for a different matching passage. This follow-up only compares snapshots; it
does not add model patch application or automatically turn imports into training
data.

## Implemented in this follow-up

- `information-native-acquire::google_import::GoogleSession` has bounded
  `drive_revisions` and `drive_revision` metadata reads. Pages contain at most 16
  revisions, use fixed Google endpoints and the existing no-redirect transport,
  retain unknown modifier fields, and always state that the provider may omit
  revisions. MIME types are exposed; export URLs are discarded.
- `loom_document::revision_diff::compare` provides exact line/Unicode-word spans
  for supplied UTF-8 snapshots. A deterministic work budget falls back to an
  explicitly reported whole-block comparison. Combined input is limited to two
  MiB; it performs no file I/O, networking, model calls, or manuscript edits.
- The Rust `revision_workbench` example renders the same comparison as inline
  or before/after HTML, with Words/Lines controls, original snapshot hashes,
  exact ranges, and visible whitespace. All source text is escaped and inert.
  The output uses no scripts, remote assets, telemetry, or saved ratings.

Generate the synthetic comparison artifact:

```sh
cargo run --locked -p loom-document --example revision_workbench -- target/revision-workbench.html
```

To compare two explicitly supplied local UTF-8 snapshots, append their paths:

```sh
cargo run --locked -p loom-document --example revision_workbench -- target/my-comparison.html before.txt after.txt
```

Output creation refuses to overwrite an existing file. The HTML contains the
complete supplied source texts; keep private comparisons local.

## Next product increment and acceptance boundary

This is a metadata API and offline format study, **not** a finished native Drive
history browser. Historical content downloads, persisted revision receipts,
native account/file/revision selection, and author-confirmed learning examples
remain to be wired and exercised. No cloud history or author preference has been
collected during this follow-up.

The next useful product increment is one explicitly selected Drive document:
list available revisions, select two retrievable snapshots, retain both,
compare them, and optionally record a writer's reason. Start with a consenting
writer's small test document and verify exported historical bytes differ from
head as expected. Show gaps and unavailable revisions. Only then add batches,
retrieval of approved examples, or model evaluation. Account setup and consent
remain prerequisites for live Google acceptance.
