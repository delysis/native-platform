# Mom Qwen3 TTS helper

This Apple Silicon helper is the narrow native execution edge for Mom Llama's
local Qwen3-TTS provider. It is built from pinned SwiftPM inputs by
`scripts/build-qwen3-tts-helper.sh`; generated binaries and Metal libraries are
never source inputs.

The Qwen adapter source is vendored from `hamptus/mlx-swift-qwen3-tts` 0.2.0 at
`2118167c06dd93233128ccef3011188e32bebd14`. The MLX Swift dependency is pinned
by `Package.resolved` to 0.30.3 at
`4dccaeda1d83cf8697f235d2786c2d72ad4bb925`.
