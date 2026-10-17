# Consultant voice evidence review

Mom's consultant references are evidence, not persona configuration. Raw clips,
source timestamps, transcripts, diarization clusters, machine observations, and
human decisions remain separately attributable. A human correction never
rewrites the observation it corrects.

## Exact Qwen3-TTS control boundary

The resident consultant route uses the pinned Qwen3-TTS Base model with a
speaker embedding extracted from the reference recording. Encoded reference
audio also supplies context to the rolling decoder windows. Although its wire
request includes `ref_text`, the resident helper currently passes
`referenceTranscript: nil` to `generateStream`; the transcript is **not** used
to condition live synthesis. Its separate file-output route can use Base ICL
conditioning with a nonempty reference transcript and encoded audio. In the
pinned talker, a nonempty `instruct` takes precedence over that ICL branch.
Do not describe an offline ICL audition as a test of the app's live voice.

The separately integrated VoiceDesign model accepts a natural-language voice
description through `instruct`, but it designs a voice rather than cloning a
reviewed consultant reference. The Qwen family also has a CustomVoice mode that
combines a built-in speaker ID with an instruction. Mom does not use those
built-in identities for the consultant roster. There are no checked-in emotion
tokens, style IDs, or style embeddings in Mom's current consultant route.

The pinned pipeline extracts a 1024-dimensional speaker embedding for the
resident route, but its wire request does not expose a style instruction or a
reviewed, separately stored speaker-embedding artifact. Combining an embedding
with a natural-language instruction is an experimental path. Qualifying it
requires an exact-model A/B sweep showing both style response and retained
identity before the resident protocol may expose a bounded instruction field.
No special emotion token vocabulary or discrete style-ID API exists in this
integration.

Consequently, emotional and speaking-style labels on consultant evidence are a
reference-selection index. At synthesis time, select a human-approved clean
segment whose performed style matches the desired style. The corpus index may
retain the exact transcript as evidence, even though the live resident route
does not currently consume it:

```json
{
  "ref_audio": "reviewed-segment.wav",
  "ref_text": "exact reviewed transcript",
  "instruct": null,
  "style_control": "implicit_from_reviewed_reference_performance"
}
```

Do not synthesize an `instruct` string from corpus labels for live synthesis
without a pinned helper path and real-model comparison proving that speaker
identity is retained.

## Evidence manifest

The ignored local segment manifest has `schema_version: 1`, exact turn-selection
provenance, and a `segments` array. Model diarization records the exact model,
revision, and runtime. A caption-bounded manual turn records its method and
version instead; it must not masquerade as a model result. Each segment contains:

- source URL, title, channel, and start/end seconds;
- immutable clip hash and audio facts;
- diarization or manual-turn cluster ID and confidence, including `null` when
  unknown;
- source-attributed or machine-proposed speaker identity with basis and
  confidence;
- exact transcript, transcript hash, and ASR model/version/runtime provenance;
- a style observation whose status is `unknown` or `machine_proposed`.

Machine-proposed emotion or speaking-style labels require their own exact model
provenance and confidence. No label is inferred from a video title, persona
description, clinical specialty, or the words in the transcript.

## Human queue

Build an immutable review queue and local browser:

```sh
node products/mom/scripts/voice-corpus-review.mjs build \
  --manifest /absolute/path/to/segments.json \
  --output-dir /absolute/path/to/review
open /absolute/path/to/review/review-queue.html
```

The page plays each source-bound clip and lets George accept, correct, or reject
the speaker identity and emotion/style labels. Blank style fields remain
explicitly `unknown`. Exported reviews are a new JSON artifact; they do not edit
the queue or source manifest.

Resolve an exported review file into the reviewed reference index:

```sh
node products/mom/scripts/voice-corpus-review.mjs resolve \
  --queue /absolute/path/to/review/review-queue.json \
  --reviews /absolute/path/to/voice-reviews.json \
  --output /absolute/path/to/accepted-reference-index.json
```

The resolver records hashes of both inputs and carries the complete decisions
alongside accepted references. Re-review changed evidence by building a new
queue rather than overwriting a previous decision record.

## Fast qualification order

1. Preserve the longest clean, continuous exact-speaker master, normally 30-60
   seconds. Derive shorter engine-specific clips from that master.
2. Run `voice-sample-qc.mjs` to catch clipping, low estimated SNR, low speech
   occupancy, and nonpreferred recording context.
3. Transcribe the exact retained boundary and record ASR provenance. Keep any
   uncertainty rather than correcting silently.
4. Diarize the source boundary and retain cluster ID, confidence, model revision,
   and receipt hash. Until that is done, record a manual caption-bounded turn
   with confidence `null`; never fabricate model provenance. A cluster is not a
   person.
5. Build the queue and have George listen to every proposed identity and label.
6. Resolve accepted decisions, then run an A/B through the resident streaming
   path against the prior reference. Human listening decides whether the
   reference is promoted. Offline ICL results are a separate experiment.

## 2026-10-17 resident audition decision

Jim Knipe, Bessel van der Kolk, and Gabor Maté were presented together in a
level-matched resident-streaming A/B against longer candidate references. The
reviewer's answer was exactly **“Current sounds better”**. This was one answer
to a bundled question, so it does not establish separate ratings or reasons
for each person. Keep the current installed references for this set; none of
those three longer candidates is promoted. The ignored local review record and
the per-voice render receipts are under
`artifacts/voice-samples/candidates/round4/auditions/`.

The reviewer then heard a second A/B of 14–16-second excerpts from those same
candidate sources. Their bundled answer was exactly **“Current still sounds
better”**. Keep the current references for all three; do not promote the
shortened candidates either. The local receipt is
`artifacts/voice-samples/candidates/round4/auditions/review-short-2026-10-17.json`.
This rejects the tested samples, not every possible source recording or future
voice model. A duration change alone has not yielded a preferred voice.
The existing quality review already marks Bessel and Gabor as `keep`; stop
spending audition time on replacements for them. Jim's current recording is
still marked `replace` for compression, but both rejected excerpts came from
the same teaching-video source. His next candidate should come from a
different, verified continuous-speaker recording, not another cut of that
source.
