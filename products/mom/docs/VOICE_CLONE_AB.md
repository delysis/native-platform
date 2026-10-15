# Qwen consultant voice builder

`products/mom/scripts/voice-clone-ab.mjs` synthesizes the same phrase for every
consultant with Qwen3-TTS Base. It refuses a reference unless the ignored local
manifest contains an exact clip hash, transcript, and verified speaker-turn
record. A podcast title alone cannot qualify the speaker.

A full run must match every identity in Mom's checked-in persona catalog.
Historical sample slugs for DeTUR, FSAP, and DNMS are normalized to the exact
product persona IDs in outputs and receipts.

The local sample set lives under
`products/mom/artifacts/voice-samples/`. It is evaluation material, not a
shippable asset set. Product use requires a separately approved reference and
voice-rights record.

## Reference quality

Screen candidates before synthesis. The quality pass reads PCM samples
directly and reports clipping, level, duration, speech occupancy, and an
active-to-quiet frame estimate. Add `recording_context` to each manifest entry;
`in_person_podcast` and `in_person_studio_interview` are preferred over remote
calls and lecture halls. The signal estimate ranks candidates but cannot detect
every wrong speaker, overlap, codec artifact, room echo, or unsuitable delivery.
`listening-review.json`, when present, supplies the authoritative listening
verdict.

The original 14-second clips were comparison fixtures, not a target duration.
Preserve the longest clean, varied single-speaker reference that improves Qwen:
varied cadence, pitch, sentence shape, and affect are useful. The manifest may
provide a `reference_variants.qwen` entry with its own source ranges, path,
hash, duration, exact transcript, and verification. Do not impose an arbitrary
maximum. Sweep increasing durations on the local Qwen build and retain the best
quality and latency result before the model's actual context bound.

Qwen loads once and synthesizes all selected personas in one process. Outputs
and an incrementally written receipt go to the ignored
`synthesis-ab/qwen/` directory.

```sh
# Validate identities, transcripts, and hashes without loading the model.
node products/mom/scripts/voice-clone-ab.mjs --validate-only

# Rank source-sample quality and merge the listening verdicts.
node products/mom/scripts/voice-sample-qc.mjs

# Synthesize all consultant voices with Qwen3-TTS Base through mlx-audio.
node products/mom/scripts/voice-clone-ab.mjs
```

Qwen is the sole active consultant-voice engine. Earlier Dots and Zyphra
outputs remain only as historical comparison evidence; their runners and
scoring dependency have been removed. Human listening remains the acceptance
gate for intelligibility, cadence, artifacts, and resemblance.
