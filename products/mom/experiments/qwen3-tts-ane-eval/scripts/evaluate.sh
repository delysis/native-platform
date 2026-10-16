#!/bin/zsh
set -euo pipefail

persona_id=""
text="The nervous system learns safety through repeated experiences of connection and choice."
cpu_control_frames=0
chunk_frames="4,8,12"
while (( $# > 0 )); do
  case "$1" in
    --persona-id) persona_id="$2"; shift 2 ;;
    --text) text="$2"; shift 2 ;;
    --cpu-control-frames) cpu_control_frames="$2"; shift 2 ;;
    --chunk-frames) chunk_frames="$2"; shift 2 ;;
    *) print -u2 "unknown argument: $1"; exit 64 ;;
  esac
done
[[ -n "$persona_id" ]] || { print -u2 "--persona-id is required"; exit 64; }

support_root="${MOM_SUPPORT_ROOT:-$HOME/Library/Application Support/com.delysis.llama-native-kit.mom-llama}"
models="${QWEN_ANE_MODELS:-$support_root/speech-models/qwen3-tts-0.6b-base-ane}"
voices="$support_root/speech-voices/qwen-icl"
manifest="$voices/manifest.json"
[[ -f "$manifest" ]] || { print -u2 "missing consultant voice manifest: $manifest"; exit 66; }
[[ -d "$models/coreml/talker0.mlmodelc" ]] || { print -u2 "missing ANE model set: $models"; exit 66; }

entry="$(jq -cer --arg id "$persona_id" '.entries[] | select(.persona_id == $id and .status == "collected")' "$manifest")" || {
  print -u2 "no collected reference voice for persona: $persona_id"
  exit 66
}
reference="$voices/$(jq -r '.clip_path' <<<"$entry")"
transcript="$(jq -r '.reference_transcript' <<<"$entry")"
[[ -f "$reference" && -n "$transcript" ]] || { print -u2 "the persona reference or transcript is unavailable"; exit 66; }

root="${0:A:h:h}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
artifacts="$root/artifacts/$stamp-$persona_id"
mkdir -p "$artifacts"

swift run --package-path "$root" -c release qwen3-tts-ane-eval \
  --models "$models" \
  --reference "$reference" \
  --transcript "$transcript" \
  --text "$text" \
  --cache "$support_root/speech-voices/qwen-ane-prepared/$persona_id" \
  --output "$artifacts/output.wav" \
  --receipt "$artifacts/receipt.json" \
  --cpu-control-frames "$cpu_control_frames" \
  --chunk-frames "$chunk_frames" \
  2> >(tee "$artifacts/events.ndjson" >&2) | tee "$artifacts/stdout.json"

print "$artifacts"
