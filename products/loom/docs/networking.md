# Cabals and Signal

This feature lives on `codex/loom-cabals-signal`. It is under active implementation.
Passing unit tests or building the application does not establish linked-device,
two-editor, Internet NAT traversal, or remote-model acceptance.

## The product

A cabal is a shared workspace, with a durable device identity and admitted
members who can edit together, go offline, and resume later. Documents remain
readable Markdown. Shared prompts enter the same document library and explicit
`@` reference mechanism as local prompts. Receiving a document never executes it.
Sharing requires saved writing, without requiring a loaded model or an initial
document. An empty workspace can become a cabal before its first words exist.

Signal brings existing relationships into a pane beside the manuscript. Link
Loom from the phone's Linked Devices screen. Contacts and groups come from the
linked account; Loom never imports a running Signal Desktop application's keys
or database. Chat messages and workspace invitations travel through Signal;
workspace changes travel directly between cabal devices.

Names, prose/verse kind, and removal state live alongside text in each document's
CRDT. A local rename keeps its document identity and merges with unseen edits.
Conflicting names receive deterministic filename suffixes; filename swaps use
the existing recoverable, no-clobber file moves. Exact creation and save receipts
resolve interrupted projections without recreating or overwriting a manuscript.
Projection failures are reported per document so other writing can continue.

Removing an open document lets pending composition finish before its file is
retired. Its shared text remains recoverable. Recover my copies retains removed
or orphaned writing under `Recovery/`, outside the shared namespace. After a
membership loss it can also retain the exact rejected editor draft and external
file version privately, preserving verse bytes. The editor advances away from
rejected input only after that explicit recovery has a durable copy.

Open Signal with `:signal` in Loom's terminal or Cmd/Ctrl+Shift+Y. An invitation
opens with `:join loom://cabal/...` or the chat's Join cabal button. Invite to cabal
prepares a message in the composer. Sending always requires a separate explicit
action. A local model can propose a reply from quoted conversation context; Use
draft copies that proposal into the composer. It never sends the proposal.

Cmd/Ctrl+backtick toggles the terminal. Cmd/Ctrl+Return from a manuscript runs
its selection; Loom captures this shortcut before the visual editor handles
Return. Composition and chat shortcuts retain their own owners. Workspace
commands remain available without a model, without a document, and while a
shared manuscript is read-only. Pending document transitions still settle first.
Read-only shared writing does not disable the document list: a removed member
can open private recovery copies while the normal save and recovery barriers
continue to protect unsent edits.

Inviting or joining from a conversation remembers its workspace in the encrypted
Signal account. Reopening the pane restores those bookmarks. The public link
`loom://workspace/<cabal UUID>` opens an existing local cabal binding; it cannot
admit a new member, create a networking profile, or choose a filesystem path.
These links work in message bodies and group descriptions. The invitation still
grants one admission and must be sent separately to each intended new member.
Forget workspace removes only the local conversation bookmark, preserving the
workspace, manuscripts, and membership.

For a Signal group, **Workspace in group description** previews a saved workspace
link appended to the current description. **Publish description** requires that
exact native review, current edit permission, and the same group revision. A
concurrent edit produces a conflict instead of replacing other people's words.
The PATCH changes only the description; membership, avatar, permissions, and
message timers retain their existing values. The small addition must fit Loom's
conservative 480 UTF-16-unit description budget; Loom never truncates existing text.

Publication and member notification have separate durable receipts. **Check
description** reads the service without sending. After a lost PATCH reply, an
explicit retry reuses the same revision and ciphertext; it cannot apply twice.
**Notify group members** separately sends a bodyless group update so other Signal
clients refresh their descriptions. Its receipt is reserved before sending, and
an uncertain notification is never automatically repeated. Opening or reopening
a review cannot publish or notify. Superseded reviews retain their receipts but
cannot authorize new work. The encrypted journal stops at 2,000 reviews or 8 MiB
instead of silently deleting uncertainty. Protocol metadata does not appear as
an empty chat bubble or enter model drafting context.

The worker verifies the server signature on the returned group change and binds
it to the exact group, editor, revision, and encrypted description before recording
publication. No group secrets cross IPC. Fresh group cache snapshots advance only
to newer revisions; delayed reads cannot restore removed members or old permissions.

One persistent draft owner serializes conversation switches and explicit sends.
Hiding a pane cannot interrupt send settlement or transplant an invitation into
another conversation. Local proposals belong to the workspace that started them
and disappear on a workspace switch; a delayed proposal never replaces the draft.

The terminal can explicitly select a friend's shared model. Only the resolved
text and explicitly resolved native media for each call cross that connection;
raw expressions, document paths, and private attachment labels stay on the
requesting device. Peer calls use the same document functions and nested
pipelines as local calls. Model offers and grant reviews show the modalities
reported by the host's verified model/projector. Chat stop sequences still
require the local model path.

Every peer step is saved before transmission. A lost connection leaves the run
unconfirmed. **Check** retrieves existing jobs without preparing or submitting a
later step; **Resume** explicitly continues the exact saved expression, input,
bindings, host, and grant. Reopening a workspace or listing history never resumes
execution. A cancellation record covers the whole pipeline across restart.
Retained output has signed remote evidence, not local inference attribution.
Reimporting it reuses its creation receipt and preserves later author edits.
Outbound requests keep their session owner while leaving the local model free
for a friend's incoming job. The local-generation permission cannot dispatch a
peer terminal call; dispatch and recovery use the peer-compute permission.

An interrupted run retains its last delivery report, including a refused grant,
busy host, or exhausted budget. This bounded, atomically replaced diagnostic is
separate from immutable execution receipts. It cannot establish completion or
authorize another call; Check and Resume keep their existing meanings. Pending
and unconfirmed calls are labelled peer jobs, with peer result reserved for a
completed run.

## Ownership and authority

* `loom-cabal` owns device identity, signed membership, Automerge documents,
  durable change storage, and authenticated Iroh transport.
* The native cabal service owns one profile lease, its network supervisor, and
  ordinary-file projections. SQLite save receipts resolve crashes between a
  CRDT commit and a manuscript write. External file edits merge from the last
  recorded projection basis, preserving the existing immutable file history.
* `loom-signal` is an owned Rust worker with bounded framed stdin/stdout. It owns
  the linked device, SQLCipher store, reconnection, disappearing-message expiry,
  authored drafts, and send receipts. Shutdown joins or terminates this exact
  child. A restart does not replay requests.
* `loom-signal-protocol` is the shared typed, length-bounded IPC contract. Chat
  content is data. The native terminal's literal-input mode forbids executable
  expressions and implicit `@` expansion when drafting from Signal context.

These boundaries borrow Plan 9's composable named resources and OTP's explicit
owners, bounded mailboxes, and supervised recovery. Automerge supplies CRDT
merging; there is no separately implemented operational-transformation engine.
Membership is signed authority outside the collaboratively editable document.

## Trust and persistence

Device identities use Iroh's Ed25519 keys, generated by OS randomness and kept
under a private profile directory. Iroh authenticates the transport endpoint.
Changes and membership are signed with an application domain separator. A peer
cannot edit membership through document operations or impersonate another
author's Automerge actor.

The initial owner admits members using a random, single-use invitation bound to
the authenticated joining device. Links expire after 24 hours on the owner's
clock. Repeating that device's interrupted join is idempotent until expiry;
completed pairing and membership outlive the link. Creating or redeeming a link
atomically removes expired invitations and retains the latest observed time, so
clock rollback cannot revive them. At most 128 unexpired records are retained,
including consumed links needed for exact retry. Expiry never removes writing
or membership. A person's name is a display label, never a password or identity
proof. Invitations are bearer capabilities: only send them to intended members.

Revocation advances a signed epoch and seals accepted history. Previously
unknown changes from older epochs cannot be smuggled back into the active
workspace. Unmerged local edits remain recoverable as orphaned history. Removal
cannot erase prose that a former member already received. Owner-device loss and
transfer need an explicit recovery design before this can be called complete.

Signal uses pinned Presage and libsignal implementations. Its database key lives
in the OS credential vault; an unavailable vault never falls back to plaintext.
Presage uses trust on first use and rejects changed identities. The Safety numbers
panel supports direct chats and current group members. It uses libsignal's
version-2, 5200-iteration fingerprint over raw ACI UUIDs, matching Signal Desktop.
Looking up a fresh public key does not trust it or consume a prekey. Each review
binds the exact local key, recipient, and both stored recipient identities;
refreshing invalidates the old review ID. Verification is local to this Loom
device and requires an explicit comparison of the displayed number or QR code.

Accepting a review journals approval in SQLCipher, then retires the owned worker.
Its supervisor waits for the process to exit before reopening the same vault.
Before any networking starts, one transaction checks that the reviewed keys are
still current, adopts the approved recipient identity, retires changed device and
group sender sessions, and records verification. A stale approval cannot adopt
another key. A failed transaction preserves the old keys and pending approval.
An approval does not resend any message. Chats with a known changed identity
cannot start a new send until it has been reviewed. The panel shows verification
only after the new worker confirms the committed record.

Send uncertainty is committed before network submission. Repeating the same
send ID checks its existing receipt; Check send is strictly read-only and cannot
dispatch a new message. Authored drafts and pending-send identities are saved
inside the encrypted database with optimistic versions and exact retry IDs.
Workspace bookmarks use the same database, with versioned updates, exact retries,
16 links per conversation, and bounded total storage. They contain no invitation
tokens or group keys. The bundled frontend/native host and worker use IPC version 4;
an older worker is rejected instead of partially serving newer requests.

Shutdown is a terminal stdin command: the reader forwards it in order and does
not schedule another blocking read. The worker reports Stopped after its Signal
tasks and vault settle. The native supervisor closes stdin before awaiting the
child, including an unsolicited stop after failure, so a pending OS read cannot
keep the worker process alive. A real child-process test keeps the parent's pipe
open to exercise this boundary.
Nonzero child exits remain failures in the supervisor, preserving retry backoff
when a worker repeatedly fails during startup.

Disappearing-message expiry is recorded at first receipt, before receiving the
next event. Later group settings cannot extend an already recorded expiry.
Expired and view-once bodies are removed, including on startup after a crash.
Disappearing conversation content is excluded from retained AI drafts.

## Transport and bounds

The native transport is Iroh QUIC. Anti-entropy pulls
missing signed changes after a fingerprint probe. Each member reconnects with
bounded concurrency and backoff. A weak connection can delay convergence without
changing the merge rule. No continuous WebRTC negotiation or bespoke crypto is
implemented in Loom.

The cabal pane's **Connections** disclosure reads device settings without
creating an identity or starting networking. It offers three policies:

* **Community relays** uses Iroh's Internet preset: direct connections, public
  discovery, hole-punching, and community relay fallback. This is the default.
* **Direct connections** disables relays and public discovery. Invitations and
  saved peer addresses supply connection hints. If both devices' addresses change
  while disconnected, fresh address hints may be needed; membership stays saved.
* **Our own relays** uses one to four distinct HTTPS relay origins and disables
  public discovery. Friends configure common relays, so a saved device identity
  remains reachable after its IP address or port changes. Incoming invitations
  and saved peer addresses cannot add a different relay to this configuration.

Settings belong to the local device, outside shared documents. Saving compares
the version that was reviewed and writes atomically under the profile lease.
A lost save reply requires a fresh read before another save. Changes apply when
the profile next starts; they never interrupt an active workspace or replace its
identity. Invalid or unsupported settings stop profile startup and are preserved,
without falling back to community infrastructure. The local settings format is
version 1. HTTPS uses Iroh's normal certificate verification; there is no custom
verifier or production option to disable it.

An owned loopback TLS relay test disables direct UDP and trusts an explicit test
CA. It transfers a document and attachment, restarts a peer with only stale
direct addresses and an unselected relay hint, then merges offline and live edits
using its saved membership and configured relay. It creates no second invitation.
This establishes relay routing and recovery locally; it does not establish
connectivity across two physical Internet networks.

Gossip cannot itself traverse every NAT. Public Iroh infrastructure means Loom
does not operate a paid TURN service, but that infrastructure has costs and
availability constraints. A self-hosted relay still needs a reachable server;
see [Iroh's relay deployment guide](https://docs.iroh.computer/add-a-relay).
Do not promise permanent free or serverless reachability.

Current limits are 16 open cabals, 32 members per cabal, 64 documents, 1 MiB per
document, 64 MiB of change storage, and 20,000 stored changes. Frames and batches
are bounded before decoding. Raw Automerge change blocks are accepted;
compressed change blocks are rejected before their unbounded upstream decoder.
The storage cap currently stops new edits with an error. Long-lived history
compaction must preserve signed provenance and offline recovery before that cap
can be relaxed.

The cabal store is version 4, the signed document payload is version 2, and the
workspace transport uses `app.delysis.loom/cabal/2`. Older experimental stores
and protocols are rejected without rewriting saved data or retaining a
compatibility layer.

## Portable attachments

A locally authored attachment reference publishes the exact retained original
into that cabal. Initial sharing captures attachments in the selected workspace;
later edits publish only references added relative to the author's observed CRDT
basis. Native file imports, recordings, and pasted/dropped image paths all use
this boundary. A received document never authorizes reading a private file just
because it names the file's content hash. Unrelated local edits do not convert
peer-authored references into publication authority.

The existing authenticated channel pulls one-MiB chunks, at most four per sync
pass. Durable contiguous prefixes resume after disconnect or restart. Only the
complete, SHA-256-verified original enters the advertised catalog or can be
relayed. Peers must remain admitted when requesting and accepting each chunk.
There are at most 256 originals, 128 MiB each, totaling 256 MiB; incomplete
reservations consume that same quota. Invalid final hashes discard only the
unverified transfer so another provider can retry it.

Received files live in a separate per-cabal media cache. Shared inline references
read that cache; private recovery documents and explicitly selected local context
retain their local authority. Shared processing never reuses a private import's
filename or acquisition receipt. A local preview or prompt inspects the retained
original through Loom's existing bounded attachment pipeline. Receiving a file
does not run its parser, and an unsupported file cannot prevent unrelated text
from arriving. Extracted text, converted media, and processing receipts have a
separate 256 MiB cache budget and bounded file count; filling it does not consume
the space reserved for raw downloads. Cache entries are retained, not silently
evicted or substituted.

Cabal recovery copies retain the original shared media namespace. The recovery
path is recorded before creating its file; the resulting document identity keeps
that namespace across later renames. Recovery cannot turn peer-authored hashes
into access to private imports. Revealing an original also requires its current
document selection and uses that document's media namespace.

Image preview tokens now bind project, session, document, and exact selected
content. A relative image path remains ordinary Markdown; its in-app rendering
cannot name another document's private image. Local prompt execution resolves
shared images and audio as native model inputs, including media reached through
explicit document references. Existing model/projector modality checks still
apply. Local QUIC tests and native storage/protocol tests establish transfer,
recovery, integrity, and isolation; current packaged two-device model execution
with these attachments remains unverified. Peer model jobs retain and transmit
the exact selected native media. Document-context settings remain local; they are
not silently published with a manuscript.

The Materials sidebar of a shared document offers **Share context as a document**.
The native review names the current cabal members and shows the exact visible
authored instructions, quoted source excerpts, and selected files. Publication
creates an ordinary Markdown document under
`Context/`, which can be edited concurrently, recovered offline, and referenced
with its quoted `@"path"`. The source manuscript and private scratch context stay
unchanged. A text-only material contributes the author's current excerpt, or its
canonical text when no excerpt was edited, without its private original or
acquisition receipt. Image/audio and source-only cards contribute their explicitly
reviewed original bytes in full through the existing bounded asset channel.

The review fingerprint binds the source, publication identity, destination,
membership roster, text, and files. Changes before publication require another
review. A durable private intent precedes asset publication; interrupted retries
reuse the same approved bytes and document identity. Membership changes require
renewed review of a pending publication. Reopening a completed publication keeps
later collaborative edits, rather than replacing them with scratch text. Each
source has one saved publication; further changes happen in that shared document.

Both writer completions and terminal prompts resolve native media from explicitly
referenced documents. References include visible document content, excluding that
document's private scratch attachments. Media already selected by the current
document is not added a second time. Existing namespace isolation, modality
checks, and aggregate media limits apply.

## Shared compute boundary

The Rust networking service now has a separate authenticated whole-job protocol.
It stays disabled until the local application configures an executor, and no
peer can grant itself access. A local grant names one authenticated device, one
cabal membership epoch, one exact model-configuration fingerprint, and limits
for output tokens, elapsed time, and accepted jobs. Membership changes invalidate
the old epoch's grants. Revoking a grant cannot be undone by replaying its ID.

The input contract accepts resolved text, a seed, an output limit, and up to
eight PNG, JPEG, GIF or WAV inputs totaling eight MiB. Media includes its format,
SHA-256 digest and canonical base64 bytes; it carries no filename, path, implicit
reference, or local-tool authority. The host checks the explicit grant and its
model modalities before decoding a new job's media. Unsupported modalities spend
no grant. The native adapter validates image dimensions and decode budgets using
the local import decoder, and checks WAV structure, finite samples, sample rate,
channels, and a two-minute per-recording bound. It checks the actual resident
projector's MIME, object and byte limits before native submission. Malformed input
produces a failed receipt without invoking the model; audio is never silently
transcribed into a text-only substitute.

Media bytes are retained in the requesting ledger before transmission and bound
into the signed job fingerprint in order. Retry cannot substitute different
bytes, formats, or ordering. Terminal recovery reads its immutable media blobs,
without rereading source attachments or following later document edits. Check
cannot create a later pipeline step; Resume uses the original bytes. The host
retains exact native media evidence privately; its public result remains a signed
remote assertion. Media storage consumes the existing 64 MiB ledgers, and space
for the terminal receipt is reserved before admission. Compute frames stop at
12 MiB, independently of the smaller workspace synchronization frame bound.

The native adapter uses the loaded, verified model and
reserves an independent job owner after two seconds without local model work.
Foreground generation, model changes, and shutdown cancel and drain that owner
before taking over the model. Received prompts and actual native execution
evidence live in a private project under the application profile; they never
borrow the active manuscript's authority or modify it. Persisted grants reopen
with the cabal profile and only serve their exact model configuration when idle.
The cabal pane's Share idle compute control reviews a friend, exact model,
output/time limits, and lifetime job budget before granting access. It shows
remaining jobs and inactive membership/model bindings. An app-owned pending
grant survives closing the pane and retains its exact retry ID; status checks
cannot create a second budget. Revoking even an uncertain, not-yet-admitted
grant durably rejects a delayed first admission. These revocation records share
the bounded grant ledger. The terminal submits explicitly selected peer calls
and retains their derived output. The requesting-device ledger
retains exact prompts, grant/model targets, and cancellation intent before
dispatch. It reserves result space, verifies every retained host assertion
against that saved input, and rejects model substitution, rewritten terminal
receipts, and state rollback. Reopening the ledger never resubmits a job or
invents a remote completion. Native app commands now discover one selected
friend's offers, prepare exact requests, submit by saved job ID, and separately
read, check, or cancel a job. Discovery and history reads cannot create a request
ledger. Reopening saved history refuses a missing database instead of creating
an empty replacement. Every command binds the current workspace and session;
only exact already-saved requests remain recoverable after membership changes.
Status rejection or a lost connection leaves the latest signed receipt intact
and reports delivery separately. A persisted cancellation wins over a later
submission; terminal results are returned from storage without dispatching again.
The app history returns bounded previews. Packaged two-device model selection,
native media execution, and result recovery still need acceptance.

An accepted job is committed before dispatch. Each authenticated caller owns its
job IDs, and a retry must match the exact original input and grant. Status checks
and cancellation never dispatch model work. The current compute protocol is
version 3 (`app.delysis.loom/compute/3`); host and requesting ledgers are version 2.
Earlier experimental ledgers are preserved and rejected without migration.
Cancellation carries the exact grant and input, so even cancellation
that arrives before submission receives a durable terminal receipt. This spends
one job from the reviewed budget and prevents any delayed exact submission from
executing. Completed replies survive restart; an
unfinished job receives an interrupted receipt and is never automatically rerun.
There is one active host job, no waiting model queue, eight incoming connections,
and four outgoing requests. Text input and output each stop at 64 KiB, output at
2,048 tokens, and a job's cancellation deadline at two minutes. A local adapter
must enforce idle admission and the selected model's actual token limit.

Cancellation, grant revocation, membership loss, deadlines, and shutdown retain
ownership until the executor returns after joining its native worker. A job stays
cancelling and occupies the slot while that join is pending. A failed ledger
write stops admission, cancels the worker, and remains a reported shutdown
failure after joining. Adapter panics produce a failed receipt.

Receipts are immutable signed remote assertions with a distinct record kind.
They bind the host, requesting device, job ID, exact input digest, grant, model
claim, and result. Signatures authenticate the host's claim; they do not prove
which model it executed and cannot mint local live-worker evidence. Only the
requesting device can retrieve its result, including after its grant ends.

The host ledger retains at most 64 grant identities, 256 jobs, and 64 MiB of
payloads, reserving space for a maximum result before admission. A full ledger
rejects new work instead of recycling identities or silently losing retry
history. Archival and longer-lived retention remain product work.

## Build and licensing

The main workspace and Signal worker use the root Rust 1.95.0 toolchain pin.
The worker has its own locked workspace because its SQLx/SQLCipher dependency
graph cannot coexist with the main workspace's rusqlite link dependency. Install
the root toolchain, Protobuf's `protoc`, and the platform OpenSSL development dependencies.
Windows source builds additionally require native Strawberry Perl for OpenSSL;
Git Bash's Perl is not the native MSVC build interpreter. CI sets
`OPENSSL_SRC_PERL` explicitly and verifies its required modules.
`node scripts/build-loom-signal.mjs` builds and places the required Tauri sidecar.
Tauri dev/build hooks invoke it; plain Cargo workspace builds need this step first.
The sidecar follows Tauri's debug/release build profile, so a development bundle
reuses the worker tested by CI. Direct script calls default to debug; `--release`
selects an optimized worker. Tauri supplies its profile through
[`TAURI_ENV_DEBUG`](https://v2.tauri.app/reference/environment-variables/).

Presage and the Signal worker are AGPL-3.0-only. Preserve the worker's license,
upstream notices, pinned dependency lockfile, and corresponding source when
distributing it. The process boundary provides lifecycle and dependency isolation;
it is not a declaration about the legal scope of the combined distribution.

The worker vendors the pinned Presage and SQLite store crates with four changed
source files: public identity and group operations, an owned-pool constructor,
monotonic group caching, and unsafe-code prohibitions. Each crate carries its upstream revision, original
file hashes, license, and patch notes. Loom joins both SQLite pools before exit,
including initialization failures; dropping the store alone left SQLCipher
connection cleanup detached and reproduced a crash during process exit.

## Remaining acceptance and implementation

On 2026-09-14, two independently profiled native macOS applications exercised
the real Iroh transport and ordinary Markdown projections on one Mac. The
initial run used `b4d719f`; the repaired run used `2d1827d`, with the second bundle
given a distinct test identity and ad-hoc signature. The observed checks were:

* Create and join without a model; bidirectional editor changes and identical
  saved Markdown files.
* Graceful shutdown, an offline file edit, an independent live peer edit, and
  convergence after restart and reopening the saved cabal binding, without
  another invitation.
* Local undo retaining a later remote edit; Unicode Markdown source/visual
  switching; new shared documents and renaming the peer's open document.
* Remote removal retaining open writing, private recovery, navigation into the
  recovered copy, and edits to that copy remaining absent from the peer.
* Live membership revocation, access to the former member's recovery copies,
  and new owner changes remaining absent from that member's saved manuscripts.
* Graceful exit of both application processes and the owned Signal worker.

These checks exposed and reproduced a disabled-sidebar trap after remote removal
and a visual editor consuming Cmd+Return. Both were repaired and rechecked in
the native application. The keyboard regression also fails before the fix and
passes in WebKit. The final frontend gate passed 473 tests, nine focused browser
tests, and Svelte checking with zero errors or warnings.

The packaged Signal worker reopened the encrypted vault created by the native
test, served protocol version 2, rejected conversation requests while unlinked,
and exited cleanly with its parent's stdin still open. Reopen, EOF shutdown,
and a controlled vault-failure exit also passed. An older standalone fixture's
Keychain lookup remained blocked; its credentials were not reset or bypassed.
No phone was enrolled and no Signal message was sent. Neither these results nor
synthetic browser composition tests establish a physical IME session or
connectivity across separate Internet NATs.

A separate native adapter test on 2026-09-14 submitted an authenticated QUIC job
to the actual local SmolLM2-135M Q4 model on Metal. It generated 16 tokens, retained
matching exact-prompt and model evidence privately, returned the same signed
receipt for an exact retry, and left the active manuscript unchanged. Both
endpoints shut down and the actual model unloaded. This test uses a mock Tauri
application host; it establishes native engine and protocol behavior, not the
packaged controls, Signal enrollment, or two-machine Internet connectivity.

WebKit exercised the compute grant review and revocation controls, refusal of a
review after the selected model changed, and closing/reopening a pane with an
uncertain grant. These browser checks use explicit IPC fixtures and do not
establish a packaged two-device model-sharing interaction.

On 2026-09-15, the native peer adapter sent real image and mixed image/audio jobs over authenticated
local QUIC to the catalog's Gemma 4 12B QAT Q4_0 model and pinned projector on
Metal. It checked each input's digest, byte count and MIME in the retained native
evidence, reopened the requesting ledger after a lost admission reply, recovered
the signed result, and returned that same result for an exact retry. The active
manuscript stayed unchanged, both network owners joined, and the native model
unloaded. This used a mock Tauri host, a synthetic image and a generated WAV tone;
it establishes native image/audio execution and evidence binding, not packaged
controls, response quality, or connectivity between physical networks.

Packaged macOS applications built from `8599b36` then exercised the same path
through native controls. The two ad-hoc-signed test copies used bundle identifiers
`app.delysis.loom.media8599.host` and `app.delysis.loom.media8599.peer`, separate
profiles, direct connections, and the built frontend asset `index-DBqFIDAq.js`
(SHA-256 `75cee33abf0dd42290db1f106b5765b554b96cd7331750b2f3d2f9c7dba9e494`).
Host PID 25022 granted the paired device four jobs, each limited to 32 tokens and
120 seconds. Peer PID 27159 selected the advertised text/image/audio model,
imported a synthetic PNG and WAV through the file picker, and completed text
and mixed-media jobs without a local model. Both ledgers retained identical
input bytes. The UI distinguished the generated outputs as peer results.

Plain raw prompts produced repetitive output from this instruction-tuned model.
Using the chat framing stored in its GGUF produced a description identifying
the blue image and the tone. This is one observed answer, not a quality benchmark;
instruction-model prompt templates remain explicit, without hidden chat wrapping.

After graceful peer exit and restart as PID 32363, opening the saved cabal folder
restored all three results and rediscovered the same model without another
invitation. Revoking its remaining grant prevented a stale selected offer from
adding a fourth host job. This exposed a generic unconfirmed message that hid
the refusal; delivery diagnostics now retain the reason separately. Both test
application processes exited, and the synthetic workspaces were preserved.
These checks establish packaged behavior on one Mac, not physical Internet
pairing, phone-linked Signal, IME, or every interruption/preemption case.

The follow-up bundle at `f680168` retained delivery diagnostics across another
native restart. Test identifiers `app.delysis.loom.deliveryf680168.host` and
`app.delysis.loom.deliveryf680168.peer` used the same isolated profiles with the
default Internet policy. With both previous processes gone, host PID 47317 and
peer PID 47448 rediscovered one another without a new invitation. Check displayed
the saved access-denied report and labelled the unresolved request a peer job;
the host still held exactly three jobs. After a graceful requester exit, PID
48161 reopened the saved workspace and restored the same report without another
Check or Resume. The built asset was `index-CqIukfNR.js` (SHA-256
`de9abb65f578217ccfa3aff2bed11386ef0a58fe3bdc1565c9c1ff6c4dc43787`).
This verifies packaged recovery and same-machine rediscovery, not connectivity
across physical networks or a new model execution.

The feature is not complete until the following have concrete evidence:

* Phone linking; real contact/group sync; receipt and explicit send; reconnect
  after process restart; changed-identity handling; expiry and draft recovery.
* Two actual editors typing concurrently, offline/restart convergence, IME,
  source/visual switching, undo, navigation, and joined application shutdown.
* Cabal membership controls, orphan recovery, coherent new/renamed/deleted
  documents, and persistent workspace/chat association in real paired sessions.
* Portable attachment sharing and prompt modality requirements.
* Explicit, revocable idle-compute grants; host-owned whole model jobs; durable
  job identities; cancellation and resource limits; correctly attributed remote
  results. Remote assertions must never masquerade as local live-worker evidence.
* Physical Signal group-description publication and member notification, with
  existing permissions and disappearing-message settings preserved. Offline
  faults, encrypted persistence, signed-response validation, and review UI are
  covered locally; no account has been linked and no real group has been edited.

Presage's current linking path does not import historical conversation backups.
The pane currently displays text and attachment counts, not attachment contents.
Neither full Signal Desktop parity nor old-history import is claimed.
