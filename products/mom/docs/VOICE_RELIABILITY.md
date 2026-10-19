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

The engine build identity includes its resolved Cargo dependency closure,
not the entire workspace manifest and lockfile. Adding an unrelated speech
backend must not invalidate saved Gemma state. Package versions, sources,
checksums and resolved dependency edges remain included, alongside engine/type
sources, compiler, target, features and native artifact evidence. A change to
this identity recipe itself invalidates earlier checkpoints once; it does not
authorize importing incompatible state. A warm callback alone cannot qualify
this boundary: verify persisted restoration after an application restart.

First-run readiness is scoped to the runtime data directory, including an
explicit `LLAMA_NATIVE_KIT_DATA_DIR` override. A setup receipt in Tauri's shared
application directory cannot qualify a fresh profile. Existing profiles without
their own receipt verify the acquired assets and complete runtime warming once;
the old application-directory receipt is left untouched.

Setup also binds the native cache build identity and the cache-preparation
schema. When those change, the setup screen prepares and persists every persona
system prefix and each nonempty saved conversation's bounded voice-history
projection before publishing readiness. It uses the same history projection,
native generation, authenticated storage and pickup preparation as calls.
Multimodal history preparation generates one discarded warm token because the
normal native generation entry point requires a positive token budget; it never
commits a message or changes user-visible audio/text. Future generation still
checks and trims to the exact shared prefix. This is measured setup work,
not a second speech pipeline or a hidden call greeting.

Preparation is sequential and observes the existing per-entry, total-byte and
entry-count cache budgets. Failures cannot publish a ready receipt. A final
metadata check requires every newly prepared checkpoint to remain present;
the selected context is then restored to worker-owned RAM. Other prepared
contexts reside in encrypted storage until selected. The preparation summary
counts newly published checkpoints, not cache tokens reused during their
construction. This explicit setup phase must not be moved into background work
that competes with an admitted call, nor repeated on every unchanged launch.

## Gemma channel boundaries before speech

Gemma can emit `<|channel>thought\n...<channel|>` even when a call requests a
short answer. The shared incremental reasoning parser must route that block to
reasoning, including an empty block, before emitting assistant-content deltas.
The final response uses the same parser before persistence and text completion.
Do not send model protocol markers to TTS or solve this at sentence boundaries:
a marker may be split across any streamed token boundary.

Candidate 21 recording diagnostics found a marker-bearing first sentence whose
144 characters and 21 words matched the corresponding TTS request. At the
approximately aligned 4:09 playback, consumed-buffer reports retained headroom
and no underrun callback was observed; final renderer metrics were unavailable.
This establishes malformed speech input, not that it explains every reported
acoustic glitch. Separate underruns later in the recording remain distinct.
Keep the accepted helper, sampling, decoder windows and buffering fixed while
qualifying this parser repair. Regression coverage includes empty/nonempty and
unfinished thought blocks, every UTF-8 split boundary, single-character deltas,
and agreement between live routing and final parsing.

## Production-path qualification

Tests must call the production policy they claim to qualify. Do not maintain a
benchmark-only text chunker, normalizer, scheduler, compute-arbitration policy,
or simulated playback policy as a substitute for the application. Inject inputs
and failures at the boundary under test, then execute its real implementation.
For whole-path behavior, retain the real event router, sentence admission,
adaptive scheduler, native synthesis command and PCM renderer together.

The frontend voice harness executes the actual `onChatEvent`,
`beginLiveVoiceTts`, `takeLiveSpeechChunk` and imported
`voice-pipeline-scheduler.js` implementations. It controls native IPC and audio
I/O to test ownership, reasoning exclusion, sentence admission, EOF ordering,
backpressure and cancellation. Its synthetic clocks and PCM are not latency or
physical-playback evidence. Backend integration tests must call the same `MomSpeech` synthesis
entry points as the app; isolated model tests qualify only that model boundary.

The shared PCM worklet context keeps its output clock running between turns.
Natural completion and cancellation retire only the owning node, analyser and
ring; they never suspend a shared context that could retain device-bound audio
for a later speaker. Explicit pause holds the processor's ring position while
rendering silence. Late callbacks have authority only over their exact node
and owner. Tests cover drain/pause races, cancellation during startup, and
cross-owner teardown. These lifecycle checks do not establish audible speaker
isolation; a packaged cross-persona stop-and-call test remains necessary.

Call performance is measured by starting the exact packaged candidate through
its normal UI in an isolated profile and reading its persisted
`voice-latency-history` with `scripts/voice-latency-report.mjs`. This runs the
production coordinator, native generation and renderer. Bind the trace to the
candidate executable, source, model, voice reference and device. Record cold
and warm pickup, content availability, speech dispatch, first PCM, first render,
renderer underruns and cancellation settlement separately. A missing stage is
unknown, never zero. Physical speaker quality still requires listening or a
recording; first PCM is not first audible speech.

There is currently no deterministic automated packaged-call driver. That is an
explicit qualification gap, not a reason to invent a second call pipeline.
The retired `real_resident_audio_to_qwen_pipeline_exercises_compute_lease`
benchmark had private word-count chunking, buffering calculations and reasoning
routing. Its historical receipts are preserved but cannot qualify current call
latency or playback. The separate-process CLI pipeline benchmark was retired
for the same reason. Component tests remain useful only at their stated boundary.

## Shared generation and speech admission

Direct chat and persona/mention dispatch must use the same incremental model
output parser, including tool continuations and final persisted results. A
successful direct-chat test does not qualify persona streaming. Protocol markers
and reasoning must be separated before visible content reaches any speech
consumer. Stored assistant messages are projected through the same parser for
display, copy and Read Aloud; the original storage and explicit raw exports are
preserved. User and tool text are never rewritten by that assistant projection.

Read Aloud and precommit speech share one synthesis-request constructor and the
same Qwen helper, voice reference cache, sampling, PCM framing and renderer.
The helper owns the final bounded-text admission for every request: already
bounded inputs remain one unchanged normalized utterance; long inputs split at
complete sentences, then clauses or a 64-word limit. Live text availability can
cause earlier dispatch, but must not bypass this admission or invent a second
normalizer. Explicit reading uses the shared adaptive PCM runway with local
calibration so a long reading does not retune a later call.

Regression evidence must cross both entry points: persona output routing and
persistence, exact Read Aloud target selection, shared request parity, long-input
word conservation, and short-input audio parity. Renderer tests do not establish
pleasant audio; keep real helper stop reasons, packet timing and audition audio
separate from physical playback acceptance.

## Voice-note storage and visibility

A persona voice note has two projections: saved audio for the user and the
assistant transcript for model context. Publish its WAV through encrypted
attachment storage, atomically binding it to the exact committed assistant
message and visible text. In-memory playback is not durable publication.
Navigation must not cancel a completed recording's pending publication; failed
publication retains the session recording and offers retry. Restored recordings
render as audio bubbles. Generated assistant audio is excluded before model
media budgeting, without excluding ordinary user audio. An edit creates a new
message occurrence and cannot inherit the previous wording's recording.
Publishing this presentation-only recording must preserve the rolled inference
window and checkpoint eligibility. Exclusions use authenticated attachment
metadata with exact conversation/message ownership, never a filename. Ordinary
media, transcript edits, branch changes and profile changes still invalidate the
relevant inference authority.

Regression checks cover both PCM/message-ID completion orders, failed-save retry,
encrypted-store reopen, exact audio bytes, stale-message rejection, and separate
rendered/model projections. Older session-only recordings cannot be recovered
from their text: show an unavailable voice bubble rather than silently replacing
the note with text or claiming a regenerated recording is the original.

## Call startup, hangup and navigation

Hangup releases the visible call and its startup ownership before native stop
acknowledgements settle. A delayed microphone permission result, audio-context
resume, greeting completion or startup failure belongs to its original call
epoch. It cannot publish into a later call, release that call's microphone,
clear its dispatch ownership or retarget the selected chat. Expected late
cancellation is silent. Genuine startup failures remain dismissible and visible
until dismissal or retry, with the failed stage recorded in latency diagnostics.

Chat rendering checks model configuration without querying resident slots.
The shared native client holds its state lock during model acquisition; a
resident-status query in the render path therefore makes chat switching wait
behind loading. Explicit engine diagnostics still query actual residency.
Configured readiness does not claim that a model has loaded or passed inference.

Regression checks hold startup and teardown promises open across hangup,
navigation and redial, and verify that stale completions preserve the new owner.
These checks do not establish pickup on another Mac. In-flight native persona
preparation currently has no per-call cancellation handle and can continue
after hangup; its revision-conditional imports and prefills must not overwrite
newer generation. Keep cache preparation until an alternative proves equivalent
resident and persisted prefix reuse, rather than exchanging a startup wait for
uncached prompt processing.

## Sentence prosody and streaming PCM continuity

Sentence-level text admission and PCM startup buffering are separate decisions.
Give Qwen complete sentences for prosody, but do not require all of a healthy
sentence's PCM before playback. A cold producer may release the startup probe
after five measured packets, at least 1.2 seconds of PCM, and sustained production
faster than playback. Both recent and cumulative cadence must qualify, with
headroom for the observed startup deficit and packet gap. Missing measurements,
slow production, and prior underrun evidence retain the conservative fallback.
This is a bounded startup decision, not a guarantee against a later producer
stall. Retain sentence boundaries, cancellation, and adaptive underrun recovery.

TTS completion receipts distinguish EOS from token-budget exhaustion, pad/cycle
guards, and cancellation. Unreported counts remain unknown. A bounded packet
timeline records arrival offsets and cumulative frame counts, without speech
text or microphone PCM, so startup policies can be replayed against actual
production. A replay verifies the observed schedule, not physical speaker
quality or behavior on another Mac.

Speculative failures cannot acquire the next microphone turn before playback is
authorized, or replace an existing capture. Native blocked/no-result envelopes
must release an unconfirmed recording-import waiter before joining its worker,
just as transport errors do. An already confirmed recording remains confirmed.

The vocoder decodes generated acoustic codes only. Speaker conditioning belongs
to the talker; unrelated reference-waveform codes must not fill an initial or
expanding decoder window. Filling the early 32-frame window with reference codes
introduced a repeatable onset pulse. Same-code comparisons against a full causal
decode isolated this effect without changing sampling, sentence boundaries,
emission cadence, overlap, or playback scheduling. Keep the accepted executable
and profile as the comparison baseline; removal of the artificial line-noise
feature predates this repair and must not be presented as a new change.

The opt-in real-model regression is
`Qwen3TTSTests.EmbeddingOnlyReferenceDecoderParityTests/testReferenceCodesDoNotChangeEmbeddingOnlyPCM`.
It holds the extracted speaker embedding, text, and MLX seed constant, then
compares PCM with no decoder reference codes and synthetic 16-by-32 and
16-by-80 code matrices. Supply a private JSON file with `model_path`,
`reference_wav_path`, `output_path`, and `seed` fields, then run:

```sh
QWEN_ONSET_FIX_DIAGNOSTIC_JSON=/absolute/path/to/input.json \
  swift test --package-path products/mom/apps/mom-llama/qwen3-tts-helper \
  --filter EmbeddingOnlyReferenceDecoderParityTests/testReferenceCodesDoNotChangeEmbeddingOnlyPCM
```

Without the environment variable the test is skipped. The diagnostic source
contains only generic text and synthetic codec IDs; model and reference paths
belong in the external input file. On the qualified local model, the d416
baseline failed both equality comparisons (maximum absolute differences 0.204
and 0.367); the onset fix passed both with zero difference across 122,880
samples. This verifies decoder independence for this fixed-seed input, not
general audio quality or physical playback smoothness.

Long quiet tails are a separate, still-open acoustic defect. A fixed-seed
29-word reproduction reached its 157-frame budget with 3.73 seconds of terminal
low-energy PCM in all three decodes: reference-padded, generated-only rolling,
and full-prefix causal. There were no nonfinite raw decoder samples. That
isolates this reproduction to the generated sequence, but does not locate the
earlier call's aggregate 4.69-second low-energy interval. Do not promote a
blanket silence filter on this evidence: it can remove quiet speech, and holding
PCM until EOF can starve otherwise continuous playback. Numeric signal evidence
and zero underruns do not establish a successful human listening test.

A temporary sampler ledger reproduced the same 157 frames byte-for-byte. All
text had been supplied by step 36. During the terminal quiet interval, EOS was
unsuppressed and ranked 2–9; every step retained 50 finite, valid candidates.
Generation exhausted its frame budget without selecting EOS. This rules out
an empty sampling set and unintended EOS suppression for this reproduction,
not for every possible input. The diagnostic instrumentation is not shipped.

Call speech normally submits one complete sentence per Qwen request. It does
not start a fresh acoustic trajectory after six words or at ordinary commas.
Only an unusually long/unpunctuated sentence reaches the 64-complete-word cap;
that fallback prefers a clause boundary and is recorded in segment telemetry.
A final unpunctuated tail is sent when Gemma finishes. The audio runway scheduler
still controls when a ready sentence is dispatched, but cannot shorten its text.
The helper's acoustic-token ceiling and renderer's duration budget accommodate
these longer requests. Streaming audio and cancellation remain active within a
sentence; this does not require buffering the full audio sentence before playing.

The rolling vocoder crossfade must blend two decodes of the same timeline.
Previously it withheld 512 samples, then blended that tail over the next batch's
new samples. Four batches of 2/3/5/5 codec frames plus the final tail emitted
27,264 samples rather than 28,800, dropping 64 ms at 24 kHz. Decoding the retained
overlap as well as the new frames preserves duration and boundary alignment.
Regression checks compare a continuous synthetic waveform across varying batch
sizes, including both empty and nonempty final remainders. This repairs sample
loss; it does not establish the cause of multi-second physical-output pauses.

## Interrupted replies and next-turn history

Stopping speech is not evidence that the reply has finished saving. A call can
already be capturing the next utterance while the previous confirmed reply is
still completing its native operation. Preparing the next prompt during that
interval reads the previous leaf and creates sibling exchanges; the store
correctly preserves the existing active branch, but subsequent replies can then
follow a different branch from the one the caller just heard.

Each live speech controller tracks its exact native generation promise. The
next prompt is prepared only after that promise settles, including failure or
cancellation, and after rechecking the call epoch. Microphone capture continues
and playback interruption remains immediate. A renderer timeout, speech-backend stop,
or cancellation request is not a substitute for native operation settlement.
This applies to call greetings, speculative audio, and complete-recording
fallback dispatches. `next_turn_history_settled.wait_ms` separates this ordering
wait from the subsequent prompt-preparation measurement.

The behavioral regression interrupts playback before the prior operation
commits, verifies capture remains available and preparation has not started,
then verifies the next prompt sees the committed leaf. It also covers native
failure and hangup during the wait. These checks establish ordering only;
physical speaker testing remains a separate acceptance step.

Text completion and history completion are separate milestones. Incremental
voice generation emits the request-owned `text_completed` event, carrying the
final assistant text in `delta`, before waiting for recording confirmation.
The renderer flushes its last sentence at this milestone. Otherwise a short
reply without trailing whitespace can wait for command completion before
starting synthesis, while that command waits for the first synthesized PCM to
confirm the recording. The ordinary `completed` event remains post-commit;
text EOF never releases the next-turn history barrier or the chat busy lease.
Queued background transcription also waits for that commit before updating
older turns, then rechecks the call epoch and caller activity.
Late text completion from an aborted or replaced request cannot flush its
replacement. Duplicate completion does not synthesize the reply twice.

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
committed conversation checkpoints are scheduled during idle time and after
hangup. Shutdown cancels and joins maintenance; it never starts an export. A saved compatible session is still restored before persona-only
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
boundary. Hangup requests a conversation-scoped idle checkpoint without waiting on the UI
path. The earlier shutdown-only export capability has been removed: quit never
starts a fresh native export or flushes a checkpoint backlog. A per-call opaque
activity ID is registered before microphone/model setup and remains active
until call-owned stop acknowledgements settle. Active calls suppress native
snapshotting and persistence; a matching-ID clear schedules later idle work.
See the opportunistic maintenance section below for the current worker and
publication contract.

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


## Speech backend selection and evidence

Calls select the in-process PocketTTS CPU backend. Voice notes and Read Aloud
select Qwen. Both enter the same persona speech operation registry, sentence
buffering, contiguous PCM validation, cancellation settlement, and renderer.
A missing requested backend is an actionable failure; it must not silently
fall back to the other model. Only an admitted Qwen request owns a Metal lease.
Pocket never reserves Metal, and call startup does not warm Qwen in the
background. Notes and Read Aloud retain cooperative Metal arbitration with Gemma.

Pocket voice conditioning is prepared offline from the full official encoder
checkpoint and the same reviewed reference clips used by Qwen. The package
contains the projected embeddings, tokenizer, configuration and provenance;
first-run acquisition installs the pinned public generation checkpoint. Runtime
calls do not require Python, network access, a voice encoder, or fresh reference
encoding. Each embedding is bound to its source clip and model revision.

Qualification must use the packaged UI and preserve selected backend/model IDs,
first PCM and first render timestamps, renderer underruns, completion status,
request cancellation settlement, and Metal lease counts. A measured zero is
not missing data. Standalone engine benchmarks may explain a component but
cannot establish call pickup or continuity. Keep the previous candidate and
its traces intact until the replacement has passed its actual app checks.

## Navigation during checkpoint persistence

Chat selection and draft retention must not perform storage waits on the native
UI thread. Their blocking workers retain the application work lease; frontend
selection and draft writes preserve invocation order. Hidden settings must not
trigger model discovery during navigation. Refresh conversation settings when
they become visible.

Opportunistic session checkpoints yield at the next encrypted chunk boundary
when an ordinary store writer is waiting. A partially written part rolls back;
already committed parts remain staged and the previous complete checkpoint
remains authoritative. Resume the retained snapshot at the next unwritten part
when idle. Do not weaken encrypted-store authentication or cache
compatibility to improve navigation latency.

Packaged-app qualification records selection queue time, selection command time,
chat rendering and switch-to-paint time, plus native worker queue/work duration
and storage writer waits. These diagnostics contain numbers and stage names,
never conversation identifiers or content. Measure populated chat switches both
while idle and after hangup, when a background checkpoint can be active;
component preemption tests alone do not establish visible navigation latency.


## Opportunistic maintenance and prompt quit (19 October 2026)

Each runtime owns one joinable session-checkpoint worker shared by its dispatch
children. Every committed text or voice turn coalesces that conversation's
pending request. After a three-second idle window, the worker exports only an
exact still-resident committed request. It never reloads an evicted model to
rebuild a cache. A foreground operation, active call, waiting store writer, or
cancellation preempts maintenance. Deferred work retries with bounded delay;
errors back off. Requests and cancellation authority are runtime-local.

The worker retains at most one immutable native snapshot. This is still a full
native export, not delta KV serialization; the existing 8 GiB per-entry ceiling
remains an upper bound, not an observed memory requirement or M2 qualification.
Persistence commits 16 MiB encrypted parts in separate storage transactions.
A staged part is never a cache hit. Only a complete, source-authorized manifest
is atomically published, preserving the previous complete version on
preemption, cancellation, failure or crash. In-process retry resumes the same
snapshot at its unwritten part. A later writer fences out and reclaims an
abandoned staging slot; old versions are reclaimed in bounded idle transactions.
Authentication and model/layout compatibility checks are unchanged.

Paused work holds no Persona/publication lock. Publication uses a nonblocking
short-lived owner lease and rechecks conversation authority inside the metadata
transaction. Cache clear invalidates older leases and retires the staging slot
under the same authority guard, preventing a delayed save from reviving cleared
state. No fresh export, transcription, orphan cleanup or cache write is launched
by quit. Cancellation signals native export preemption before waiting for locks,
wakes the idle worker, discards queued cache work and joins the active unit.
Saved messages and original recordings were committed independently of this
reusable cache. If quit interrupts a save, restart can use the previous complete
checkpoint and evaluate the newer suffix; it must never import partial state.

Historical transcription uses the existing production recovery/commit path.
One coalesced timer runs after foreground ownership is released and scans
pending recordings on active branches across conversations without selecting
those chats. Foreground work cancels the timer and preempts active ASR. Already
started transcript commits settle normally. Failed/empty transcription backs
off; a complete scan prunes stale retry identities. Original voice media and
user-visible message type stay intact. No transcription backlog is drained at
quit or chat selection.

Record native snapshot, reconstruction, part-write maximum, publication, and
total persistence time separately. The terminal shutdown summary records
checkpoint-worker join, final approval sweep, speech join and native join
independently, alongside their completion evidence. Qualify with production
idle publication plus authenticated restart, quit with queued and active cache
work, and packaged call/navigation measurements. Fixture tests prove race and
ownership rules; they do not establish physical audio quality, M2 performance,
or a wall-clock upper bound for native/OS calls.

Before the consolidated gate, run the complete product frontend suite, the
affected Rust package tests in their default parallel mode, and package clippy
with all targets and warnings denied. A serialized diagnostic run does not
replace parallel qualification. Whole-page UI harnesses must load the same
production prerequisite scripts in the same order as `index.html`; inject only
DOM, clock and native-I/O boundaries, not an alternate scheduler. Tests that
mutate global cache state must coordinate with every test that clears that state.
After a failure, preserve the receipt, repair the specific cause, and verify its
focused gate before repeating the broader gate. A passing component result
still does not replace the packaged quit/restart measurements above.
