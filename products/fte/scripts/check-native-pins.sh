#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=../native-pins.env
source "$repo_root/native-pins.env"

for revision in "$NATIVE_KIT_REV" "$LLAMA_CPP_RS_REV"; do
  if [[ ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
    echo "native pin is not an exact 40-hex revision: $revision" >&2
    exit 1
  fi
done

manifest="$repo_root/Cargo.toml"
lockfile="$repo_root/Cargo.lock"

native_manifest_count=$(grep -Ec '^llama-native-(cache|engine|host|types) = .*rev = "'"$NATIVE_KIT_REV"'"' "$manifest")
if [[ "$native_manifest_count" -ne 4 ]]; then
  echo "Cargo.toml has $native_manifest_count coherent native pins, expected 4" >&2
  exit 1
fi

native_lock_count=$(grep -Fc "?rev=$NATIVE_KIT_REV#$NATIVE_KIT_REV" "$lockfile")
if [[ "$native_lock_count" -ne 4 ]]; then
  echo "Cargo.lock has $native_lock_count coherent native sources, expected 4" >&2
  exit 1
fi

binding_lock_count=$(grep -Fc "?rev=$LLAMA_CPP_RS_REV#$LLAMA_CPP_RS_REV" "$lockfile")
if [[ "$binding_lock_count" -ne 2 ]]; then
  echo "Cargo.lock has $binding_lock_count coherent binding sources, expected 2" >&2
  exit 1
fi

for obsolete in \
  c61692d48b0768bb242bcecb7a80c3318fc476b4 \
  b71dfaa16c77b7069259bd15add740b80f895017 \
  d4e4eb8f4255cf84017c9fa37ec8af9396f7995a \
  6a82439ee449599f7a7e477e1150ae29efdb23d6 \
  01e48b7c1e7de39c3e5e8a67cd9efac498f8da1f
do
  if grep -Fq "$obsolete" "$manifest" "$lockfile"; then
    echo "superseded native-stack revision remains: $obsolete" >&2
    exit 1
  fi
done

echo "native pins coherent: $NATIVE_KIT_REV / $LLAMA_CPP_RS_REV"
