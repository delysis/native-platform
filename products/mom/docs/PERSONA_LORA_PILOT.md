# Per-persona Qwen3-TTS LoRA pilot

This path produces one small persona adapter for the single resident Qwen3-TTS
Base model. It does not produce or ship a copied full model. Python, PyTorch,
CUDA, PEFT, and the trainer are offline build tools only; none are part of the
Mom application runtime.

## Admission gate

Training must not consume the current caption/ASR corpus directly. The checked-in
`full-transcripts/manifest.json` says its raw cues are unattributed unless a
separate turn overlay applies, and most entries explicitly say acoustic
diarization was not performed. The app's Parakeet route also declares
diarization unsupported. `voice-clone-ab.mjs` verifies a selected evaluation
clip, transcript, hash, and turn record, but it does not build or acoustically
diarize a training corpus.

In this context “OCR” can only reasonably refer to the broader extraction work
that produced retained captions and Whisper drafts. OCR itself is an image/text
transform and is not part of the podcast audio path in this repository. The
offline `diarize-archive-source.mjs` and `speaker-diarization-ledger.mjs` tools
now produce acoustic speaker clusters and review records, but those machine
clusters do not establish a named speaker or verify a transcript. The current
corpus has no admitted, source-disjoint training split.

Before training, an external/offline corpus builder must emit an immutable
admission manifest. Every source, normalized audio file, VAD segment, transcript,
diarization result, target-speaker identity decision, and human review must have
its own SHA-256 identity and tool/config revision. A segment is admitted only
when the persona identity and transcript are verified and the interval contains
one speaker. Interviewer speech, overlap, interjections, music, clipping,
ambiguity, and boundary contamination are rejected. Rejected intervals and the
reason/evidence remain in the manifest. Train, validation, and test splits are
separated by source appearance. The manifest also binds a documented voice-rights
record when one is available. This personal-project pipeline does not require
separate rights paperwork to run; speaker identity, transcript, and acoustic
purity evidence remain mandatory.

## Pinned pilot

The pilot uses:

- Qwen upstream `0c6a7cbb6c8421a46332f8c2434c7825c4c855ef`;
- Base model `Qwen/Qwen3-TTS-12Hz-1.7B-Base` revision
  `fd4b254389122332181a7c3db7f27e918eec64e3`;
- LoRA companion `079db4ba844037f970b488dc96b32e5c7490642f`;
- rank 64, alpha 32, zero LoRA dropout, PEFT's default random A / zero B
  initialization, AdamW weight decay 0.01, and constant learning rate;
- all attention and MLP projection matrices in the talker and code predictor:
  `q_proj,k_proj,v_proj,o_proj,gate_proj,up_proj,down_proj`.

Thinking Machines Lab's *LoRA Without Regret* reports that attention-only LoRA
underperforms MLP or MLP+attention, that lower ranks can exhaust capacity in
longer supervised runs, and that the `alpha / rank` parameterization makes the
best learning rate approximately rank-independent. Their standard recipe uses
alpha 32, random A with scale `1 / sqrt(d_in)`, zero B, and one learning rate for
both matrices. They also found LoRA's best learning rate about 10x FullFT in
their language-model experiments. That ratio is a sweep prior, not a qualified
Qwen3-TTS value: the TTS companion reports that 2e-5 produced noise and EOS
failures in its run. This first executable pilot therefore starts at the
companion's 2e-6 and must be followed by a bounded learning-rate and rank sweep
against held-out speech.

On a Linux NVIDIA training host, after the admission gate has produced 24 kHz
mono speaker-pure data and source-disjoint train/validation/test JSONL files:

```sh
git clone https://github.com/QwenLM/Qwen3-TTS.git Qwen3-TTS
git -C Qwen3-TTS checkout 0c6a7cbb6c8421a46332f8c2434c7825c4c855ef

git clone https://github.com/instavar/qwen3-tts-lora-finetuning.git qwen3-tts-lora
git -C qwen3-tts-lora checkout 079db4ba844037f970b488dc96b32e5c7490642f
QWEN_DIR="$PWD/Qwen3-TTS" bash qwen3-tts-lora/scripts/apply_patches.sh

git clone https://github.com/instavar/instavar-voice-evaluation.git instavar-voice-evaluation
git -C instavar-voice-evaluation checkout 29c38cfd86b889abc8b79df063c817dd8f684903

huggingface-cli download Qwen/Qwen3-TTS-12Hz-1.7B-Base \
  --revision fd4b254389122332181a7c3db7f27e918eec64e3 \
  --local-dir "$PWD/base-1.7b"

QWEN_DIR="$PWD/Qwen3-TTS" \
INIT_MODEL_PATH="$PWD/base-1.7b" \
TRAIN_JSONL="$PWD/admitted/authorized-pilot.train.with-codes.jsonl" \
VAL_JSONL="$PWD/admitted/authorized-pilot.validation.with-codes.jsonl" \
TEST_JSONL="$PWD/admitted/authorized-pilot.test.with-codes.jsonl" \
OUTPUT_DIR="$PWD/output/authorized-pilot-r64-lr2e-6" \
SPEAKER_NAME="authorized-pilot" \
AUDIT_CORPUS=1 \
INSTAVAR_VOICE_EVAL_DIR="$PWD/instavar-voice-evaluation" \
CORPUS_GROUP_FIELD="source_id" \
LORA_RANK=64 \
LORA_ALPHA=32 \
LORA_DROPOUT=0 \
LORA_BIAS=none \
LORA_TARGET_MODULES="q_proj,k_proj,v_proj,o_proj,gate_proj,up_proj,down_proj" \
LR=2e-6 \
EPOCHS=10 \
BATCH_SIZE=4 \
GRAD_ACCUM_STEPS=4 \
MIXED_PRECISION=bf16 \
bash qwen3-tts-lora/scripts/run_lora_train.sh
```

The pinned companion trainer does not expose a training seed or bind its
shuffled dataloader order into a receipt. Add and test that before treating a
run as reproducible or packaging its `config_sha256`; the command above is an
exact pilot execution, not yet a reproducible promotion recipe.

Do not select the last checkpoint automatically. Compare every epoch to the
unadapted resident Base route with frozen short, long, emotional, numerical, and
out-of-domain prompts. Record validation/test loss, ASR intelligibility,
speaker similarity as a supporting metric, blind identity/naturalness ratings,
EOS failures, interviewer leakage, time to first PCM, real-time factor, and
memory. Repeat at rank 16 and 128 and at a bounded learning-rate sweep before
choosing a production adapter.

## Runtime package

`PersonaAdapterManifest` binds the base model revision and exact weights hash,
all PEFT tensor names and shapes, rank, alpha, evaluated inference scale,
speaker embedding, admitted-dataset digest, optional rights-record digest, trainer
revision, and training-config digest. `PersonaAdapterBank` verifies those
values and both safetensors files before retaining MLX arrays in memory. The
adapter-aware native MLX projection applies
`W(x) + inference_scale * alpha / rank * B(A(x))`; its controller binds the
manifest's exact talker and code-predictor paths, rejects unresolved or unequal
module sets, selects one resident persona at a time, and clears generation
caches on every switch. The remaining integration step is to load a configured
adapter package root when the resident helper starts and carry `persona_id`
over the serial Rust-to-Swift request protocol. Until admitted data has produced
a validated package, the shipped resident request path continues to use Base
speaker-embedding conditioning with reference-code decoder context.

Primary guidance: [Thinking Machines Lab, “LoRA Without Regret”](https://thinkingmachines.ai/blog/lora/).
The Qwen-specific trainer is the pinned community companion above and remains
an implementation candidate rather than primary model-author guidance.
