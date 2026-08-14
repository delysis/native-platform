# speech-native-kit

`speech-native-kit` is a Tauri-independent, local-first Rust host for speech
recognition and synthesis. It owns audio-domain contracts, capability routing,
resident local backends, cancellation, and lifecycle. It does **not** own an
HTTP server, provider credentials, microphone capture, audio playback, or
product conversation state.

## Crates

| Crate | Responsibility |
|---|---|
| `speech-native-types` | STT/TTS requests, audio formats, events, usage, errors, descriptors, and backend traits. |
| `speech-native-router` | Privacy and capability gates plus deterministic model and voice selection. |
| `speech-native-host` | Backend registry, dispatch, cancellation, and orderly shutdown. |
| `speech-native-platform` | Conservative platform capability discovery and the proven Apple TTS adapter. |
| `speech-native-backend-parakeet` | Resident local Parakeet STT using Hugging Face-managed weights. |

STT and TTS live together because they share audio formats, streaming and
backpressure, cancellation, platform permission semantics, voice/model
discovery, and real-audio tests. Text generation and hosted-provider routing do
not live here.

## Composition

```text
speech-native-kit ──► local STT/TTS host
         │
         ├──► product app (capture, playback, transcript UX)
         └──► optional provider gateway bridge (hosted speech and /v1/audio/*)
```

Free Token Energy may implement hosted speech backends or OpenAI-compatible
audio endpoints as an optional downstream bridge. Those adapters depend on
these crates; the speech core never depends on Free Token Energy.

There is no generic Tauri speech plugin in the lean workspace. A product that
ships speech owns its capture/playback UX, permission policy, IPC commands, and
joined shutdown at its composition root. A Rust consumer may inject and use an
`Arc<SpeechHost>` directly.

## Verification

```sh
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
./scripts/check-boundaries.sh
```

Real Apple TTS and Parakeet STT proofs remain environment-gated and must label
fixture, local-inference, network, and terminal-event evidence honestly.
