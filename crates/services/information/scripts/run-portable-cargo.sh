#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
"$repo_root/scripts/check-portable-packages.sh"

packages=()
while IFS= read -r package; do
  packages+=("-p" "$package")
done < "$repo_root/scripts/portable-packages.txt"

case "${1:-}" in
  test)
    exec cargo test --locked "${packages[@]}" --all-targets
    ;;
  clippy)
    exec cargo clippy --locked "${packages[@]}" --all-targets --all-features -- -D warnings
    ;;
  *)
    echo "usage: $0 test|clippy" >&2
    exit 2
    ;;
esac
