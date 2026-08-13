#!/usr/bin/env bash
set -euo pipefail

product_root="$(cd "$(dirname "$0")/.." && pwd)"
workspace_root="$(cd "$product_root/../.." && pwd)"
# shellcheck source=../native-pins.env
source "$product_root/native-pins.env"

if [[ "$NATIVE_KIT_IMPORTED_PATH" != "crates/native" ]]; then
  echo "unexpected imported native path: $NATIVE_KIT_IMPORTED_PATH" >&2
  exit 1
fi
if [[ ! "$LLAMA_CPP_RS_REV" =~ ^[0-9a-f]{40}$ ]]; then
  echo "llama-cpp-rs pin is not an exact 40-hex revision: $LLAMA_CPP_RS_REV" >&2
  exit 1
fi

manifest="$workspace_root/Cargo.toml"
lockfile="$workspace_root/Cargo.lock"

native_manifest_count=$(grep -Ec '^llama-native-(cache|engine|host|types) = \{ path = "crates/native/crates/llama-native-(cache|engine|host|types)" \}$' "$manifest")
if [[ "$native_manifest_count" -ne 12 ]]; then
  echo "Cargo.toml has $native_manifest_count canonical native path bindings, expected 12" >&2
  exit 1
fi

native_git_count=$(grep -c '^source = "git+https://github.com/delysis/llama-native-kit' "$lockfile" || true)
if [[ "$native_git_count" -ne 0 ]]; then
  echo "Cargo.lock retains $native_git_count llama-native-kit Git sources" >&2
  exit 1
fi

binding_lock_count=$(grep -Fc "?rev=$LLAMA_CPP_RS_REV#$LLAMA_CPP_RS_REV" "$lockfile")
if [[ "$binding_lock_count" -ne 2 ]]; then
  echo "Cargo.lock has $binding_lock_count coherent binding sources, expected 2" >&2
  exit 1
fi

for obsolete in \
  2d69f086e922ed7bdfd6236baf5a1ad0ed568360 \
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

echo "native paths coherent: $NATIVE_KIT_IMPORTED_PATH / llama-cpp-rs=$LLAMA_CPP_RS_REV"
