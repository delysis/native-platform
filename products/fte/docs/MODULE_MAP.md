# Module and Repository Map

Free Token Energy owns the text/model provider gateway and its desktop
consumer. Local speech is an independently versioned sibling, not a workspace
member or implicit desktop capability.

## Text and model gateway

| Crate | Owner boundary |
|---|---|
| `fte-types` | Protocol-neutral generation requests, items, events, usage, errors, and policies. |
| `fte-protocols` | OpenAI and Anthropic edge codecs. |
| `fte-router` | Text/model route planning, admission, retries, deadlines, and accounting. |
| `fte-providers` | Hosted generation provider adapters. |
| `fte-store` | Response-chain state and injected secret/cache storage traits. |
| `fte-backend-llama` | The only bridge from FTE to `llama-native-kit`. |
| `fte-loopback` | Authenticated OpenAI/Anthropic-compatible REST and SSE edge. |
| `tauri-plugin-free-token-energy` | Text/model gateway IPC, loopback control, and their lifecycle only. |

## Speech sibling

[`delysis/speech-native-kit`](https://github.com/delysis/speech-native-kit)
owns STT/TTS contracts, routing, resident local backends, cancellation,
lifecycle, and its optional Tauri plugin. Its default Tauri permission is
status-only; synthesis, file transcription, and live transcription are
separate explicit grants.

The FTE desktop does not install that plugin. This keeps ONNX, platform speech
frameworks, live-audio IPC, and microphone-adjacent authority out of a desktop
that has no speech UI.

## External repository direction

- `llama-native-kit` owns the in-process llama.cpp runtime. It must not own
  general STT/TTS routing or platform speech adapters.
- Free Token Energy owns hosted-provider policy and public protocol edges. A
  future optional bridge may implement hosted speech backends and
  OpenAI-compatible `/v1/audio/*` endpoints by depending on
  `speech-native-kit`.
- Product applications own conversation state, microphone permissions,
  playback UX, and the decision to install either plugin.

`scripts/check-module-boundaries.sh` enforces that speech does not drift back
into the FTE core plugin or desktop capability surface.

The downstream adapter contract is specified in [Optional speech bridge](SPEECH_BRIDGE.md).
