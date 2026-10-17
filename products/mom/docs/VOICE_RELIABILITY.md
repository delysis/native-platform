# Voice reliability qualification

## Evidence snapshot — 16 October 2026

This is a dated implementation and evidence snapshot, not a live qualification
declaration. Each local gate, candidate build, and device run applies only to
the exact source SHA and tree recorded in its receipt or manifest; a later
receipt supersedes an earlier one only for its own exact identity.

The M4 Max runtime evidence below covers bounded voice-history rollover with
synthetic input. Under a manual 21,845 MiB memory budget, the initial
8,192-token setting resolved to 18,944 native context cells in that run. It is
not an M2 measurement or packaged-call acceptance. The two latest consolidated
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
remains independent and responsive. All streamed speech segments preserve an
incomplete final word until another token or end-of-stream establishes it.

Qualification also exposed a process failure: attaching a UI tool to an exited
candidate could silently relaunch it without the isolated data and memory-budget
environment. A distinct app identifier did not isolate the runtime store. The
candidate builder now embeds a safe default data namespace, and qualification
must verify the live PID, executable and effective data path before attaching UI
automation. Those earlier launches are failed attempts, not 32 GB budget evidence.
Never infer process identity from an app window or a successful build.

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
boundary. No automatic full-conversation KV checkpoint at hangup is claimed.

The optional staged Parakeet transcription tap remains bounded to 40,960
samples (2.56 s at 16 kHz), including in-flight audio; it is not a live ASR
fan-out on the Gemma microphone path. A live call sends microphone PCM through
the bounded Gemma prefill queue, with the saved recording retained as recovery
source. Overflow and stalled native acknowledgements fail explicitly. Deferred
PCM replay is tested for exactly-once delivery, including chunks received
during an asynchronous drain. The saved recording remains authoritative.

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
context only. No release app has been launched for this qualification. No
paired baseline/ABBA comparison has been performed; `97e6e459` remains a
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
