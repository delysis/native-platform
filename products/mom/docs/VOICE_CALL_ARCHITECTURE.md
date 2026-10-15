# Voice call architecture

Status: implementation plan. The typed Parakeet end-of-utterance event described
below is implemented; the remaining stages require their stated qualification
before the phone control is enabled.

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
3. An acoustic detector marks speech onset and pause candidates. A semantic turn
   detector decides whether a pause is an end of utterance or a hesitation.
4. A compactor omits long silence from model input while preserving a reversible
   ledger from compacted samples to original frame numbers and times.
5. The audio model consumes immutable prefix chunks during speech. On a typed end
   of utterance, it finalizes the current prefix and generates without replaying
   already-prefilled audio.
6. Speech synthesis produces bounded audio chunks. Playback begins once the first
   verified chunk is available and remains cancellable at every boundary.

Every capture, inference, and playback operation has one owner and one terminal
result. Starting a new turn cancels the prior generation and playback before it
can publish more audio or text.

## Turn detection

Parakeet Realtime EOU is the first semantic detector because it is already the
resident transcription backend. Its `[EOU]` token is now exposed as a typed
`end_of_utterance` event and never enters transcript text. Acoustic start/stop
events use a separate typed lane so a pause cannot be mistaken for semantic
completion.

Initial tuning values are hypotheses for corpus evaluation, not product
constants:

- 32 ms analysis frames;
- speech onset after three frames at probability 0.60 or greater;
- pause candidate after six frames below probability 0.35;
- semantic decisions at 200, 500, and 900 ms of pause;
- forced turn close after 1,500 ms if no semantic detector answers.

SmartTurn v3.2 is the first challenger and Silero VAD v6 the first acoustic
challenger. Selection is by recorded call-corpus results: false interruptions,
missed turn ends, time to decision, and behavior with noise, accents, short
acknowledgements, and mid-sentence pauses. No detector is selected from a demo.

## Barge-in

Microphone analysis never stops during playback. When speech onset is confirmed,
the player ducks within 100 ms p95, stops within 150 ms p95, and the active
generation receives cancellation acknowledgement within 200 ms p95. Echo-only
input must not trigger interruption. A stopped operation cannot publish a late
chunk, transcript, or completion into the next turn.

## Silence compaction

Compaction preserves 160 ms before speech and 240 ms after it. Pauses of 600 ms
or less remain exact; longer pauses retain 160 ms plus a ledger entry containing
the original frame interval and duration. The ledger is stored with the request
receipt so timestamps, interruption decisions, and failures remain auditable.
Compacted Gemma audio is capped below its 30-second input limit.

## Incremental prefill

The native model boundary needs explicit `begin`, `append`, `checkpoint`,
`finalize`, `generate`, and `cancel` operations. Prefix reuse may ship only after
an equivalence test proves that chunked and one-shot input produce the same model
state within the engine's documented numeric tolerance. Benchmark 320, 640,
1,280, and 2,560 ms chunks; choose the smallest chunk that improves end-of-turn
latency without reducing throughput or destabilizing memory.

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
