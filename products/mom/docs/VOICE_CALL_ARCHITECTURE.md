# Voice call architecture

Status: native capture, adaptive acoustic detection, incremental model prefill,
silence-edge speculation, streaming synthesis, and AudioWorklet playback are
implemented. Packaged conversational latency and barge-in remain qualification
work; a component benchmark is not packaged acceptance.

## Product behavior

A call is one continuous, full-duplex session. Mom may listen while a consultant
speaks, stop playback when the user begins speaking, and resume after a complete
user turn. The transcript and retained audio remain ordinary conversation data;
temporary capture, model input, and playback buffers are bounded to the call.

The call surface exposes only familiar call controls and live state through
icons, animation, and accessibility descriptions. Implementation status,
thresholds, model names, and instructions do not belong in visible chrome.

## Audio path

1. Apple Voice Processing I/O captures microphone audio with echo cancellation
   and automatic gain control while speaker playback remains active.
2. Capture is converted once to signed mono 16 kHz PCM and assigned monotonically
   increasing frame numbers. A bounded ring buffer keeps the exact recent audio.
3. A constant-storage streaming percentile estimator continuously adapts the
   acoustic threshold. Detection runs on 1 ms frames with a 4 ms attack and
   20 ms release; resumed speech cancels uncommitted generation.
4. A compactor omits long silence from model input while preserving a reversible
   ledger from compacted samples to original frame numbers and times.
5. The audio model consumes exact 15,840-sample projector blocks during speech.
   These match Gemma 4's 100-frame acoustic unit. A trailing partial block is
   sealed immediately at silence; no additional response delay is inserted.
6. Speech synthesis produces bounded audio chunks. Playback begins once the first
   verified chunk is available and remains cancellable at every boundary.

Every capture, inference, and playback operation has one owner and one terminal
result. Starting a new turn cancels the prior generation and playback before it
can publish more audio or text.

The caller may speak for any length of time while generation is pre-armed. Once
native VAD releases the silence gate, however, the supervised native ticket
has 10 seconds for a call turn and 20 seconds for a voice note after input is
sealed; pickup has a separate 12-second renderer deadline. The renderer's
failsafe follows the native deadline by two seconds. A stalled request is
cancelled, reported visibly, and persisted as `generation_timeout`; it cannot leave the
call silently waiting or close the call without an explanation.

## Turn detection

The currently qualified trigger is the native adaptive acoustic edge. Generation
is pre-armed while speech continues and sleeps on the capture owner's condition
variable. The first silence edge seals the current model input and releases that
worker directly. Resumed speech aborts the uncommitted generation and retargets
capture to a fresh model session. Parakeet's typed `[EOU]` event remains a future
semantic input; it does not add delay to the current silence-edge path.

The acoustic edge is intentionally eager. Speculative output remains private
until PCM is ready, so a hesitation can belay work without publishing stale text
or audio. Voice notes retain a 900 ms send commitment threshold, but model work
starts at the earlier 20 ms silence edge and overlaps that interval.

## Barge-in

Microphone analysis never stops during playback. When speech onset is confirmed,
the player ducks within 100 ms p95, stops within 150 ms p95, and the active
generation receives cancellation acknowledgement within 200 ms p95. Echo-only
input must not trigger interruption. A stopped operation cannot publish a late
chunk, transcript, or completion into the next turn.

Native capture opens when reply playback starts and retains exact PCM before a
model session is attached. Qwen completes its PCM production first; Gemma then
prepares while the buffered tail plays, and retarget replays the retained audio
exactly once into that session. A confirmed user onset stops playback locally
at once and is timed from detector event through completed speaker shutdown.

## Silence compaction

Compaction preserves 160 ms before speech and 240 ms after it. Pauses of 600 ms
or less remain exact; longer pauses retain 160 ms plus a ledger entry containing
the original frame interval and duration. The ledger is stored with the request
receipt so timestamps, interruption decisions, and failures remain auditable.
Compacted Gemma audio is capped below its 30-second input limit.

## Incremental prefill

The native boundary owns explicit begin, push, seal, speculate, confirm, abort,
and retarget operations. PCM enters a preallocated 30-second 16 kHz ring. The
projector reader sleeps on exact boundary notifications and uses two resident KV
sequences as ping-pong checkpoints, avoiding a large state-copy at every block.

## Measurement

Every turn records capture callback, resampling, detector, queue, ring, projector,
decode, renderer delivery, TTS, first PCM, playback, and underrun timings. Repeated
VAD transitions are coalesced in memory so they cannot crowd terminal milestones
out of the bounded trace or cause a write storm. Native monotonic timestamps are
used across renderer delivery whenever available.

The report preserves every scalar metric in the durable renderer summary. New
instrumentation therefore reaches JSON and console analysis without a second,
easy-to-miss schema edit. The aggregate scoreboard includes call pickup,
projector warmup, silence-event delivery, first token, first PCM, playback, and
underrun distributions.

The latest durable checkpoint for every trace is summarized with:

```sh
node products/mom/scripts/voice-latency-report.mjs --limit 20
```

Use `--json` for machine analysis. Historical traces created before coalescing
may have 64 VAD-only stages and legitimately report later metrics as unavailable.

The ignored native integration benchmark
`real_incremental_voice_prefill_reports_silence_to_first_token` streams a real
16 kHz voice sample in 10 ms producer blocks through the exact resident Metal
audio path. It runs twice in one process so model startup and steady-state turn
latency remain separate. Its first turn is deliberately only 500 ms long so
static prompt work cannot hide beneath a long utterance; the second turn uses
four seconds to exercise multiple projector blocks. On the qualified M4 Max,
the resident 51.2K-context path reached first token 203 ms after short-turn
silence and 153 ms after long-turn silence. Set `MOM_LLAMA_MULTIMODAL_MODEL_PATH`,
`MOM_LLAMA_MM_PROJ_PATH`, and `MOM_LLAMA_TEST_VOICE_WAV`, then run the test with
`--ignored --nocapture`. The checked 2026-10-15 receipt is in
`products/mom/artifacts/latency/2026-10-15-incremental-voice-native.json`.

First-segment sizing is measured separately against the resident Qwen helper.
The 2026-10-15 five-run sweep at each of four, five, and six words is in
`products/mom/artifacts/latency/2026-10-15-qwen-tts-first-segment-sweep.json`.
Four words reached first PCM at 127.8 ms median / 136.4 ms p95, synthesized at
no worse than 0.69 real-time factor, and retained at least 395.4 ms more audio
than synthesis wall time. That experiment established the minimum viable
runway, but independent four-word TTS requests produced conspicuous prosody
resets in the packaged app. The live chunker now releases a naturally
punctuated clause as soon as it is usable, with an unpunctuated fallback at six
complete words for a call and ten for a voice note. Later fallbacks are eight
and twelve words respectively. Qwen's first two codec frames yield about
138.7 ms of PCM, so the renderer starts from that first complete packet rather
than waiting for the next packet and inflating its runway to about 357 ms. The
audio-thread ring still recovers coherently behind a 100 ms rebuffer boundary.
Every trace records packet count, first-packet duration, maximum packet-arrival
gap, and minimum producer lead so this lower bound remains continuously audited.

The combined resident-model benchmark in
`products/mom/artifacts/latency/2026-10-15-voice-pipeline-contention.json`
feeds every real Gemma content delta through the same semantic chunk boundary as
the product, then starts Qwen while both models share Metal. This deliberately
includes the time from first token to a natural speakable fragment instead of
pretending TTS can begin on one token. The latest 500 ms and four-second turns
reached first token at 215 ms and 149 ms, speakable text at 272 ms and 237 ms,
and first PCM at 447 ms and 401 ms after the silence edge. The first 139 ms PCM
packet exceeds the 120 ms buffered-audio floor, but packet-arrival jitter can
still require a short wall-clock guard before that packet begins playing. Qwen's
first PCM took 164-175 ms under contention, compared with
137 ms alone. Callback gaps reached 212-215 ms, but the preceding packet covered
most of each gap: the measured startup runway required to avoid underflow was
11-40 ms. Startup protects the uncovered portion of the observed gap with a
callback margin and a 120 ms floor. Playback should wait only the uncovered
wall-clock deficit, rather than rounding up to the next callback. A real
underrun or a gap that exceeds one packet's coverage raises protection
immediately.

The native half of this benchmark begins both measured turns on fresh named
workers, matching the packaged command's supervised blocking boundary. This is
intentional regression coverage for the prior prefill-worker failure: direct
admission on the test thread is insufficient evidence. Both turns retained the
resident 51.2K context, hit the settings cache and model residency, and admitted
their sessions in 4 ms wall time.

After the first segment, Qwen admission is controlled by live audio runway
rather than an unconditional segment loop. The lead controller continuously
adapts its dispatch floor from time to first audio, packet duration and jitter,
observed synthesis real-time factor, and any renderer underrun. It immediately
raises protection after an underrun and decays that penalty only after healthy
segments. While runway exceeds the floor, semantic text can accumulate but no
additional PCM is synthesized; consumption notifications from the audio-thread
ring wake the producer at the boundary. A bounded timer covers the non-worklet
fallback. This keeps PCM close enough to real time for interruption without
depending on fixed machine-specific buffering. Traces record the selected
runway, wait count and duration, maximum runway, observed rate, and underrun
penalty.

The native runtimes share a second hysteretic controller for Metal time. Qwen
claims the real-time compute lease before synthesis and while produced PCM lead
is below its adaptive low-water mark. Gemma observes that lease only between
token decodes, preserving its KV state and checking cancellation every 2 ms
while it waits. Qwen releases the lease once its lead reaches the high-water
mark; Gemma then resumes until the lead falls again. Packet duration and the
largest observed production gap raise both watermarks on slower or burstier
machines. Each trace records acquisitions, releases, Gemma yield count and
duration, minimum and maximum producer lead, and the selected watermarks. This
keeps urgent audio production ahead of playback without allowing synthesized
speech or semantic generation to run arbitrarily far ahead.

Semantic production has a separate exact-request hysteresis band. Once a real
TTS-dispatchable phrase reaches 16 queued words, Gemma pauses at the next token
boundary with its KV state intact. It resumes after the queue drains to eight
words. An incomplete phrase that TTS cannot consume never pauses generation.
The width of this band is the minimum useful compute quantum: it avoids a model
handoff for every token while still bounding how far a reply can run ahead of
interruptible playback. Traces record the number of transitions, command
round-trip time, paused time, native token-boundary yields, and maximum queued
words. The PCM controller likewise crosses its full adaptive low/high-water
band before handing Metal back, so packet jitter raises the quantum instead of
causing faster ping-pong.

Moving synthesis off Metal remains a valid way to remove that contention
entirely. Chatterbox Nano is a CPU candidate, but aggregate faster-than-real-time
throughput is not sufficient admission evidence: its current whole-utterance
path must be measured for first PCM on the same short segments, sustained rate,
memory residency, interruption waste, and persona similarity. ANE deployment
requires a separately qualified Core ML conversion and operator path. Until one
of those paths passes the same trace gates, the resident Qwen scheduler remains
authoritative.

The companion chunk-size sweep is in
`products/mom/artifacts/latency/2026-10-15-qwen-tts-chunk-size-sweep.json`.
Across five randomized resident runs at each size, two through eight codec frames
all reached first PCM in 125–133 ms and synthesized at 0.59–0.66 real-time
factor. A live trace that fell to 1.45 real-time factor therefore identified
shared Metal contention rather than an undersized Qwen emission. The next Gemma
audio session now begins only after the final TTS request has produced its PCM,
while that buffered tail is playing. This preserves overlap with playback while
keeping the audio producer's critical path free of competing model preparation.
Each durable trace records every segment's synthesis wall time, output duration,
real-time factor, buffered runway, the final aggregate and worst-segment rates,
the TTS-to-next-prefill boundary, next-session preparation time, remaining PCM
when preparation completes, and whether preparation overlapped active TTS.

The live Gemma path now admits audio at the actual unified-audio projector
boundary. The intended 12B model loads llama.cpp's `gemma4ua` projector: an
encoder-free frontend that maps each independent 640-sample raw-waveform frame
to one model token. At 16 kHz that is 40 ms, with no fixed-length padding and no
downsampling. The earlier 15,840-sample batching rule belonged to the older
spectrogram frontend and retained almost a second of already-heard speech. The
resident path and its acceptance now use 640-sample projector frames.

Loading the GGUF does not compile the first Metal audio-projector evaluation.
The packaged runtime now feeds one exact 640-sample frame through the resident
incremental-audio path during supervised launch warm-up and does not report the
voice runtime ready until that evaluation finishes. Calls await the same warm
promise before conversation setup. Routine startup work cannot preempt it, and
the latency trace records `audio_projector_warm_us` plus the explicit
`call_runtime_ready` boundary. This moves the observed 0.7-second first-frame
compile out of the caller's turn while leaving the window event loop and local
hang-up control responsive.

The acceptance producer is paced from one monotonic audio clock: 160 samples
arrive every 10 ms against absolute deadlines. Relative sleeps are forbidden
because scheduler drift would slow the imaginary microphone and give prefill
time that a phone call does not have. In the current four-second exact run,
Gemma projected all 100 frames during capture. Projector work totaled 371.6 ms,
the ring peaked at 90 ms, and the last frame finished 7.6 ms before physical
silence. Silence-to-generation-start was 28.7 ms, generation-start-to-first-token
was 80.8 ms, and silence-to-first-token was 109.4 ms. Four complete words were
available to Qwen at 207.5 ms; first PCM and safe playback both arrived at
349.8 ms. The exact receipt is
`products/mom/artifacts/latency/2026-10-16-streaming-audio-to-audio-40ms.json`.

Qwen startup no longer walks every persona prefix while the application is
already accepting microphone audio. That eager loop could monopolize Metal
long enough for Gemma's 40 ms projector frames to accumulate: one captured run
left 1.43 seconds of audio after physical silence and delayed the first token to
1.89 seconds. Startup now loads the one resident Qwen process and runs one
bounded two-code-frame probe to compile the shared talker, code-predictor, and
vocoder shapes. The speech runtime joins that probe before advertising itself
as ready. Reference encodings remain content-addressed on disk; the selected
persona's prepared prefix is admitted lazily and preserves its exact voice.

The measured startup boundary took 592.0 ms to load the resident process and
237.4 ms for the compile probe, 830.7 ms total. Three subsequent exact 12B
audio-stream runs kept the ring high-water mark at 90 ms, completed their final
projector frame 5.5-8.8 ms before physical silence, produced Gemma's first token
in 106.2-109.2 ms, and produced playable Qwen PCM in 357.1-368.6 ms. These runs
used Christine's exact disk-cached reference with a deliberate first-use prefix
miss. The end-to-end acceptance now rejects more than 80 ms of projector work
after silence, more than 200 ms in the input ring, a first token beyond 500 ms,
or first PCM beyond 750 ms. The repeated receipt is
`products/mom/artifacts/latency/2026-10-16-streaming-audio-to-audio-bounded-warm.json`.

Gemma 4's 422.86M-parameter MTP assistant is qualified against this same audio
boundary rather than inferred from token throughput. The paired harness keeps
the target model, assistant, four seconds of audio, prompt, temperature-one
sampler, seed, and resident Qwen process identical. It rejects a trial if draft
decoding changes the sampled reply. A depth sweep found two-token drafts to be
the useful operating point: 44.7 tokens/s with full acceptance, compared with
40.5 tokens/s without drafting. Three-token drafts fell to 36.4 tokens/s at 75%
acceptance, and deeper drafts deteriorated further. MTP cannot reduce the first
target-token time; both paths measured about 96.5 ms. Starting it immediately
also delayed the first dispatchable phrase because accepted tokens arrive in
verified bursts. The measured candidate schedule therefore emits the opening five
tokens normally, dispatches the first speech chunk, catches the assistant up,
then uses two-token MTP for the remainder. In the contended audio-in/audio-out
trial this preserved the 172 ms first speech handoff and shortened remaining
Gemma generation by about 20 ms. It did not materially change response onset,
which remained dominated by Qwen startup. The original receipt rounded
"playable" up to a later packet callback and therefore reported 529-540 ms.
The corrected underrun constraint permits playback between callbacks. A fresh
Rust-harness run measured the no-draft leg safe at 401 ms after the silence edge
and the MTP leg at 405 ms; the MTP leg reached first PCM at 394 ms and needed
another 10.7 ms of wall-clock runway. Buffered audio and wall-clock protection
are separate controls: the renderer keeps the 120 ms buffered-audio floor and
holds first PCM behind a short adaptive timer seeded at 18 ms including its
2 ms safety margin. Each packet
updates the exact deficit relative to first PCM and cumulative preceding audio;
an underrun raises the guard immediately. Playback can therefore begin between
callbacks without hoarding a second packet. The Rust harness records first PCM
and the mathematically earliest underrun-free start so this distinction remains
visible. That run is
`products/mom/artifacts/latency/2026-10-16-voice-pipeline-mtp-ab-rust.json`.

The external paired harness cannot validate the product compute lease: Gemma
and Qwen are child processes there, while the lease is deliberately
process-local. The app now has an ignored same-process acceptance,
`real_resident_audio_to_qwen_pipeline_exercises_compute_lease`, which feeds PCM
at real-time cadence through the resident incremental-audio path and dispatches
the exact app Qwen route at the first speakable phrase. A fixed
temperature-one seed now makes the text side of that acceptance repeatable.
It exposed why naive time slicing was wrong: allowing Gemma one decode every
60 ms while Qwen held the lease produced 19 fair passes but made the first
packet schedule require 75-83 ms of extra runway. Safe playback regressed to
534-551 ms even though Gemma completed at 1.62-1.63 seconds.

The current policy gives Qwen uninterrupted startup priority and renews that
window on every real PCM packet. A producer that stops making progress stops
renewing, so the bounded fair-pass fallback still prevents a dead TTS worker
from starving Gemma indefinitely. Once measured PCM lead crosses the 220 ms
high watermark, the lease releases and Gemma and Qwen overlap. Two identical
text/seed runs originally measured 156-158 ms from physical silence to first
Gemma text, 313-318 ms to the speech handoff, 458-479 ms to first PCM, and
469-496 ms to mathematically underrun-free playback. That first version of the
acceptance waited for 32 characters, however, while the product then released
four complete words. After making the acceptance use that historical boundary,
two repeated runs measured 156-160 ms to first text, 276-279 ms to speech
handoff, 434-440 ms to first PCM, and 448-454 ms to safe playback. Only
13-14 ms beyond first PCM was required. Gemma completed at 1.47 seconds after
one 0.76-0.77 second yield. This keeps the renderer's 16 ms learned first-call
guard valid while removing the startup contention introduced by periodic fair
passes.

With the same four-word input, Qwen emitted four packets, completed in
780-786 ms, and accumulated 430-432 ms of producer lead. This is materially
more stable than the overlong acceptance chunk, which produced 6-11 packets
and 0.69-1.34 seconds of lead.

The acceptance originally continued through the complete streamed reply using
bounded four-word semantic chunks and a playback-consuming reservoir. That
exposed two product scheduler bugs. First, terminal text previously
bypassed the chunk bound and could send the entire remaining reply to Qwen;
one measured request produced 7.44 seconds of audio and ran 3.34 seconds ahead
of playback. Terminal flush now drains short tails immediately but keeps long
tails bounded. Second, the maintenance target counted the entire packet and
segment-boundary interval as uncovered risk, including time the preceding PCM
was already playing and the scheduler's own deliberate wait. The target now
protects Qwen time-to-first-audio plus only the uncovered portion of an
internal packet gap.

Two full six-segment runs now begin safe playback at 437-446 ms, bound maximum
producer lead to 1.01-1.02 seconds, and spend 2.44-2.72 seconds deliberately
waiting for playback to consume the reservoir. Gemma receives a useful decode
window between the first two Qwen requests and completes at 2.51-2.56 seconds.
The 9.21-10.07 second synthesis pipeline stays just ahead of the corresponding
9.26-9.84 seconds of audio rather than racing seconds ahead, and finishes
before playback consumes the final PCM. In that historical configuration,
remaining onset work was the 116-121 ms needed to admit four complete words and
Qwen's 145-160 ms time to first PCM.
The deterministic A/B receipt is
`products/mom/artifacts/latency/2026-10-16-resident-voice-pipeline.json`.
The same-process acceptance now mirrors the packaged call's current clause
policy: natural punctuation can release earlier, otherwise the first segment
waits for six complete words and continuations wait for eight. The earlier
four-word onset figures remain useful component evidence but are not current
packaged-call latency claims; the next real-model run must replace them.
MTP remains a qualified candidate in the direct engine harness; it is not yet
wired into this resident production path, so current product diagrams and
receipts must not label the Gemma lane as MTP.

The receipt now classifies packet timing by segment. An interval between two
PCM callbacks from the same Qwen request is an internal producer interval; an
interval crossing Qwen requests is a segment-boundary interval and includes
the scheduler's intentional reservoir wait. A fresh full-path run measured a
278 ms maximum internal callback interval and a 978 ms maximum boundary
interval. The latter is not an underrun: the exact packet trace needed 32 ms
of initial runway, kept 139-991 ms of PCM available, and finished 11.694
seconds of audio synthesis at 11.338 seconds after silence. This distinction
removes the former false diagnosis that every approximately one-second
boundary gap represented choppy production. The classified receipt is
`products/mom/artifacts/latency/2026-10-16-resident-voice-pipeline-gap-classification.json`.

The first Qwen packet is now decomposed inside the resident Swift helper rather
than inferred from its outer wall time. In the latest exact product-path run,
physical silence to the first Gemma token was 145.9 ms; admitting four complete
words took another 81.2 ms. From dispatch to first PCM took 166.3 ms: Qwen codec
generation accounted for 102.0 ms, the first PCM decode window for 62.1 ms,
outer routing and event delivery for 1.75 ms, and the helper left only 0.21 ms
unclassified. The resident reference voice was a 95 us memory-cache hit. The
exact packet trace then required 13.9 ms of startup runway, placing safe playback
at 407.2 ms after physical silence. After that point, Gemma and subsequent Qwen
segments overlap playback; the scheduler deliberately waited 2.895 seconds in
aggregate to keep the reservoir between 139 and 869 ms instead of producing the
whole response ahead of the listener. The decomposition receipt is
`products/mom/artifacts/latency/2026-10-16-resident-voice-pipeline-first-packet-decomposition.json`.

The first-packet sweep then separated packet size from vocoder context. A
one-codec-frame packet arrived earlier but carried only 58.7 ms of PCM; its
four-frame refill forced 127.2 ms of startup runway and regressed safe onset to
481.0 ms, so it was rejected. Retaining two initial codec frames while reducing
only the first rolling vocoder context from 48 to 32 frames cut first decode
from 61.5-62.1 ms to 43.2-43.4 ms. Two complete native runs reached safe
playback at 369.9 and 376.3 ms, versus 400.6 and 407.2 ms for the preceding
48-frame controls. The generated review sample is unclipped (-3.12 dB peak,
-18.89 dB RMS); human listening remains the quality gate before calling this
setting packaged-app qualified. The A/B receipt is
`products/mom/artifacts/latency/2026-10-16-qwen-first-decode-window-ab.json`;
the rejected packet-size receipt is
`products/mom/artifacts/latency/2026-10-16-qwen-first-packet-one-frame-rejected.json`.

The resident talker now materializes the invariant role, speaker, and codec
prefix during background voice warm-up and retains its KV state in a bounded
32-entry cache keyed by admitted persona identity plus the exact reference
SHA-256. Adapter or model-state changes invalidate the cache. In a same-process
probe after acoustic graph warm-up, a prefix miss took 84.5 ms to produce the
first two codec frames and 128.5 ms to first PCM; the immediate cache hit took
67.1 ms and 106.5 ms respectively, saving 17.4 ms of code generation and
22.1 ms to PCM. The exact audio-in to Gemma to Qwen acceptance proved the
foreground request hit this cache and reached safe playback at 379.9 ms. Its
codec stage still took 94.5 ms while Gemma shared Metal, showing that startup
contention can consume the intrinsic cache gain. The measurement is
`products/mom/artifacts/latency/2026-10-16-qwen-resident-prefix-cache.json`.

The Rust-owned paired harness is
`products/mom/crates/mom-llama-cli/src/bin/voice-pipeline-benchmark.rs`. The
paired end-to-end receipt is
`products/mom/artifacts/latency/2026-10-15-voice-pipeline-mtp-ab.json`; the
acceptance/depth sweep is
`products/mom/artifacts/latency/2026-10-15-gemma4-mtp-depth-sweep.json`.
The authoritative latency path has no Python runtime or benchmark dependency:
Rust owns capture, Gemma execution, scheduling, measurement, and receipt
generation, while the resident Qwen process is the same Swift helper used by
the packaged macOS app.

The same Rust harness now runs with the packaged runtime's four native sequence
lanes and requires token-exact reuse of Gemma's resident text prefix. A
read-only prepared-input command had been revoking that prefix between voice
turns; preserving the fourth lane across read-only worker commands restored 4
tokens on the first measured turn and 47 of 86 prefix tokens on the next. With
audio delivered at its real 16 kHz clock in 10 ms writes and projected in 40 ms
frames, the four-second turn reached Gemma's first token 104 ms after physical
silence, dispatched speakable text at 191 ms, produced first Qwen PCM at 377 ms,
and had sufficient startup runway to begin playback at that point. The shorter
500 ms turn reached first token at 113 ms and playable audio at 531 ms because
its Qwen trace needed another 119 ms of runway. The exact receipt is
`products/mom/artifacts/latency/2026-10-16-resident-audio-prefix-e2e-v2.json`.

The packaged renderer now keeps the native producer and the AudioWorklet on a
single inspectable audio timeline. Each PCM packet records its monotonic packet
index, source sequence and sample offset, enqueue time on the AudioContext
clock, admission frame on the audio thread, buffered frames, and periodic
consumption credits. The worklet separately reports context resume, first
non-silent rendered frame, minimum live buffer, underrun onset, recovery, and
drain. End-to-end `silence_to_playback_ms` uses the first rendered sample when
that evidence exists; context resume remains a separate milestone. This lets a
packaged trace distinguish late synthesis, IPC/admission delay, renderer-thread
starvation, and an actual PCM underrun without inferring any of them from a
spinner or a successful `AudioContext.resume()` call.

## Qualification sequence

1. Capture, echo cancellation, playback cancellation, and ownership tests.
2. Typed acoustic and semantic turn events through the product IPC boundary.
3. Silence compaction with reversible mapping and property tests.
4. A consented corpus covering interruptions, hesitations, noise, and accents.
5. One-shot versus incremental-prefill equivalence and latency measurements.
6. Streaming synthesis and bounded playback.
7. Packaged macOS calls at desktop and compact widths, followed by human review.

Useful upstream references:

- [Gemma audio input](https://ai.google.dev/gemma/docs/capabilities/audio)
- [NVIDIA NeMo end-of-utterance model](https://huggingface.co/nvidia/parakeet_realtime_eou_120m-v1)
- [SmartTurn](https://github.com/pipecat-ai/smart-turn)
- [Silero VAD](https://github.com/snakers4/silero-vad)
- [Apple Voice Processing I/O](https://developer.apple.com/documentation/audiotoolbox/kaudiounitsubtype_voiceprocessingio)
