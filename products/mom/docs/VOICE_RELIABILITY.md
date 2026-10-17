# Voice reliability qualification

## Evidence snapshot — 17 October 2026

This is a dated implementation and evidence snapshot, not a live qualification
declaration. Each local gate, candidate build, and device run applies only to
the exact source SHA and tree recorded in its receipt or manifest; a later
receipt supersedes an earlier one only for its own exact identity.

Historical focused counts below are tied to their retained receipts. The
latest fully passing consolidated gate before these changes is `5c47b94a`.
The `599ecbde` gate failed on three Clippy diagnostics and rejected checkout
cleanliness after scratch files appeared during the run. Both the failure and
the fix are retained; scratch work now stays outside the source checkout.
The shutdown capability and native capture ledger below require a subsequent
clean exact-commit gate. Candidate attempt 01 does not validate this tree.
Do not promote a package unless its manifest binds the final clean source and
tree to a complete passing local gate.
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
