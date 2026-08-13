#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
package_file="$repo_root/scripts/portable-packages.txt"
normalized_package_file="$(mktemp)"
trap 'rm -f "$normalized_package_file"' EXIT
tr -d '\r' < "$package_file" > "$normalized_package_file"

if grep -Ev '^[a-z0-9-]+$' "$normalized_package_file"; then
  echo "portable package list contains an invalid entry" >&2
  exit 1
fi

if [[ "$(sort "$normalized_package_file" | uniq -d | wc -l | tr -d ' ')" != 0 ]]; then
  echo "portable package list contains duplicates" >&2
  exit 1
fi

expected="$(
  find "$repo_root/crates" -mindepth 2 -maxdepth 2 -name Cargo.toml \
    ! -path '*/tauri-plugin-information-native/Cargo.toml' \
    -exec dirname {} \; \
    | xargs -n1 basename \
    | sort
)"
actual="$(sort "$normalized_package_file")"
if [[ "$actual" != "$expected" ]]; then
  echo "portable package list does not exactly cover the non-Tauri workspace crates" >&2
  diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
  exit 1
fi

if grep -Eiq '(^|-)tauri($|-)|gui|desktop' "$normalized_package_file"; then
  echo "portable package list includes a GUI/Tauri package" >&2
  exit 1
fi

echo "portable package boundary passed"
