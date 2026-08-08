# Module and Repository Map

Free Token Energy contains two independent service families and one desktop
consumer. They may be composed in one application, but they do not share a
request model, registry, Tauri state, permissions, or lifecycle.

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

## Speech gateway

| Crate | Owner boundary |
|---|---|
| `fte-speech-types` | Independent STT/TTS requests, events, tickets, descriptors, and backend trait. |
| `fte-speech-router` | Speech capability, privacy, model, and voice route planning. |
| `fte-speech-gateway` | Speech backend registry, dispatch, cancellation, and shutdown. |
| `fte-speech-platform` | Runtime platform discovery and the proven macOS Apple TTS backend. |
| `fte-speech-parakeet` | Resident, in-process Parakeet STT over Hugging Face-managed weights. |
| `tauri-plugin-free-token-energy-speech` | Speech-only IPC, live audio-input sinks, and speech lifecycle. |

The speech crates do not depend on the text gateway crates. The text Tauri
plugin does not depend on or authorize speech. An embedding application must
install `tauri-plugin-free-token-energy-speech` and grant
`free-token-energy-speech:default` explicitly before a webview can invoke
speech commands.

Apple TTS and Parakeet STT are executable. Windows, Linux, Android, Apple STT,
Kokoro, `parakeet.cpp`, sherpa-onnx, and resident Gemma audio transcription are
candidate or deferred lanes unless their individual runtime evidence says
otherwise. No current speech module captures a microphone; capture is a
permissioned product/UI responsibility that supplies ordered audio to the
gateway's typed input sink.

## Desktop composition

The `free-token-energy` desktop package under `src-tauri/` installs both Tauri
plugins because it exercises both services. This composition does not merge
their state:

```text
Free Token Energy desktop
├── tauri-plugin-free-token-energy
│   └── model gateway + optional authenticated loopback
└── tauri-plugin-free-token-energy-speech
    └── Apple TTS + Parakeet STT
```

Other applications install only what they use. In particular, a local chat
application does not acquire speech commands merely because it embeds the text
gateway.

## External repository direction

- `llama-native-kit` owns the in-process llama.cpp runtime. It must not own
  general STT/TTS routing or platform speech adapters.
- Free Token Energy owns provider and protocol composition. Its generic speech
  crate family is deliberately dependency-independent and can be extracted to
  a separately versioned `speech-native-kit` without changing its architecture.
- Product applications own conversation state, microphone permissions,
  playback UX, and the decision to install either plugin.

`scripts/check-module-boundaries.sh` enforces the two Tauri-plugin dependency
and permission boundaries in CI.
