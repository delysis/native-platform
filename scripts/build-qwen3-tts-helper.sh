#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PACKAGE="$ROOT/products/mom/apps/mom-llama/qwen3-tts-helper"
DEST="$ROOT/products/mom/apps/mom-llama/src-tauri/binaries/qwen3-tts-helper-aarch64-apple-darwin"
METAL_DEST="$ROOT/products/mom/apps/mom-llama/src-tauri/binaries/mlx.metallib-aarch64-apple-darwin"
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo "Qwen3 TTS helper currently supports Apple Silicon macOS" >&2
  exit 1
fi
swift build --package-path "$PACKAGE" -c release --disable-sandbox
cp "$PACKAGE/.build/release/Qwen3TTSHelper" "$DEST"
cp "$PACKAGE/.build/release/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib" "$METAL_DEST"
chmod 755 "$DEST"
