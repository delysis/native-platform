# Consultant voice evidence review

Mom's consultant references are evidence, not persona configuration. Raw clips,
source timestamps, transcripts, diarization clusters, machine observations, and
human decisions remain separately attributable. A human correction never
rewrites the observation it corrects.

## Exact Qwen3-TTS control boundary

The integrated consultant route uses the pinned Qwen3-TTS Base model in ICL
mode. It conditions on an exact reference transcript plus encoded reference
audio. The bundled public `generateStream` API passes `instruct: nil` for this
route. In the pinned talker implementation, a nonempty `instruct` branch takes
precedence over the ICL branch; it does not combine an instruction with the
reference transcript and audio codes. Passing an emotion instruction through
that internal parameter would therefore discard the clone reference.

The separately integrated VoiceDesign model accepts a natural-language voice
description through `instruct`, but it designs a voice rather than cloning a
reviewed consultant reference. The Qwen family also has a CustomVoice mode that
combines a built-in speaker ID with an instruction. Mom does not use those
built-in identities for the consultant roster. There are no checked-in emotion
tokens, style IDs, or style embeddings in Mom's current consultant route.

The pinned lower-level pipeline can also extract a 1024-dimensional speaker
embedding when the loaded weights contain the speaker encoder. Its file-output
API accepts that embedding and a natural-language `instruct` at the same time.
Mom's resident wire request exposes neither field, and the consultant corpus
contains no reviewed speaker-embedding artifact. That combination is therefore
an experimental path, not current product behavior. Qualifying it requires an
exact-model A/B sweep showing both style response and retained identity before
the resident protocol may expose a bounded instruction field. No special
emotion token vocabulary or discrete style-ID API exists in this integration.

Consequently, emotional and speaking-style labels on consultant evidence are a
reference-selection index. At synthesis time, select a human-approved clean
segment whose performed style matches the desired style. The resulting Base ICL
markup is:

```json
{
  "ref_audio": "reviewed-segment.wav",
  "ref_text": "exact reviewed transcript",
  "instruct": null,
  "style_control": "implicit_from_reviewed_reference_performance"
}
```

Do not synthesize an `instruct` string from corpus labels for Base ICL unless a
future pinned model and helper explicitly support simultaneous ICL and
instruction conditioning and a real-model comparison proves that speaker
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

The optional post-diarization emotion pass is documented in
`EMOTION_DIARIZATION_ARCHIVE.md`. It binds raw uncalibrated model scores to the
exact acoustic `segment_id` and creates a separate human review artifact. It
does not modify or verify the segment's speaker identity.

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

Resolve an exported review file into the Base ICL reference index:

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
6. Resolve accepted decisions, then run the Qwen Base ICL A/B path against the
   prior reference. Human listening decides whether the reference is promoted.
