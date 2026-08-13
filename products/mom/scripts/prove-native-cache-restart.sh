#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
model_file="${MOM_LLAMA_MODEL_PATH:-}"

if [[ -z "$model_file" || ! -f "$model_file" ]]; then
  echo "MOM_LLAMA_MODEL_PATH must point at a real local GGUF." >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for the cache-restart proof." >&2
  exit 2
fi

proof_root="${LLAMA_NATIVE_KIT_CACHE_PROOF_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/llama-native-kit-cache-proof.XXXXXX")}"
mkdir -p "$proof_root"

export LLAMA_NATIVE_KIT_DATA_DIR="$proof_root"
export LLAMA_NATIVE_KIT_STORE_KEY_HEX="${LLAMA_NATIVE_KIT_STORE_KEY_HEX:-4444444444444444444444444444444444444444444444444444444444444444}"

cd "$repo_root"
cargo build -q -p mom-llama-cli

cli="$repo_root/target/debug/mom-llama-cli"
"$cli" settings update \
  --model-path "$model_file" \
  --device cpu \
  --max-tokens 32 \
  --kv-cache-policy prefixes-only \
  --json >/dev/null

save_result="$("$cli" kv-cache save --json 2>"$proof_root/save.stderr.log")"
cache_id="$(jq -er '
  select(
    .status == "prompt_smoke_verified"
    and .readiness == "prompt_smoke_verified"
    and .receipt.real_engine_invoked == true
    and .receipt.fake_fixture == false
  )
  | .result.id
' <<<"$save_result")"

# This is a new CLI process. Its runtime memory cache starts empty and the
# sequence state must be authenticated and loaded from the encrypted store.
restore_result="$("$cli" kv-cache restore --cache "$cache_id" --json 2>"$proof_root/restore.stderr.log")"
jq -e --arg cache_id "$cache_id" '
  .status == "prompt_smoke_verified"
  and .readiness == "prompt_smoke_verified"
  and .result.id == $cache_id
  and .receipt.real_engine_invoked == true
  and .receipt.fake_fixture == false
' <<<"$restore_result" >/dev/null

status_result="$("$cli" kv-cache status --json)"
jq -e '
  .result.status == "saved"
  and (.result.entries | length) == 1
  and .result.memory_entries == 0
  and .result.persistent_bytes > 0
  and .result.persistent_bytes <= .result.persistent_capacity_bytes
  and (.result.entries | length) <= .result.persistent_capacity_entries
' <<<"$status_result" >/dev/null

# Prove the automatic session tier, not only the explicit persona-pack command.
# Every CLI invocation is a new process, so the second send can reuse only the
# authenticated persistent checkpoint created by the first send.
"$cli" settings update \
  --kv-cache-policy automatic \
  --json >/dev/null

first_chat="$("$cli" chat send \
  --conversation cache-session-proof \
  --message "Remember the code word orchid and acknowledge briefly." \
  --json 2>"$proof_root/session-first.stderr.log")"
jq -e '
  .status == "real_prompt_smoke_passed"
  and .receipt.real_engine_invoked == true
  and .receipt.fake_fixture == false
' <<<"$first_chat" >/dev/null

session_status="$("$cli" kv-cache status --json)"
session_cache_id="$(jq -er '
  .result.entries
  | map(select(.tier == "session_persistent" and .owner_id == "cache-session-proof" and .state == "ready"))
  | first.id
' <<<"$session_status")"

second_chat="$("$cli" chat send \
  --conversation cache-session-proof \
  --message "What code word did I ask you to remember?" \
  --json 2>"$proof_root/session-second.stderr.log")"
jq -e --arg cache_id "$session_cache_id" '
  .status == "real_prompt_smoke_passed"
  and .result.cache_reused == true
  and .result.cache_id == $cache_id
  and .receipt.real_engine_invoked == true
  and .receipt.fake_fixture == false
' <<<"$second_chat" >/dev/null

final_status="$("$cli" kv-cache status --json)"
jq -e '
  .result.memory_entries == 0
  and (.result.entries | map(select(.tier == "persona_pack" and .state == "ready")) | length) == 1
  and (.result.entries | map(select(.tier == "session_persistent" and .state == "ready")) | length) == 1
  and .result.persistent_bytes <= .result.persistent_capacity_bytes
  and (.result.entries | length) <= .result.persistent_capacity_entries
' <<<"$final_status" >/dev/null

jq -n \
  --arg schema "llama_native_kit.cache_restart_proof.v1" \
  --arg status "passed" \
  --arg cache_id "$cache_id" \
  --arg session_cache_id "$session_cache_id" \
  --arg data_dir "$proof_root" \
  --arg model_path "$model_file" \
  '{
    schema: $schema,
    status: $status,
    cache_id: $cache_id,
    data_dir: $data_dir,
    model_path: $model_path,
    save: {
      readiness: "prompt_smoke_verified",
      real_engine_invoked: true,
      fake_fixture: false
    },
    fresh_process_restore: {
      readiness: "prompt_smoke_verified",
      real_engine_invoked: true,
      fake_fixture: false,
      memory_entries_after_restart: 0,
      persistent_entries: 1
    },
    automatic_session_checkpoint: {
      readiness: "real_prompt_smoke_passed",
      real_engine_invoked: true,
      fake_fixture: false,
      fresh_process_reused: true,
      cache_id: $session_cache_id
    },
    budgets: {
      memory_bytes: 268435456,
      persistent_bytes: 2147483648,
      persistent_entries: 64
    }
  }'
