#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "$0")/.." && pwd)
acceptance_dir=$(mktemp -d /tmp/mama-llama-persona-ux.XXXXXX)
trap 'rm -rf "$acceptance_dir"' EXIT

export LLAMA_NATIVE_KIT_DATA_DIR="$acceptance_dir/data"
export LLAMA_NATIVE_KIT_STORE_KEY_HEX=000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f

cd "$repo_dir"

catalog_hash=$(shasum -a 256 crates/mom-llama-runtime/assets/therapy_consult_personas.yaml | awk '{print $1}')
test "$catalog_hash" = "09557b34bc9c85108ab7f901ca419fa50eb60283fb79519b6eeecccf8213ea64"

cargo run -q -p mom-llama-cli -- persona list --json > "$acceptance_dir/personas.json"
jq -e '
  [.result[].title] | sort == ([
    "Bessel van der Kolk",
    "Gabor Maté",
    "Peter Levine",
    "Judith Herman",
    "Richard Schwartz",
    "Janina Fisher",
    "Ad de Jongh",
    "Christine Courtois",
    "Robert Miller (Feeling-State Addiction Protocol)",
    "Arnold Popky (DeTUR)",
    "Jim Knipe",
    "Francine Shapiro",
    "Shirley Jean Schmidt (DNMS)",
    "Dolores Mosquera"
  ] | sort)
' "$acceptance_dir/personas.json" >/dev/null
jq -e 'all(.result[]; (.execution_profile.system_message | length) > 1000)' \
  "$acceptance_dir/personas.json" >/dev/null

cargo run -q -p mom-llama-cli -- persona-group list --json > "$acceptance_dir/groups.json"
jq -e '.result == []' "$acceptance_dir/groups.json" >/dev/null

cargo run -q -p mom-llama-cli -- persona instantiate \
  --persona persona-judith_herman --json > "$acceptance_dir/started.json"
jq -e '
  .status != "blocked"
  and .result.title == "Chat with Judith Herman"
  and .result.kind == "chat"
  and .result.source_conversation_id == "persona-judith_herman"
' "$acceptance_dir/started.json" >/dev/null

cargo run -q -p mom-llama-app -- --dump-html > "$acceptance_dir/app.html"
rg -q 'data-action="personas-open"' "$acceptance_dir/app.html"
rg -q 'Start a private conversation with a saved Persona.' "$acceptance_dir/app.html"
rg -q 'data-action="persona-open" data-conversation="persona-judith_herman"' \
  "$acceptance_dir/app.html"
rg -q 'data-action="persona-instantiate" data-persona="persona-judith_herman"' \
  "$acceptance_dir/app.html"
if rg -q 'data-action="skills-open"|Body &amp; trauma lens|Safety &amp; recovery stages lens' \
  "$acceptance_dir/app.html"; then
  echo "obsolete primary navigation or abstract Persona seeds remain" >&2
  exit 1
fi
if rg -q 'Edits version this template|class="persona-template-banner"' \
  "$acceptance_dir/app.html"; then
  echo "internal Persona-template state leaked into the normal chat surface" >&2
  exit 1
fi

rg -q 'captureChatViewport' apps/mom-llama/ui/coop-hx.js
rg -q 'restoreChatViewport' apps/mom-llama/ui/coop-hx.js
rg -q 'stream.dataset.followTail' apps/mom-llama/ui/coop-hx.js
composer_clear_line=$(rg -n -F 'if (textarea) textarea.value = "";' apps/mom-llama/ui/coop-hx.js | head -n 1 | cut -d: -f1)
dispatch_line=$(rg -n -F 'result = await invoke("mom_llama_chat_dispatch"' apps/mom-llama/ui/coop-hx.js | head -n 1 | cut -d: -f1)
test "$composer_clear_line" -lt "$dispatch_line"
rg -q -F 'if (!message && !attachmentIds.length) return;' apps/mom-llama/ui/coop-hx.js

echo "persona product UX ok: exact 14-person catalog, zero seeded groups, editable template flow, explicit start flow, attachment-only dispatch, immediate composer clear, stable transcript viewport"
