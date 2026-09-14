#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
MINE_TARGET_DIRECTORY=$(cargo metadata --locked --no-deps --format-version 1 |
  node -e 'let s=""; process.stdin.on("data", c => s += c); process.stdin.on("end", () => process.stdout.write(JSON.parse(s).target_directory));')
pnpm --dir products/loom/apps/loom exec tauri build --debug --bundles app -- --locked
sh "$ROOT/scripts/sign-mine-development.sh" "$MINE_TARGET_DIRECTORY/debug/bundle/macos/Loom.app"
