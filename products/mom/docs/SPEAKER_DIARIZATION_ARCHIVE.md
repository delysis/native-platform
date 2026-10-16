# Speaker diarization archive boundary

`products/mom/scripts/speaker-diarization-ledger.mjs` is the durable boundary
between audio/caption ingestion, an acoustic diarizer, and human speaker
identity review. It deliberately refuses caption speaker labels in the
diarization slot. Captions and OCR/ASR text remain transcript evidence; they do
not prove who spoke.

The builder consumes a content-addressed source record, transcript segments,
and acoustic diarizer segments. Raw diarizer cluster names are replaced with
deterministic, source-local `speaker_1`, `speaker_2`, and so on, ordered by first
appearance. Every output interval retains the transcript segments it overlaps,
the transcript text hashes, model and configuration provenance, confidence,
overlap, and uncertainty reasons. The ledger itself has a canonical SHA-256
identity.

Normal archive ingestion also emits a `speaker-attributed-transcript.json`
artifact. It preserves each original OCR/ASR/caption segment and text hash while
attaching the acoustically supported anonymous speaker ID. A transcript segment
that crosses a speaker boundary retains every candidate speaker and is marked
uncertain instead of being assigned by guesswork. Human identity review can
therefore happen later without rerunning OCR, ASR, or acoustic clustering.

Human review is a separate record bound to that exact ledger digest. Every
anonymous speaker must be explicitly bound to a Persona, rejected, or merged
into another anonymous speaker. Merge cycles, stale ledgers, pending speakers,
and incomplete evidence fail closed. Applying a review marks only clean,
non-overlapping, non-uncertain intervals as admission candidates. Audio slicing,
hashing, transcript correction, and train/validation/test assignment remain
later admission steps; the ledger never silently promotes a candidate.

## Build a source ledger

The input JSON uses schema `mom-speaker-diarization-input-v1`. `transcript.path`
and `diarization.path` may point to JSON arrays or JSONL files relative to the
input. Their declared SHA-256 digests are verified before use. Diarization must
declare `kind: "acoustic_diarization"` and record its tool revision and config
digest.

```sh
node products/mom/scripts/speaker-diarization-ledger.mjs build \
  --input source-diarization-input.json \
  --output source-speaker-ledger.json \
  --review-template source-speaker-review.json
```

Fill the review template, preserving its `ledger_sha256`, then resolve it:

```sh
node products/mom/scripts/speaker-diarization-ledger.mjs review \
  --ledger source-speaker-ledger.json \
  --review source-speaker-review.json \
  --output source-speaker-reviewed.json
```

For this local personal project, the training admission schema does not require
a separate rights document. If a rights or consent record is supplied, it is
retained and content-addressed. Speaker identity still requires a human review
digest; neither OCR, captions, ASR, nor diarizer cluster assignment can admit a
voice by itself.

## Produce acoustic speaker clusters

`acoustic-speaker-diarizer.mjs` runs the local Qwen speaker encoder over each
time-aligned transcript interval, clusters the resulting acoustic embeddings,
and writes the diarization JSON consumed above. It does not trust caption
speaker names. A singleton, a weak cluster, or a segment too close to another
speaker cluster is retained with `uncertain: true` for human review.

```sh
node products/mom/scripts/acoustic-speaker-diarizer.mjs \
  --media interview.m4a \
  --transcript interview-segments.jsonl \
  --helper target/debug/qwen3-tts-helper \
  --model "$HOME/Library/Caches/delysis/voice-ab/models/qwen3-tts-1.7b-base-bf16" \
  --output interview-diarization.json \
  --max-speakers 2
```

`--max-speakers` is optional. It is useful when the source is known to be a
two-person interview. Without it, clustering stops at the configured cosine
threshold (`--threshold`, default `0.78`). The runner prints the output digest;
put that exact digest in the ledger input as `diarization.artifact_sha256`.

For normal archive ingestion, use the composed command below. It performs the
acoustic pass, constructs the content-addressed ledger, and emits the bound
human-review template in one operation. This is the archive pipeline entry
point; running the two lower-level commands by hand is only useful for repair
or inspection.

```sh
node products/mom/scripts/diarize-archive-source.mjs \
  --media interview.m4a \
  --transcript interview-segments.jsonl \
  --source-id interview-1 \
  --source-url https://example.test/interview-1 \
  --transcript-tool-id local-whisper \
  --transcript-tool-revision model-and-config-digest \
  --helper target/debug/qwen3-tts-helper \
  --model "$HOME/Library/Caches/delysis/voice-ab/models/qwen3-tts-1.7b-base-bf16" \
  --output-dir speaker-ledgers \
  --max-speakers 2
```

Rolling captions often contain overlapping duplicate cues and corrections only
a few milliseconds long. The pipeline unions their time ranges and creates
bounded three-second acoustic windows before embedding. Text remains unchanged
as transcript evidence and is aligned back onto the acoustic windows in the
ledger. Silence remains a boundary. This prevents caption formatting from
becoming accidental speaker segmentation.
