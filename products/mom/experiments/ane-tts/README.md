# ANE TTS experiment

This experiment keeps ANE and alternate-TTS work outside the live voice-call path.
It records what ran on the development M4 Max, what merely fell back, and the next
bounded proof required before any product integration.

## Result

Audio8 through Core AI is a runnable GPU alternative. It is not yet an ANE result.
The checked Core AI export accepts a Neural Engine preference, but macOS reports
ANE-incompatible operations and an explicit `ANECCompile() FAILED`, then executes
through the remaining allowed compute units. Its resulting timing must therefore
be treated as fallback timing.

Espresso is a plausible direct-ANE home for transformer portions of a codec TTS:
Q/K/V and SwiGLU projections match the operations it already supports for Qwen.
It is not a model converter, has no TTS importer, and does not implement the
convolutional/transpose-convolution vocoder. Its own six-layer hardware probe also
fails to compile on this macOS 27 M4 Max with `verifyBundleAtPath: invalid model`.

The useful proof boundary is therefore:

1. Restore Espresso's own fallback-disabled ANE fixture on macOS 27.
2. Port and parity-check one Audio8/Qwen transformer frame with fallback disabled.
3. Measure compile-once cost, frame latency, and power while the existing Gemma Metal
   host is active.
4. Only then consider moving the full autoregressive talker/code predictor. Leave the
   codec decoder on CPU/GPU until it has its own parity and latency proof.

## Speculative codec decoding

The cited multi-token paper reports a 4–5× reduction in token prediction time by
adding several prediction heads and selecting a sequence with Viterbi decoding. It
does not require additional training in the paper's setup, but it does require a
model-specific multi-head implementation and quality gate; it is not a scheduler flag
that can be applied to the current Qwen helper.

Apple's coarse-grained acceptance work instead uses a fast draft model and a larger
target model, accepting acoustically interchangeable codec tokens by groups derived
from the target embedding space. That is promising after a small draft exists. It does
not remove the immediate need for a correct, resident target pipeline, and running both
models on the same Metal device could increase the contention this experiment is meant
to remove. A useful later proof would put the draft or target transformer on ANE and
measure accepted frames per joule with the PCM deadline scheduler active.

Audio8 already demonstrates one related, practical optimization: its graph fuses the
slow autoregressive step, sampling, and nine fast-codebook predictions into one engine
call per 46 ms audio frame. That cuts dispatch overhead without speculative acceptance,
but its 32-frame streaming window still delays first audio to roughly 1.5 seconds here.

## Candidate comparison

| Candidate | First audio | Sustained speed | Streaming | Persona fidelity | Footprint / power | Integration state |
| --- | ---: | ---: | --- | --- | --- | --- |
| Resident Qwen MLX | 126–128 ms median in the focused sweep | worst measured RTF 0.690 | PCM chunks | Current reference-audio cloning and Persona path | Shares Metal with Gemma; power not yet measured | Product-integrated; correctness work remains |
| Chatterbox Nano CPU | 499 ms for four words; 1.388 s for 24 words | median RTF 0.386–0.216 | Current implementation emits only a finished wave | Zero-shot reference cloning; not yet qualified on the Persona set | 7.13 GB measured peak RSS; power not yet measured | Practical contention-free experiment; needs incremental decode |
| Audio8 Core AI GPU | 1.519 s warm median | RTF 0.985 warm median | Exact 32-frame chunks, about 1.486 s each | Model supports transcript-conditioned zero-shot cloning; local fidelity not yet checked | Model card reports 0.85–0.99 GB; power not locally measured | Apache-2.0 preview model on an unmerged runtime PR |
| Audio8 Core AI “ANE preference” | No valid ANE measurement | No valid ANE measurement | Same host API | Same model | ANE compile failed and fell back | Requires graph surgery and fallback-disabled placement proof |
| Espresso direct ANE | No inference reached | No measurement | No TTS host | No TTS voice path | No measurement | MIT research runtime; macOS 27 kernel compile currently fails |

The latency values in this table come from `receipt.json` and the two existing Mom
latency receipts it names. The Audio8 memory figure is a publisher claim rather than a
local peak-RSS measurement. None of these runs measured package power, so no
energy-efficiency conclusion is justified yet.

## Reproduce the Audio8 GPU probe

The model root needs these Hugging Face files from
`coreai-community/Audio8-TTS-Preview-0.6b-CoreAI` revision
`91a97eb275335bf44f3eec74637af09363a75705`:

- `audio8_dualar_int8_cl2048_w32.aimodel/` (876,260,194 bytes with metadata)
- `audio8_codec_decoder_fp16_t160.aimodel/` (261,101,337 bytes with metadata)
- `tokenizer/` (13,081,444 bytes)

The 416 MB codec encoder is needed only to register new voice references. It was not
downloaded for this latency probe.

```sh
cd products/mom/experiments/ane-tts/audio8-probe
AUDIO8_ROOT=/path/to/Audio8-TTS-Preview-0.6b-CoreAI \
  AUDIO8_COMPUTE=gpu AUDIO8_MAX_FRAMES=96 \
  swift run -c release mom-audio8-probe
```

`AUDIO8_COMPUTE=ane` is deliberately exposed as a diagnostic. A successful command is
not evidence of ANE execution because Core AI treats the selection as a preference and
can fall back. Inspect compiler diagnostics and require a fallback-disabled proof before
reporting an ANE result.

## Reproduce the Espresso probe

At Espresso revision `05ea6a3da91be642abd3d4741c0341a941ce1064`:

```sh
swift run espresso-bench --ane-only --inference --layers 6
```

On this host it failed five compile attempts with
`com.apple.appleneuralengine Code=10: verifyBundleAtPath: invalid model`.
That is a compatibility blocker before a TTS port can produce meaningful measurements.

## Evidence

See `receipt.json` for measured runs,
source revisions, model sizes, and the comparison with the resident Qwen and CPU
Chatterbox Nano receipts.
