# Evaluation: Qwen3-TTS 0.6B on ANE

## Decision

The runtime is a credible replacement candidate for the current Metal-contending Qwen TTS path.
It produced the required consultant voice from Christine Courtois's existing reference WAV and
transcript, streamed playable PCM, ran faster than real time, and cancelled promptly. It remains an
experimental branch because voice quality needs listening review and the app adapter must be built
against the current live helper protocol after that review.

`mlx-audio-swift` is not part of this ANE path. The QwenANE target uses Foundation, Core ML, and
Accelerate only. Gloam uses `mlx-audio-swift` for its separate MLX backends; adding it here would
restore the GPU contention this experiment is intended to remove. It is still relevant upstream for
MLX TTS/STT/diarization work, but should remain outside the ANE adapter.

## Revisions and machine

- QwenANE runtime: `TinyTrashLabs/gloam-voice-studio` at
  `60e0ddaf8797e4c33043427a6c7712833f4d82d3` (MIT)
- Model: `tinytrashlabs/Qwen3-TTS-0.6B-Base-ANE` at
  `551e82a5618b4804a51559bc508fece40d63b6a7` (Apache-2.0)
- `mlx-audio-swift` inspected at `233c0d71da3dfd8d83a4b51549e1ae01c50b11b8`
- Apple M4 Max, macOS 27.0.1 (26A434)
- Reference: existing local `christine_courtois` manifest entry, 24 kHz mono PCM16 plus its exact transcript

## Placement evidence

The evaluator requests `.cpuAndNeuralEngine`, which excludes GPU execution, then inspects Core ML's
compute plan for every recurring model. Every schedulable operation below was Neural
Engine-preferred; none was GPU-preferred or GPU-supported:

| Graph | Function | NE-preferred | CPU-preferred | GPU-supported |
|---|---:|---:|---:|---:|
| talker0 | decode | 1,448 | 0 | 0 |
| talker0 | prefill | 1,448 | 0 | 0 |
| talker1 | decode | 1,461 | 0 | 0 |
| talker1 | prefill | 1,459 | 0 | 0 |
| cp_ane | default | 5,037 | 0 | 0 |
| upMall | w12 | 218 | 0 | 0 |
| upMall | w20 | 218 | 0 | 0 |

A CPU-only control did not silently produce equivalent audio: `cp_ane` returned a non-finite output
at frame zero. This model set should therefore be treated as ANE-required. The host text projection,
sampler, and vocoder head intentionally use Accelerate on CPU. Reference voice preparation also
uses CPU-only Core ML; upstream documents that asking ANE to compile those one-time fp32 encoders
can hang. The published model card's broader statement that the preparation encoders run on ANE is
inconsistent with the current runtime source and was not used as evidence.

## Latency and throughput

Warm process, prepared voice cache hit, `temperature = 1`, seed 7:

| Schedule | First playable PCM | Render wall | Audio | RTF | Cancel acknowledgement |
|---|---:|---:|---:|---:|---:|
| `[4, 8, 12]` | **416 ms** | 1,262 ms | 2,070 ms | **0.609** | **17.5 ms** |
| `[1, 2, 4, 8, 12]` | 501 ms | 1,271 ms | 2,070 ms | 0.614 | 50.4 ms |

The two schedules produced the same 27 codec frames. Their WAVs measured 158.5 dB PSNR, but the
smallest initial schedule was slower on this line due to extra scheduling/vocoder overhead and the
stream trimmer's leading-silence holdback. `[4, 8, 12]` remains the default.

For `[4, 8, 12]`, prompt construction was 2.0 ms, prefill plus first decode was 124.3 ms, the
generation loop was 1,097.1 ms, vocoder busy work was 447.7 ms overlapped with generation, and the
final vocoder wait was 38.0 ms. The output is 24 kHz mono PCM16, -5.49 dBFS peak and -20.95 dBFS RMS.

The first-ever process launch took 42.8 seconds while Core ML prepared the compiled assets. A later
process loaded in 646 ms. The engine must be created during application startup and kept resident;
a call must never instantiate it. Voice preparation took 856 ms uncached and 0.75 ms from the
persona cache. Preparing every installed persona ahead of a call is practical.

## Product adapter shape

1. Keep one resident `QwenANEEngine` in the existing Swift speech helper and prepare persona voices
   into their own cache directories. Never select a generic voice when a persona is missing.
2. Warm the selected contact before exposing the call modal. The engine retains up to four voice
   prompt/vocoder states, so a small contact LRU is natural.
3. Forward `onAudio` chunks directly into the existing bounded PCM ring. Do not assemble a complete
   WAV before playback.
4. Bind the helper's request cancellation flag to Qwen's per-frame `cancelled` closure. A cancelled
   render returns no samples; already-enqueued PCM must still be invalidated by the request epoch.
5. QwenANE accepts complete text segments, not individual live tokens. Start the first short phrase
   as soon as the language model closes a safe prosodic boundary, then use `QwenTalkSession` to carry
   sampler, transcript, codec, and vocoder context across later phrases. This avoids the four-word
   prosody resets seen in voice notes without waiting for the whole reply.
6. Record model load, voice prep/cache, phrase close, render start, first PCM, every chunk, underrun,
   cancellation, and playback start in the app's existing latency trace.

Redacted copies of the detailed receipts are committed in `evidence/`; the ignored raw receipts remain unchanged locally. The listenable local artifact is intentionally
not committed:

`artifacts/20261016T093258Z-christine_courtois/output.wav`
