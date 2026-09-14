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
text for each call crosses that connection; raw expressions and document paths
stay on the requesting device. Text-only peer calls use the same document
functions and nested pipelines as local calls. Attached images/audio and chat
stop sequences still require the local model path.

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
the authenticated joining device. Repeating that device's interrupted join is
idempotent. A person's name is a display label, never a password or identity
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
tokens or group keys. The bundled frontend/native host and worker use IPC version 3;
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

The native transport is Iroh QUIC. Its Internet preset supplies discovery,
hole-punching, and relay fallback; local tests disable relays. Anti-entropy pulls
missing signed changes after a fingerprint probe. Each member reconnects with
bounded concurrency and backoff. A weak connection can delay convergence without
changing the merge rule. No continuous WebRTC negotiation or bespoke crypto is
implemented in Loom.

Gossip cannot itself traverse every NAT. Public Iroh infrastructure means Loom
does not operate a paid TURN service, but that infrastructure has costs and
availability constraints. A self-hosted relay configuration and explicit network
policy remain product work; do not promise permanent free or serverless reachability.

Current limits are 16 open cabals, 32 members per cabal, 64 documents, 1 MiB per
document, 64 MiB of change storage, and 20,000 stored changes. Frames and batches
are bounded before decoding. Raw Automerge change blocks are accepted;
compressed change blocks are rejected before their unbounded upstream decoder.
The storage cap currently stops new edits with an error. Long-lived history
compaction must preserve signed provenance and offline recovery before that cap
can be relaxed.

The current cabal store and signed document payload format is version 2. Older
experimental stores are rejected without being rewritten. No compatibility
layer is retained for the unreleased version 1 prototype.

## Shared compute boundary

The Rust networking service now has a separate authenticated whole-job protocol.
It stays disabled until the local application configures an executor, and no
peer can grant itself access. A local grant names one authenticated device, one
cabal membership epoch, one exact model-configuration fingerprint, and limits
for output tokens, elapsed time, and accepted jobs. Membership changes invalidate
the old epoch's grants. Revoking a grant cannot be undone by replaying its ID.

The current input contract accepts resolved text, a seed, and an output limit.
It carries no expressions, document paths, implicit references, or local-tool
authority. Images and audio need their own bounded input contract before they
can use this protocol. The native adapter uses the loaded, verified model and
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
The app history returns bounded previews. Remote terminal calls remain text-only;
packaged two-device model selection and result recovery still need acceptance.

An accepted job is committed before dispatch. Each authenticated caller owns its
job IDs, and a retry must match the exact original input and grant. Status checks
and cancellation never dispatch model work. The current compute protocol is
version 2: cancellation carries the exact grant and input, so even cancellation
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
