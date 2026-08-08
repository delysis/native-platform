#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
model_file="${MOM_LLAMA_MODEL_PATH:-}"

if [[ -z "$model_file" || ! -f "$model_file" ]]; then
  echo "MOM_LLAMA_MODEL_PATH must point at a real local GGUF." >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for the Persona mention cache proof." >&2
  exit 2
fi

proof_root="${LLAMA_NATIVE_KIT_PERSONA_CACHE_PROOF_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/llama-native-kit-persona-cache-proof.XXXXXX")}"
mkdir -p "$proof_root"
export LLAMA_NATIVE_KIT_DATA_DIR="$proof_root"
export LLAMA_NATIVE_KIT_STORE_KEY_HEX="${LLAMA_NATIVE_KIT_STORE_KEY_HEX:-5555555555555555555555555555555555555555555555555555555555555555}"

cd "$repo_root"
cargo build -q -p mom-llama-cli
cli="$repo_root/target/debug/mom-llama-cli"

"$cli" settings update \
  --model-path "$model_file" \
  --device cpu \
  --max-tokens 8 \
  --kv-cache-policy prefixes-only \
  --json >/dev/null

source_id="$("$cli" conversation new --title "Persona cache source" --json | jq -er '.result.id')"
source_send="$("$cli" chat dispatch \
  --conversation "$source_id" \
  --message "Remember the stable Persona prefix." \
  --timeout-s 300 \
  --json 2>"$proof_root/source.stderr.log")"
source_leaf="$(jq -er '
  select(.readiness == "real_prompt_smoke_passed" and .receipt.real_engine_invoked == true)
  | .result.output.assistant_message_id
' <<<"$source_send")"

persona="$("$cli" persona freeze \
  --conversation "$source_id" \
  --message "$source_leaf" \
  --name "Restart witness" \
  --handle "restart-witness" \
  --history full \
  --json)"
persona_id="$(jq -er '.result.id' <<<"$persona")"
host_id="$("$cli" conversation new --title "Persona cache host" --json | jq -er '.result.id')"

first="$("$cli" chat dispatch \
  --conversation "$host_id" \
  --message "@restart-witness answer briefly." \
  --timeout-s 300 \
  --json 2>"$proof_root/first.stderr.log")"
first_cache_id="$(jq -er '
  select(
    .readiness == "real_prompt_smoke_passed"
    and .receipt.real_engine_invoked == true
    and .receipt.fake_fixture == false
    and .result.invocation.results[0].cache_reused == false
  )
  | .result.invocation.results[0].cache_id
' <<<"$first")"

# A new CLI process has no in-memory Persona prefix. Reuse therefore proves an
# authenticated encrypted persistent restore for the exact Persona version.
second="$("$cli" chat dispatch \
  --conversation "$host_id" \
  --message "@restart-witness answer again." \
  --timeout-s 300 \
  --json 2>"$proof_root/second.stderr.log")"
jq -e --arg cache_id "$first_cache_id" '
  .readiness == "real_prompt_smoke_passed"
  and .receipt.real_engine_invoked == true
  and .receipt.fake_fixture == false
  and .result.invocation.results[0].cache_reused == true
  and .result.invocation.results[0].cache_id == $cache_id
' <<<"$second" >/dev/null

persona_leaf="$("$cli" persona get --persona "$persona_id" --json | jq -er '.result.active_leaf_message_id')"
"$cli" message edit \
  --conversation "$persona_id" \
  --message "$persona_leaf" \
  --content "This explicit edit creates Persona version two." \
  --json >/dev/null

third="$("$cli" chat dispatch \
  --conversation "$host_id" \
  --message "@restart-witness answer after the revision." \
  --timeout-s 300 \
  --json 2>"$proof_root/third.stderr.log")"
third_cache_id="$(jq -er '
  select(
    .readiness == "real_prompt_smoke_passed"
    and .receipt.real_engine_invoked == true
    and .receipt.fake_fixture == false
    and .result.invocation.targets[0].version == 2
    and .result.invocation.results[0].cache_reused == false
  )
  | .result.invocation.results[0].cache_id
' <<<"$third")"
[[ "$third_cache_id" != "$first_cache_id" ]]

jq -n \
  --arg schema "llama_native_kit.persona_mention_cache_restart_proof.v1" \
  --arg status "passed" \
  --arg model_path "$model_file" \
  --arg data_dir "$proof_root" \
  --arg persona_id "$persona_id" \
  --arg first_cache_id "$first_cache_id" \
  --arg revised_cache_id "$third_cache_id" \
  '{
    schema: $schema,
    status: $status,
    model_path: $model_path,
    data_dir: $data_dir,
    persona_id: $persona_id,
    first_use: {cache_id: $first_cache_id, reused: false},
    fresh_process_exact_version: {cache_id: $first_cache_id, reused: true},
    revised_persona: {version: 2, cache_id: $revised_cache_id, reused: false},
    real_engine_invoked: true,
    fake_fixture: false
  }'
