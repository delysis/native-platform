# Emotional annotation after speaker diarization

Mom treats emotional annotation as a second, source-bound evidence pass after
acoustic speaker diarization. It is not speaker identification. Every emotion
observation binds to the exact `segment_id` created by the immutable speaker
ledger and carries that segment's source hash, timestamps, anonymous speaker
label, aligned transcript hashes, overlap state, and speaker uncertainty.

The implementation is
`products/mom/scripts/emotion-diarization-ledger.mjs`. Its default inference
contract uses `emotion2vec/emotion2vec_plus_large` through a resident local
FunASR worker. The worker is explicitly offline; model acquisition and Python
environment creation happen beforehand. One model load serves every segment in
a source. The JavaScript orchestration extracts exact 16 kHz mono segment
boundaries, records the installed FunASR version, hashes the complete local
model snapshot, and preserves the model's raw labels, raw scores, and raw result.

## Model choice

The default is an evidence-backed practical choice, not a claim that emotion is
objectively recoverable from speech:

- The official [emotion2vec+ large model card](https://huggingface.co/emotion2vec/emotion2vec_plus_large)
  specifies a roughly 300M-parameter model trained with 42,526 hours of selected
  pseudo-labeled speech, a 16 kHz input, and nine output categories: angry,
  disgusted, fearful, happy, neutral, other, sad, surprised, and unknown. The
  official repository recommends the post-June-2024 checkpoint after fixing an
  earlier emotion2vec+ bug. The pinned initial revision is the repository's
  post-fix June 24 snapshot, `6c303ba987b86b93193de93e34bb2b077a6bedc4`.
- The underlying [ACL 2024 emotion2vec paper](https://aclanthology.org/2024.findings-acl.931/)
  reports cross-language representation results, but it does not establish
  calibration for these nine checkpoint scores or accuracy on Mom's interview
  archive. The ledger therefore calls every score **uncalibrated**.
- [ExHuBERT](https://huggingface.co/amiriparian/ExHuBERT) is a useful future
  cross-check. It was trained over 37 emotion datasets and produces six
  arousal/valence combinations. Its public checkpoint is about 2.67 GB, requires
  custom model code, and expects a three-second 16 kHz waveform. It is not a
  drop-in categorical replacement and is not the primary archive pass.
- [SenseVoice](https://github.com/FunAudioLLM/SenseVoice) jointly predicts ASR,
  language, acoustic events, and broad emotion tokens. Its authors report strong
  emotion benchmarks and low latency, but its public interface does not provide
  the same simple, independently preserved nine-class score vector. It is better
  suited to a later independent comparison than to silently replacing the
  dedicated classifier.
- A community [MLX conversion](https://huggingface.co/mlx-community/emotion2vec-plus-large-mlx)
  and Swift consumer can reduce Apple Silicon overhead. It must first pass
  per-segment score parity against the pinned official checkpoint; conversion
  lineage alone is not a qualification receipt.

## Offline annotation

Acquire the exact model snapshot into the Hugging Face cache or another local
directory, and provision a dedicated interpreter containing FunASR. Then run:

```sh
hf download emotion2vec/emotion2vec_plus_large \
  --revision 6c303ba987b86b93193de93e34bb2b077a6bedc4

node products/mom/scripts/emotion-diarization-ledger.mjs annotate \
  --speaker-ledger /absolute/path/interview.speaker-ledger.json \
  --media /absolute/path/interview.m4a \
  --python /absolute/path/emotion-runtime/bin/python \
  --model /absolute/path/to/pinned/emotion2vec_plus_large/snapshot \
  --model-id emotion2vec/emotion2vec_plus_large \
  --model-revision 6c303ba987b86b93193de93e34bb2b077a6bedc4 \
  --output-dir /absolute/path/emotion-review
```

The inference subprocess receives `HF_HUB_OFFLINE=1` and
`MODELSCOPE_OFFLINE=1`; a missing local asset fails instead of fetching a
different revision. Output contains:

- `SOURCE.emotion-ledger.json`, the immutable machine-observation ledger;
- `SOURCE.emotion-review.json`, a pending human-review record bound to the
  ledger digest; and
- `SOURCE.emotion-review.html`, a local review queue that plays the exact source
  interval and exports decisions.

The default proposal gates are a top score of at least `0.55` and a top-two
margin of at least `0.10`. They are triage thresholds, not calibration. Low
scores, low margins, model `unknown`, non-specific `other`, missing output,
inference failure, overlap, and upstream speaker uncertainty remain explicit
reasons. A missing result is emitted as `status: "unknown"`; it is never replaced
with `neutral` or another convenient label.

## Human qualification

George reviews every segment in the HTML queue. The exported review requires an
explicit accept, correction, unknown, or rejection for every exact segment.
Speaker identity is never inferred or modified by this review. Resolve it with:

```sh
node products/mom/scripts/emotion-diarization-ledger.mjs resolve \
  --ledger /absolute/path/SOURCE.emotion-ledger.json \
  --review /absolute/path/emotion-review.json \
  --output /absolute/path/SOURCE.reviewed-emotion-index.json
```

The resolved index contains the immutable machine observation and the separate
human decision side by side. Only a human-reviewed segment marked
`usable_for_style_selection: true` may become a performed-style reference for
Qwen3-TTS Base ICL. The label does not become a TTS instruction: Mom's current
clone route cannot combine its reference-audio ICL branch with an `instruct`
branch without discarding the clone reference.

## Known limitations

Emotion categories are culturally and context dependent; interview speech can
contain mixed or changing affect inside one diarized segment. Utterance-level
classification collapses that variation. Music, room tone, cross-talk, acting,
recording quality, language, age, and the words being spoken can all influence
scores. The model card does not publish calibration for the nine-class head,
and Mom has not calibrated it on this archive. These observations are retrieval
suggestions for human review, never verified speaker identity, clinical state,
or ground-truth emotion.
