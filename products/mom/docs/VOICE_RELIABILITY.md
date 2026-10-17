# Voice reliability qualification

## Cryptographic work budget (17 October 2026)

Encrypted storage owns confidentiality and authenticity of persisted conversation
and native cache state. The native engine must not layer a MAC, signature, or
whole-payload digest over that already-authenticated state. Live allocation
identity and exact metadata equality establish reuse inside the process. A
storage adapter grants nonserialized import proof only after authenticating
both metadata and every payload chunk from the same storage snapshot. Cloning
an immutable allocation preserves its identity; mutation, byte copies and plain
deserialization do not. Model/context compatibility remains mandatory.

The current export is `native-seq-v3`, without the old HMAC trailer or SHA export
receipt. Reconstruction copies only the fixed configuration header. The cache
no longer hashes token lists on validation, and checkpoint publication compares
the saved conversation directly. Speculative audio retries compare one shared
request snapshot directly; they do not serialize and hash old audio or KV.
Voice-window publication also compares inference-authority fields directly;
its persisted boundary still has a content identity so later edits invalidate it.
Unsupported stored formats are preserved and refused, not migrated. These
changes supersede the historical signing/receipt descriptions below.

Every cryptographic operation outside storage needs an explicit purpose, input
budget and cadence. In particular:

| Boundary | Reason to retain cryptography | Work placement |
| --- | --- | --- |
| Acquired models and voice assets | Verify external bytes against the pinned publisher manifest before a native parser consumes them; local AEAD cannot verify a download | Admission or changed-file verification; carry verified file identity forward instead of rescanning unchanged files |
| Model/context fingerprints | A saved native state must name the exact model and compatible native layout | Small configuration and existing artifact digests; never hash native state to identify it |
| Media content identities | Caller-supplied IDs/digests cannot authorize different audio/image bytes; a collision could reuse another utterance's evaluated state | At untrusted media admission, bounded by the media byte limit; reuse an already verified identity within that boundary |
| Canonical audio frames | Reuse projected audio across different WAV wrappers and partial retries without retaining duplicate PCM in every cache key | 640 samples per frame; never the KV tensors |
| Persisted source/approval identities | A changed prompt, source, tool, or approval must not inherit a prior operation's authority | Bounded source/configuration metadata, outside per-token playback |
| Build/package evidence | Bind an executable to the actual native objects and source used to build it | Build/qualification time, outside call setup |
| Explicit controlled-generation/evaluation receipts | Bind recorded interventions and distribution transitions to their evaluated inputs | Opt-in research API; can hash vocabulary-sized arrays per token and must not enter ordinary voice generation |

A duplicate hash does not become necessary by calling it a receipt, signature,
provenance check or fingerprint. Prefer exact equality for process-local state;
reuse the verified digest when the same trusted adapter already read the bytes.
New large scans must identify the trust boundary they cross, explain why an
existing verified identity cannot be reused, and include startup/resumption
measurements. TLS for model acquisition and storage key derivation are separate
required security boundaries, not live-call cache operations.

Regression checks must cover changed metadata/bytes, sole-owner copy-on-write,
untrusted deserialization, authenticated restart, corrupt/missing chunks and a
late prompt edit that restores native KV rather than silently replaying it.
A unit test proving cache eligibility is not a pickup-latency measurement.
Measure call activation, checkpoint settlement, restoration, prefill, first
content and first rendered audio separately. Do not use zero missing counters
as evidence of uninterrupted playback: absent terminal metrics remain unknown.

The focused 12B four-path check after this repair retained 5,270 positions and
restored 5,262 after a late prompt edit, with zero replayed positions. Resident
generation took 169 ms; native import plus generation took 214 ms; token replay
took 10,254 ms; cold generation took 11,907 ms. All four outputs matched. The
snapshot was 1,813,345,000 bytes. These timings exclude model loading and disk
decryption: the engine test explicitly simulates the trusted storage adapter,
while separate product tests cover authenticated reopen and corrupt chunks.
They are not call-pickup or physical-speaker measurements.

Acquisition now carries a verified file identity into the native loader, avoiding
its adjacent model/projector scan. Only Unix identities currently qualify for
this reuse; other platforms rehash. A changed identity forces verification and
the expected content digest still has to match. Parakeet's existing/shared
bundle now has one content verification pass on Unix, with metadata checks
around ONNX initialization. New copied bundles have two passes (copy and
published destination), down from five; these are source-derived counts, not
measured startup speedups. Platforms without strong identities retain full
verification at the load boundaries.

Native artifact identity remains a build-time operation. The binding at
`ce39455a799de9e96e3cbd509dd117b3a6a55e9f` also fixes two sources of irrelevant
cache invalidation: the separately compiled multimodal sources now receive
the source-path remapping, and `cc`-built archives use stable member names.
Two actual native builds with distinct Cargo output directories produced
16 byte-identical archives, including wrapper and multimodal libraries, on
the same Mac/compiler/features. No artifact digest was normalized or weakened.
This does not establish cross-compiler or cross-machine reproducibility.

First-run readiness is scoped to the runtime data directory, including an
explicit `LLAMA_NATIVE_KIT_DATA_DIR` override. A setup receipt in Tauri's shared
application directory cannot qualify a fresh profile. Existing profiles without
their own receipt verify the acquired assets and complete runtime warming once;
the old application-directory receipt is left untouched.

## Pickup regression: phase-consistent history

The candidate 07 warm greeting reused 13,743 of 13,973 prompt positions and
started playback in 2.038 seconds. Candidate 13's warm greeting reused only
11,212 of 15,066 positions; runtime readiness took 2 ms but prefill through
first content took 12.684 seconds. A boolean cache hit is insufficient evidence
of conversation reuse. These are different saved-history states, not a paired
performance benchmark.

The history policy reserved up to 4,096 microphone positions only for Live
input. Pickup could fill that space with history which the immediately following
microphone preparation then rolled away. A cancelled preparation did not commit
its narrower boundary, so redial could select the longer old history again.
All voice phases now reserve the same microphone budget. A changed boundary is
saved before generation in a metadata-only compare-and-set transaction, even
if no subsequent exchange commits. Unchanged windows do not write. The update
checks source history, active branch, model/persona profile, skills and previous
boundary; it preserves all messages and unrelated metadata. It does not export
native KV or change the existing window-profile version.

The regression fails under the previous mode-specific budget (greeting boundary
0, microphone boundary 6). Eight focused context tests pass with the repair,
including near-limit rollover followed by cancellation, store reopening and
redial without a generated exchange, and rejection of stale metadata updates.
These component results do not establish packaged latency improvement.

For subsequent candidates, compare the same conversation/source state and
record prompt positions, actually reused positions, prefill time and full
call-request-to-render time. Exercise greeting, microphone prearm, hangup and
redial as one sequence; committed-turn-only tests miss this regression. Preserve
the baseline executable as well as its manifest. Do not call a historical-source
rebuild the same artifact if the original executable is unavailable.

The latest full user call's completed segments reported zero receipt-to-PCM-EOF
drain. Natural reply render durations matched emitted PCM duration within about
a tenth of a second. That does not qualify fluent audio: generated silence and
prosody breaks require output-PCM measurements, not underrun counters. Do not
remove the final-packet ownership barrier based on these traces.

## Evidence snapshot — 17 October 2026

This is a dated implementation and evidence snapshot, not a live qualification
declaration. Each local gate, candidate build, and device run applies only to
the exact source SHA and tree recorded in its receipt or manifest; a later
receipt supersedes an earlier one only for its own exact identity.

At the candidate 09 checkpoint, the clean-source gate was `b60a82e4` (64 command records).
Candidate 09 was built from that exact tree, with immediate ringing during
setup. It has not been launched or accepted. Its native cache identity matches
candidate 08. Candidate 08 (`52c26174`) included the PCM-tail transport repair,
but human testing still reported interrupted speech. Successful component checks
do not supersede that packaged failure. All failed receipts remain preserved.
Do not promote a package unless its manifest binds its clean source and tree to
the complete passing gate and the required packaged evidence.
The M4 Max runtime evidence below covers bounded voice-history rollover with
synthetic input. Under a manual 21,845 MiB memory budget, the initial
8,192-token setting resolved to 18,944 native context cells in that run. It is
not an M2 measurement or packaged-call acceptance. Earlier consolidated
gate attempts documented below failed for separate workspace issues. Consult
the newest exact local-ci receipt before making a gate claim. Candidate identity
and packaging evidence belong to the generated candidate manifest, not this
document.

Opportunistic ASR slotting is implemented. The latest focused Mom frontend
receipt recorded 244/244 passing tests before the consolidated gate attempts.
A low-priority resident-only Parakeet slot runs during the targetless interval
with playback fully buffered. A finalized transcript
replaces that older audio only in future model input; the original recording
remains attached for playback and provenance. This can reduce history pressure
before rolling the inference boundary. Live microphone PCM flows to Gemma's
bounded prefill queue; caller onset cancels the exact idle ASR operation and
waits for its terminal acknowledgement before Gemma preparation. A slot
exceeding 500 ms disables later in-call slots, and cold Parakeet loading is
skipped. A real Parakeet active-cancel test passed 20/20, with 32.55 ms median
and 33.22 ms maximum terminal latency. This verifies the transcription
cancellation boundary; packaged opportunistic slot/capture sequencing and
target-device behavior remain unqualified.

The actual 32 GB M2 device, physical speaker/microphone loopback, sustained
packaged call, and 30-minute soak remain unqualified. Synthetic tone/PCM and
unbundled native runs do not substitute for those checks.

## Candidate 08 follow-up: cache identity and attribution

Saved conversation checkpoints were present. Candidate 07 and 08 had different
native build IDs while all other fingerprint fields matched, so restoration
correctly rejected the older checkpoint. The CMake-produced macOS archives
included member timestamps in the bytes used for build identity. A controlled
reproduction changed only an object's timestamp: ordinary archives differed;
`ZERO_AR_DATE=1` archives matched. The binding now supplies that environment to
CMake. Actual native artifact hashing remains mandatory. The old candidate 07
archives were not retained, so this proves a reproducibility defect without
claiming timestamps were its only changed build input.

For a binding revision update, settle every active identity before the full
gate: both dependency manifests and lockfile, the engine build recipe and
runtime identity constant/test, the xtask exact-revision seal, and the reviewed
build-script digest in `ci/ignored-tests.json`. Search for the previous revision
and digest across the tree; historical prose is evidence, active policy is not.
Run the focused identity, policy and ignored-test inventory checks together.
The first two gates for this update caught omitted seals; their failed receipts
remain part of the development record.

Live voice lookup is RAM-only, but authenticated cache candidates previously
rescanned their entire multi-gigabyte payload at each lookup/import. The signing
authority now remembers successful authentication of the exact immutable Arc
allocation, with a bounded weak-reference index. Metadata is authenticated again
on each use; changed bytes, metadata, authority or allocation require full
verification. Weak references do not retain the payload. Cold disk entries still
receive full verification before native import.

Candidate 08's delayed voice-note row eventually became a complete replayable
player. Its admission took 27.674 seconds, including an 18.879-second cache
lookup; this aggregate can include worker queueing and authentication, and does
not prove disk access. Its recording included room audio as well as a fixture,
so it cannot qualify fixture-only streaming overlap. Voice-note backfill now
waits for speech synthesis to finish before starting Parakeet. Calls continue
to queue work for the existing preemptible idle slot.
Non-call archival readers now register their exact operation and terminal task
with foreground suspension. New microphone capture can retain PCM immediately,
but Gemma preparation waits for ASR cancellation and any already-started
transcript commit. Readers still examining metadata are marked cancelled before
they can enter inference. Capture discard/start failure resumes recovery only
after model cancellation succeeds; a failed acknowledgement keeps background
work paused. Resident cancellation measurements do not qualify cold model
initialization, which retains its separate bounded settlement path.

Two playback stops aligned exactly with barge-in events on subsequent capture
traces. The user could not establish whether they spoke at those moments.
Do not classify these as echo failures or successful natural playback. New
barge records link capture trace, interrupted reply trace and request identity;
reports distinguish natural completion, stopped playback and missing completion.
Zero underruns never overrides a stopped or missing terminal.

Qualification must verify the running executable AND the intended saved
conversation ID before each test. An accidentally selected new-persona call
must remain a separate receipt, not resumption evidence. Snapshot the complete
trace sequence, since barge-in belongs to the next capture. Preserve unknown
human-versus-echo attribution. Gate/build source is frozen; exactly one owner
launches candidates, and no model checks compete with a user's live test.

## Candidate 07 follow-up: delayed speech and incomplete traces

Candidate 07 restored the saved conversation during startup: 18.328 seconds
of total startup, including 17.234 seconds of prefix restoration. Its warm call
preparation reused 13,973 resident positions with zero prefill; first playback
was 2.038 seconds after the call request. This confirms that restart restoration
and subsequent RAM readiness worked in this run, not that all device or context
cases are qualified.

The user reported one-to-three-second pauses and possible old/new speech mixing.
Some microphone onsets were the user speaking; these are not evidence of echo.
One reply's worklet first-render marker arrived about one second after its
AudioContext resume completed. The context clock advanced only 37 milliseconds
in that wall-clock interval; other replies started rendering within 3–5 ms.
This is a renderer timing anomaly, not acoustic proof or an explanation of all
mid-speech pauses.

The saved reply traces ended around 60–61 KiB before terminal playback metrics.
The recorder silently skipped snapshots above 64 KiB and stopped adding stages
at 512. These limits could hide later underruns, cancellation or completion.
Missing events in these incomplete receipts cannot establish that none occurred.
The repair aggregates repeated delta/PCM traffic and records explicit bounded
compaction while retaining later lifecycle evidence. Caps remain unchanged.

A separate actual-controller regression reproduced a transport ordering defect:
the native synthesis result could resolve before its final PCM channel packets.
Tauri 2.11.5 uses asynchronous fetch delivery for raw channel payloads of at least
1,024 bytes; command completion is a separate transport. The old pump retired
its operation and started the next segment, discarding the delayed audio tail.
The repair waits for both native success and validated PCM EOF before releasing
that segment. Stop/failure releases the delivery wait immediately, while native
producer completion remains a separate cancellation obligation. The watchdog
bounds missing EOF. Per-segment dispatch, first PCM, EOF and result receipts now
distinguish transport delay from synthesis delay. This reproduction establishes
a software defect, not that it caused every reported pause in candidate 07.

## Packaged regression findings

The clean-tree gate for `91246311` passed, but its first candidate did not pass
packaged acceptance. Human testing reported eighth-ring pickup, speaker-triggered
cutoffs and weak recall. One greeting trace took 9.366 seconds to playback and
was stopped by the barge-in path after 619 ms with 4.52 seconds still buffered.
Zero renderer underruns in that trace did not establish audible continuity.
The microphone reported echo cancellation enabled at 48 kHz; the output worklet
used 24 kHz. Reference availability, correlation and residual energy must be
recorded at the interruption boundary to distinguish echo from real double talk.

Pickup previously excluded untranscribed historical audio, even though later
voice turns included it. Pickup now measures and consumes the same authoritative
history: pending audio stays present until an exact transcript is committed.
Empty archival transcription is an observed skip, retaining the recording for
later recovery; source, backend and durable-commit failures remain errors.
The initial speculative reply pause is now 350 ms, while interruption onset
remains independent and responsive. An uncalibrated call initially buffers audio;
a complete first segment may release playback when measured production RTF is
at most 0.85, it holds at least one second and twice the measured startup/refill
runway, and no excessive jitter or underrun has been observed. Slow, short or
unknown probes retain complete-reply buffering. Previously slow producers still
require two healthy segments to recover. No first-packet-only probe is used. All streamed speech segments preserve an
incomplete final word until another token or end-of-stream establishes it.

Qualification also exposed a process failure: attaching a UI tool to an exited
candidate could silently relaunch it without the isolated data and memory-budget
environment. A distinct app identifier did not isolate the runtime store. The
candidate builder now embeds a safe default data namespace, and qualification
must verify the live PID, executable and effective data path before attaching UI
automation. Those earlier launches are failed attempts, not 32 GB budget evidence.
Never infer process identity from an app window or a successful build.

Candidate 04 (`27fdfd0d`) completed its greeting with 11,208 of 11,260 prompt
tokens resident, first PCM at 1.019 seconds and rendered playback at 2.419
seconds. The greeting had no renderer underrun or barge-in. Subsequent room
audio triggered listening turns; their latest trace was initially mistaken for
the greeting trace. That interpretation was retracted after checking trace IDs
and absolute timestamps. Qualification must read the complete call sequence,
not infer pickup behavior from `voice-latency-latest.json`.

That run exposed a browser handoff defect: deferred input replay included the
whole physical recording, including playback and idle time. A later speech
onset supplied a different contiguous origin, so some turns could not retain
their exact inference derivative. The next prompt then processed old physical
audio again. Deferred input now contains only detector-admitted source ranges;
the handoff attaches that queue synchronously before awaiting IPC. Accepted
source ranges are recorded only after acknowledgement, and cancellation maps
model offsets through that ledger before replaying the physical tail. The
`voice_inference_archive` trace records whether the exact derivative survived.
This repair needs a new exact-source gate and packaged continuation test.

Candidate attempts 02 and 03 exposed shell toolchain drift: a missing `sysctl`
path and Node 26 instead of the pinned Node 22. The builder now validates tools
before creating output, uses absolute macOS utility paths, fixes child Node
resolution, and records Node/Rust/pnpm identities in its manifest. Failed
attempts remain separate evidence.

Background startup preparation also needs revision fencing for PersonaPack
restores and cold prefix creation, not only conversation checkpoint imports.
Ordinary chat can run while startup warming is reading a disk entry. Both
worker commands now compare the inspected revision before changing live state;
superseded preparation reports no ready prefix. Automatic warming no longer
runs separate probe generations in the live worker. Authenticated native export
and integrity checks remain required, while deterministic restore/parity probes
remain in explicit cache qualification. Automatic readiness is therefore
`resident_prefix_prepared`, not a claim that a smoke generation ran.

The audio-projector warm command now encodes one frame without taking a
language-model context. It cannot change the conversation revision, committed
prefix, or resumable audio checkpoint. The previous synthetic generation could
replace foreground KV before the persona revision check even began. Startup
selection must also retain the selected chat's identity, so its saved session
is considered before its shared persona prefix; choosing the source contact
would defer conversation restoration until pickup.
Startup may acquire an empty model slot or reuse an exact resident model, but
cannot replace an occupied slot. Its prefix preparation requires that worker's
initial revision (zero), and never loads another profile. This also covers
foreground work that completed before startup's first cache inspection;
comparing only against the revision observed at inspection would be too late.
Startup resolves the selected frozen model before acquisition. Call readiness
is nevertheless established again by explicit preparation of the actual
conversation: its result includes audio-encoder readiness. A superseded or
blocked background warm cannot permanently poison call admission. Encoder
warmup is memoized within its worker, so this readiness check does not re-encode
a frame on every call.

## Candidate 05 startup and playback regressions

Candidate 05 (`ddaf8fe7`) passed its exact component gate but failed cold
pickup. The user called eight seconds after launch. Runtime preparation took
72.801 seconds, including 69.317 seconds in persona preparation, with no
compatible prefix reused. Engine source changes between candidates 04 and 05
changed the conservative native build fingerprint, so earlier native KV was
ineligible. This is not permission to weaken compatibility validation. The
automatic cold path also exported and persisted a large PersonaPack before
reporting readiness. The packaged trace did not separately measure decode,
export, encryption and disk time within those 69.317 seconds.

Automatic preparation now evaluates the prefix into worker-owned RAM without
exporting a snapshot. Explicit cache creation still persists PersonaPacks;
committed conversation checkpoints remain scheduled after idle, hangup and
shutdown. A saved compatible session is still restored before persona-only
preparation. The revision fence applies to both resident-only and exported
prefill. Qualification must measure a cold long persona and a same-build
restart separately, before offering another candidate for manual testing.
A launched process with a verified executable is not evidence of voice readiness.

A separate diagnostic Metal run evaluated 11,143 tokens in 27.815 seconds,
reused that resident prefix in 26 milliseconds, and spent 23.001 seconds on
explicit export of 3,831,765,240 bytes. These are debug diagnostic timings,
not release or M2 performance. The call surface now shows Calling and starts
ringing immediately from the user's gesture while recovery, microphone, and
exact-conversation setup continue. Greeting/model admission remains gated on
that setup and the call epoch; the latency clock still starts at the original
call request, so initialization time remains visible.

Checkpoint persistence yields to foreground work during receipt hashing,
each bounded encrypted part, and subsequent SQLite mutation stages. A typed
preemption rolls back the entire transaction, preserving the prior durable
checkpoint and retaining the pending request for retry. Poisoned call activity
fails closed. Native snapshot export already in progress still serializes call
activation; immediate hangup/redial must measure this separately in the release
package. Bounded write preemption does not make that native export cancellable.

The later human call was responsive for much of the run. Completed turns had
exact inference derivatives, and many reused roughly 13,000 native positions.
Three renderer stops followed barge-in detection with no underruns. These facts
do not prove the interruptions were genuine caller speech or establish audible
continuity. Preserve the whole timestamped call sequence, including failed
pickup, rather than selecting only the successful warm call.

Two actual presentation/cancellation defects were found after this run. The
fallback voice-note handler referenced try-block locals after leaving their
scope, throwing before TTS admission after a successful generation. Those
bindings now survive through the owning dispatch. The sidebar excludes voice
note prompts and their replies from ordinary text previews. Also, PCM startup
could finish after Stop while awaiting worklet preparation or AudioContext
resume. Startup and paused-context resume now observe cancellation before
scheduling or announcing playback, and late cleanup cannot suspend a newer
owner of the shared context. Tests execute the production handlers with those
asynchronous boundaries deliberately held open.

Unbound greeting/fallback speech owners previously accepted any new `started`
event for the same conversation. Dispatch now supplies a bounded correlation
nonce, echoed by the native event bridge, and the speech owner binds only the
matching dispatch. Native request IDs still own generation/cancellation; the
nonce grants no native authority. Late events for stopped/retired owners are
rejected. Cancellation during historical prefill can legitimately leave no
live-audio checkpoint: a replacement that completed the same immutable prompt
and requests sample zero now retains the streaming path instead of being
cancelled again and converted into an ordinary attachment dispatch. Nonzero
replay offsets still require a reused checkpoint.

These repairs require their own exact component and packaged qualification;
candidate 05 remains a failed acceptance attempt. Physical speaker/double-talk,
long-call continuity and the actual 32 GB M2 remain separate gates.

## Candidate 06 observation and queued playback regression

The exact `a51965bf` component gate passed all 64 records. Candidate 06 used
that clean source. A later callback audit reproduced another stale-audio path:
with the eight-second worklet ring full and additional PCM pending, Stop cleared
the worklet then awaited context suspension. An already queued `consumed` event
could schedule that pending PCM as a fallback BufferSource. A queued underrun
could also change global buffering calibration after cancellation. The regression
produced one stale source and one stale underrun callback before the repair.
Scheduling, queued worklet callbacks and late synthesis completion now reject
stopped owners. The actual controller regression holds suspension open and
requires both counts to remain zero; a new exact gate/package is required.

Candidate 06 was launched during conflicting qualification instructions despite
a hold for that repair. No synthetic fixture was played; the user made a call.
Launch ownership is now retained by the root integrator, separate from delegated
build ownership, so prior delegation cannot implicitly authorize another launch.
The user reported a very responsive conversation after Preparing cleared.
Greeting trace `76c044b2-765a-44fb-8ec4-983ba757482c` measured 20.172 seconds
waiting for an already running startup warm, 177 milliseconds for exact call
preparation, ringing at 20.350 seconds and playback at 33.789 seconds from the
call request. Startup itself took 31.988 seconds, including 28.452 seconds of
cold persona prefill. Greeting reused 11,205 of 13,706 prompt positions;
remaining history was processed after dispatch. This native build differed
from earlier candidates. Same-build saved-session restart remains the decisive
cache-resumption check; this first cold run does not establish it.

## Inference-window behavior

Voice preparation measures the complete text and audio prompt against the
native context. Live input reserves one quarter of context, capped at 4,096
tokens, plus completion and framing headroom. When history does not fit, the
planner preserves system messages, advances the boundary by complete
user/assistant exchanges, and stops at a lower-water target. The headroom keeps
the same boundary on a small next turn.

The boundary is persisted as metadata on the conversation. It identifies the
last omitted message and hashes the omitted source authority and frozen
inference profile. Original messages and attachments remain intact; this is
rolling inference history, not a generated summary. The boundary is committed
atomically with the generated exchange. A source edit, branch change, or
profile change makes the saved boundary ineligible for reuse.

The passing M4 Max runtime receipt is
`outputs/qualification/real-voice-context-rollover-m2-budget-synthetic-tone-retry.log`.
It warms the prepared persona prefix, imports long original history, fits a
synthetic live microphone turn under the measured reserve, completes the turn,
checks that all source messages remain unchanged, and verifies that the window
and native persona prefix are reused on the next turn. The run took 71.44 s.
Despite the receipt's historical filename, it is an M4 Max run, not M2 evidence.

The read-only real-audio preflight receipt is
`outputs/qualification/context-window-preflight-real-audio.log`. At a 4,096-token
context it measured 3,178 static multimodal prompt tokens after cancel #1 and
preserved the cancellation/resume evidence: 20 decode-cancel cycles, 19/19
resume-prefix hits, and cancel-to-terminal p50/p95/max of 1.967/2.527/2.707 ms.
The capacity-switch request measured 3,310 tokens before and after its edit,
with only 12 shared prefix tokens; it completed after trimming to that common
prefix. This used synthetic PCM on an M4 Max.

`outputs/qualification/parakeet-samantha-active-cancel-20.log` passed 20/20
active Parakeet cancellations after a partial result. Batch and streaming
transcripts both included the sentence ending; terminal latency was 32.55 ms
median and 33.22 ms maximum. This verifies the transcription cancellation
boundary but does not qualify packaged opportunistic slot/capture sequencing.
The preceding compiler-artifact mismatch is recorded in
`outputs/qualification/parakeet-samantha-active-cancel-20-initial-toolchain-failure.log`.
A fixture-digest typo also failed before inference; neither is a model-quality result.
That digest assertion appeared in the session tool output but was not saved to
a separate raw log before the corrected run; do not treat the initial
toolchain-mismatch log as that failure receipt.

Cold absence skips a transcription opportunity; it does not permanently disable
future slots after the model becomes resident. A slow acknowledged stop or a
failed terminal acknowledgment disables later in-call slots for that process.
Idle history recovery is suspended before foreground chat or call admission,
including across asynchronous metadata lookup and transcript commit. Resident
ASR keeps a two-second settlement bound. An already-running cold attachment
transcriber may take up to thirty seconds to settle initialization; this is
preparation time, not a rapid-cancellation claim. The call surface opens
immediately with its preparing state and working hangup control.

## Cache and transcription boundaries

Before pickup, warm-up prepares the call's actual frozen profile. It aliases a
source persona's cache owner only when the relevant system, model, projector,
selected-model, and skill fields match. During an active call, prefix lookup is
RAM-only; live generation may use resident KV state or bounded in-memory replay
and does not read or write the disk cache. Pre-call preparation can recognize
worker-owned resident prefixes without inventing serialized cache metadata. The
real Gemma 4 12B prefix receipt below verifies repeat preparation reused 11
resident tokens and retained the existing restore/replay checks; a packaged
call still needs request-scoped filesystem evidence for the no-disk-I/O
boundary. Hangup now requests a conversation-scoped checkpoint without waiting
on the UI path, and graceful shutdown flushes eligible committed checkpoints
after work drain but before native teardown. Shutdown captures a restricted
resident-checkpoint capability before cancelling its operation scope. That
capability cannot load a model or submit generation; ordinary native access
stays revoked while the final committed state is saved. A per-call opaque activity ID is
registered before microphone/model setup and remains active until call-owned
stop acknowledgements settle; a matching-ID clear precedes the hangup
checkpoint. Active calls suppress idle persistence, and graceful shutdown
clears remaining IDs only after work drain. These paths are source-level
implementation until the focused tests and a package receipt verify them.

The optional staged Parakeet transcription tap remains bounded to 40,960
samples (2.56 s at 16 kHz), including in-flight audio; it is not a live ASR
fan-out on the Gemma microphone path. A live call sends microphone PCM through
the bounded Gemma prefill queue, with the saved recording retained as recovery
source. Overflow and stalled native acknowledgements fail explicitly. Deferred
PCM replay is tested for exactly-once delivery, including chunks received
during an asynchronous drain. The full physical recording remains the original
attachment and recovery source; selected derived audio can represent the exact
PCM sent to live inference.

## Live audio and retained inference input

For browser and native microphone turns, the complete physical microphone recording
remains the original attachment and is not rewritten. Browser samples are quantized before submission and native input is quantized
at engine admission, using the same canonical PCM16 rule. A successful commit
may retain those exact acknowledged samples as a
`VoiceInferenceAudio` derived artifact with its source ranges and canonicalizer
version. The native ledger translates model checkpoint sample offsets through
acknowledged physical ranges before replay, rather than treating a model offset
as a physical recording index. Failed or ambiguous derivative construction
leaves the physical WAV available and records a diagnostic. This preserves live-to-history media identity while keeping the full
physical capture available for playback and recovery. The live request and
selected derived history append their `native-media` placeholder at the same
current-user message boundary; the placeholder is projected into inference,
not stored in visible message content.

Speculative cancellation truncates the accepted-sample ledger to the native
resume checkpoint before replaying the source suffix. A focused Node test
verifies byte-identical PCM16 samples across that reconstruction and rejects a
mismatched sample count; the runtime abort test verifies that a cancelled
speculation releases no recording for import. If derivative metadata is invalid
or persistence fails, the original physical attachment still commits. The
runtime emits structured `model_input_derivative_unavailable` reason data,
leaves the derived artifact unlinked, and history falls back to the original
audio path. Such a fallback must not be reported as a live-prefix/cache hit.
Real native tests demonstrate retained mixed text/audio state, authenticated
restart without token replay, exact partial-audio prefix reuse, and cold-output
parity. Browser/native IPC and physical packaged behavior require the separate
app checks and candidate run; native tests alone do not establish those gates.

## Evidence history and limitations

The focused integrated-source checks recorded: all-target check for engine,
runtime, CLI, and app; all-target Clippy with warnings denied; runtime
integration tests (39 passed, 17 ignored); app tests (166 passed, 3 ignored);
and frontend tests (244/244).
The engine focused suite passed 225 tests (15 ignored), and runtime unit tests
passed 310. These focused checks are not the settled-tree `local-ci` gate. The
M4 20-cycle audio run, 5,270-token edited-prompt four-path parity run,
5,556-token encrypted restart test, and earlier frontend/Rust counts are
historical snapshots. The earlier 3,310-token
capacity-switch case found a failure before the native lane-trim repair; the
post-repair case above passes with only 12 shared tokens. Keep all failure
receipts unchanged.

`outputs/qualification/real-saved-prefix-restores-final.log` passed the real
Gemma 4 12B native prefix test on an M4 Max. Repeating preparation reused 11
resident tokens without a supplied snapshot; the test also checked native
restore, durable replay, and rejection after a context change. It is native
prefix correctness evidence, not a packaged voice call or M2 result. The first
attempt lacked its model environment and is retained separately as
`outputs/qualification/real-saved-prefix-restores-final-missing-env.log`.

The old `/tmp/mom-live-parakeet-app.log` names a development data identity but
does not bind the executable, bundle digest, or source commit. It is diagnostic
context only. A release candidate was launched, but the first attempt failed
process/data-directory identity verification and is not acceptance evidence.
No paired baseline/ABBA comparison has been performed; `97e6e459` remains a
comparison candidate, not an established release baseline.

The encrypted-cache restart receipt is correctness evidence, not an application
latency result. It used a debug build, and an earlier CPU sample attributed
most restore time to unoptimized ChaCha20 decryption. The cache restore and
SWA-export repairs have separate passing M4 receipts; import remains slower
than resident reuse and needs performance qualification.

## Development and device gates

Use one integrator for the checkout, app state, and serialized Cargo/model
slot. Preserve user data and every failed receipt. Run affected frontend tests,
then focused Rust tests, and finally one settled-tree `cargo run --locked -p
xtask -- local-ci <new-evidence-directory>` gate. Do not treat an ignored
real-model diagnostic or a component test as packaged acceptance.

Run one-off real-model checks through the exclusive-log wrapper. Choose a new
log path outside the checkout; the wrapper opens it with create-new semantics,
so a second run cannot overwrite the first. Child stdout and stderr share that
file, followed by a small exit-status record; environment values are not
recorded. For the active Parakeet cancellation fixture, pin a WAV with the
expected byte length and SHA-256 before running:

```sh
SPEECH_NATIVE_TEST_WAV_ACTIVE_CANCEL=/absolute/path/to/pinned-active-cancel.wav \
SPEECH_NATIVE_PARAKEET_MODEL_DIR=/absolute/path/to/pinned-parakeet-model \
node products/mom/scripts/run-voice-check.mjs \
  /absolute/path/to/new-active-cancel.log -- \
  rustup run 1.95.0 cargo test --locked \
    -p speech-native-backend-parakeet --test real_parakeet \
    real_gateway_transcription_and_request_scoped_cancellation \
    -- --ignored --exact --nocapture
```

The candidate builder requires the passing summary for the exact clean source
tree, verifies source identity before and after packaging, and writes a
per-file SHA-256 manifest. It does not install or launch the app. The exact
candidate must then be identified before any device run.

For every packaged run, keep the exact candidate executable in a foreground
terminal session, verify its PID and SHA-256 against the manifest before CUA
attaches, and confirm that the same PID has the expected isolated
`runtime.sqlite3` open. Recheck PID identity after attachment because app
selection can auto-launch after process exit. A configured memory budget or
settings receipt without that live process/store binding does not establish an
isolated run. A package is not promoted from a build or source test alone:
retain the exact clean-tree gate, manifest, effective settings/store identity,
request traces, playback evidence, failed runs, and explicit hardware limits.

Packaged qualification must record the app and executable paths and hashes,
bundle identity/version, commit and dirty state, OS/build, Mac model/RAM,
model/projector/Parakeet identities and digests, voice-reference digest, and
cold/resident state. Retain traces and resource samples. Exercise a sustained
multi-minute call, 20 varied cancel/resume cycles, a 30-minute soak, physical
playback/microphone loopback with double-talk, and the 32 GB M2 under normal
thermal load. Measure startup, first-token/PCM/playback percentiles, underruns,
cancellation, peak memory, queue high-water, CPU, and thermal state.

## Audit corrections

One Clippy attempt pinned Cargo and RUSTC to Rust 1.95.0 but left PATH pointing
at Homebrew `cargo-clippy` and `clippy-driver`; it failed on incompatible
compiler metadata before producing project diagnostics. The corrected pinned
retry reached project linting and passed after a test-source fix. See
`outputs/qualification/clippy-homebrew-toolchain-mismatch.txt` and the pinned
retry receipt.

The first consolidated attempt used Node 26 instead of pinned Node 22. A
later policy scan traversed ignored SwiftPM `.build` output as project source;
the scanner now excludes that generated subtree, with a regression test.
Ignored-test inventory counts and the audited build-script digest were also
corrected. Failed gate receipts remain failures.

The historical generated Parakeet WAV could not be reproduced byte-for-byte.
The current run uses a fresh explicitly pinned fixture identity; it does not
claim reproduction of the historical audio artifact.

The direct cache-preparation-to-microphone handoff also passed on M4 Metal:
`outputs/qualification/real-text-greeting-incremental-audio-prefix-final.log`
reports 211/211 prepared tokens reused without a supplied snapshot, followed
by the greeting-to-audio check. The worker now retains preparation's text KV in
the microphone prefix lane, including when no serialized RAM snapshot exists.
Background maintenance retains at most 64 transcript intents containing saved
message/attachment identities; it does not retain raw capture PCM. Unqueued
maintenance is rediscovered from saved recordings during idle recovery.

The clean d4cb3a33 consolidated attempt failed because root workspace tests
required the generated Loom Signal helper before the runner built it. The
runner now provisions that isolated workspace and macOS smoke support first,
and installs WebKit before browser tests. Failed attempt evidence remains at
`/Users/george/.codex/evidence/local-ci-d4cb3a33-attempt-01`.

The clean d1157b1d attempt reached workspace tests and found a missing
`state_cache_directory` initializer in Loom's `NativeModelConfig`; the narrow
initializer was added in 5140b8ed. That clean gate then exposed two Loom
media-admission failures: the shared identity ledger had applied Mom's
256-audio history allowance to ordinary mixed media, allowing 33 distinct
payloads where Loom's contract remains 32. The full receipts remain at
`/Users/george/.codex/evidence/local-ci-d1157b1d-attempt-01` and
`/Users/george/.codex/evidence/local-ci-5140b8ed-attempt-01`. The repair keeps
the default ledger aggregate limit at 32 and opts in only Mom's committed
history emitter to the bounded audio-history allowance. These receipts remain
failures even if a later exact-tree gate passes.

## Streaming and shutdown follow-up

The producer's unread backlog remains bounded at 30 seconds. The accepted PCM
archive has a separate capacity derived from the admitted native context, so a
worker keeping up with speech does not reject an utterance merely because its
total duration crosses 30 seconds. The model still enforces its actual context
budget. A 45-second processed-stream test distinguishes archive retention from
backpressure.

The final shutdown regression test creates a real committed conversation while
an active call guard excludes idle persistence, captures export access, cancels
the scope, flushes, tears down the worker, and requests authenticated restore
from the encrypted checkpoint. Its run receipt, not the test's existence,
establishes success. The actual M2 and sustained physical-call qualification
remain separate from all M4 component measurements.

A quick call restart can overlap the previous hangup's encrypted checkpoint
write. Preparation first probes the worker for the pending committed request's
identity. If it still owns that exact request, RAM wins without a cache database
read; an older persisted checkpoint must not replace it. This probe reads only
ownership/count metadata and preserves speculative resume state. The shutdown
regression also creates an older disk checkpoint and a newer divergent resident
turn to verify this ordering before final save/restart.

Checkpoint publication is also conditional on the serialized source conversation
still matching inside the SQLite write transaction. A delayed writer cannot
replace a newer conversation's saved state. Conditional native imports compare
the worker revision at the import command itself, closing the gap between RAM
inspection and disk loading. Generation attempts advance that revision even
when cancelled; read-only inspection leaves resume checkpoints intact.

## Candidate 10 findings and corrective scope (17 October 2026)

Candidate 10's exact 1a283ba7 component gate passed all 64 records. Packaged
ringing began at 15 ms; the first greeting was subsequently cancelled by a
recorded microphone onset before the fixture played. This is not a clean
speaker playback pass, and source tests cannot determine whether a person or
echo caused that onset. A quiet-room/double-talk interval remains required.

Rollover formerly applied its two-thirds low-water mark to the whole context
allowance. A long mandatory persona could exceed that mark, forcing the fitter
to discard every older turn even though recent exchanges still fit the hard
limit. Headroom now applies only to the budget remaining after the mandatory
persona/system/current-input projection. A versioned profile invalidates old
window boundaries once; source history and native prefix authority remain
unchanged. Warm windows that still fit require no baseline remeasurement.

A silent native note can already own a prearmed speculation. Sending it rejects
the incomplete recording, but previously stranded optimistic user/reply rows.
Confirmation failure now settles only nodes and routing owned by that exact
speculation, preserves replayable native/browser audio when available, and
leaves newer replies untouched. The complete-recording guard remains intact.

Speaker-played fixtures are not reliable microphone injection: candidate 10's
second fixture produced zero detected onsets and zero accepted samples. That
run demonstrates failure-state cleanup needs, not recognition quality. The
first fixture also began late and was truncated by silence handling. Retain
both failed attempts. Start fixture playback immediately with capture and stop
without intermediate tool/readback trips; do not call source-duration clocks
proof that the whole waveform reached the microphone.

## Native metadata independence

Candidate 11 (2f418b3f) passed components but failed cross-build cache-identity
qualification and was not launched. GGML's generated source overlay lives under
the consuming application's target directory; its Git discovery embedded the
application commit in libggml-base.a. Thus unrelated UI commits changed native
artifact bytes and invalidated compatible cached state despite ZERO_AR_DATE.

Binding revision 0eda11257a189bd26484d9b751eb6710fec0d7c0 pins GGML metadata to
the actual upstream 5f55650a78f92aff4d48d671423e888fac0469ff. The overlay honors
that explicit value, and the build script applies it after generic environment
forwarding so an environment override cannot substitute another revision. Full
artifact hashing and context-fingerprint validation remain intact. A real CMake
regression under two distinct parent Git commits kept the pinned metadata equal;
the unpinned control followed each parent commit. The earlier failed candidate
and comparison receipt remain preserved. Always compare native identity between
app-only builds; a component pass alone does not establish cache compatibility.

## Standalone hashing performance

The Mom-only release build must enable sha2 0.10.9's `asm` feature. That version
gates its runtime-dispatched ARM SHA-256 implementation behind this feature;
without it an ARM Mac uses the software implementation. Enabling it only in a
Loom dependency made workspace checks and standalone Mom builds resolve
different hashing backends. The workspace dependency now declares the feature
so every product receives the same implementation choice. Digest formats, HMAC
authentication, cache fingerprints, and encryption remain unchanged.

Candidate 14's release fingerprints and selected-package feature tree confirmed
`default,std` without `asm`. An isolated release comparison on this Mac, hashing
the same 128 MiB input four times per sample, measured median totals of 887.463
ms without acceleration and 186.381 ms with it; all digests matched. This is a
hashing measurement, not a pickup-latency or audio-playback acceptance result.
Retain the selected-product feature tree with performance qualification; a
workspace-only feature graph can conceal this regression.

Candidate 14's warm redial reused 13,670 of 13,718 prompt tokens, but still took
12.808 seconds to render. The first 11.916 seconds preceded speech preemption;
the call-activity registry lock can wait for a background native checkpoint.
The snapshot includes whole-buffer authentication, but its queue, native copy,
and authentication phases were not separately timed in that trace. Faster
hashing must not be presented as proof that this entire delay or the reported
mid-word audio dropouts have been fixed.
