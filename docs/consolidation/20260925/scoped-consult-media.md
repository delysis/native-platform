# Scoped consult media and whole-message context

Parent: PR #103, `b09be65ce15233815b6ea56c813b0c39246f2934`.

## Implemented

Mom's actual mention-dispatch call path now uses the shared whole-unit selector
and native `GenerationBatchRequest`. It no longer rejects every image-bearing
consult. Each target receives only its selected source history, selected host
history and exact current attachments. Text and citation labels remain attached
to their original message. Attribution and content are selected as one unit.

Attachment preparation is shared with direct chat through an explicit scoped
adapter. History-only source access does not inspect or consume an unsent draft.
Occurrence identity is retained even when distinct attachments share one payload.
Each occurrence is independently checked before deduplication. The final union
keeps Mom's stricter reference/text/media budgets, ownership, policy, graph,
decoder-grade, digest and source-length checks.

The native media contract appends payload markers to the final user turn. The
handoff therefore includes an explicit bounded source map linking payload IDs to
conversation/message/attachment/artifact occurrences. This is not a claim that
native media has been retroactively inserted at earlier chat-template turns.
The source map is JSON data and never recursively parsed as @ commands.

Compatible text cases can share a batch only within resident sequence/context
limits. Media cases execute individually: one shared batch-wide media vector
must never mix private inputs from different experts. Native model identity is
rechecked at admission. Every admitted request is drained and waited before its
logical invocation ID is reused. Observer errors and unwinding callback panics
cancel and drain; panic-abort builds cannot provide that recovery. The normal
application panic hook remains responsible for its own output policy.

Text-only prefix caching remains. Media runs do not offer incompatible text
prefix state. Cache hits are derived from native restored/resident/batch-shared
token counters, not from supplying a cache or replaying its tokens.

Encrypted immutable planned-input and initial-native-output receipts retain
source digests and actual completion DTOs, including when a streaming observer
fails. These are not owner-worker native seals, model-acceptance proof, or new
read capabilities. Existing conversation commit and tool approval flows remain.

## Deliberate boundaries

- Audio remains Mom's existing explicit transcription-first path. Video, opaque
  binary, incomplete inspection and unsupported formats do not become admitted
  merely because the request is a consult.
- A media-bearing target with bound tools is explicitly blocked: its existing
  exact-token constrained tool-decision and approval-resume path cannot preserve
  media. No silent removal or fabricated tool/decoder support is introduced.
- Text token measurements do not count image/audio expansion. The actual native
  decoder and worker retain final media capability/context admission.
- Preparation retains the existing whole-history bounded inspection policy;
  this is not an unbounded lazy-history reader or a throughput improvement claim.
- These are runtime changes used by the retained Mom shell. The simplified
  normal Loom frontend, common application owner and per-document expert
  invocation still require their own integration. Linking two shells is not it.
- The new encrypted receipt namespaces need an explicit product retention and
  removal-impact review before release. This patch does not claim that removing
  an expert destroys all retained historical invocation evidence or receipts.
- No schema conversion, EASL promotion, hosted fallback, extra runtime owner,
  process permission, model quality guard or native acceptance waiver is added.

## Qualification

Authored 29 additional Rust tests across identity, scoped attachment preparation,
source maps, selection, grouping, exact native output correlation, cancellation,
observer failure and cache accounting. Existing tests are retained. None of these
Rust tests, rustfmt, Clippy, or native/model/UI checks was executed in the authoring
runtime, which has no Rust toolchain. Source assembly and blob identities are a
separate evidence class, never a substitute for compilation.

```sh
cargo fmt --all
cargo test --locked -p workspace-document
cargo test --locked -p llama-native-types media_identity::tests
cargo test --locked -p mom-llama-runtime attachments::
cargo test --locked -p mom-llama-runtime mentions::
cargo test --locked -p mom-llama-runtime document::tests
cargo clippy --locked -p workspace-document -p llama-native-types -p mom-llama-runtime --all-targets -- -D warnings
```

Commit any formatting-only change before settled-tree component qualification.
Then exercise real direct chat and consults with repeated images, different
private expert images, canonical PDF/DOCX text, explicit audio transcription,
source edits, per-target cancellation, cold/warm text cache and application quit.
Check source identities and actual cache counters, not only answer prose. Do not
claim a passed old-model or old-tree test establishes this candidate's acceptance.
