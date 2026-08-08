# Ecosystem module boundaries

```text
llama-native-kit ───────> free-token-energy ──> mom-llama
       └───────────────────────────────────────────^

speech-native-kit ────────────────────────────────> mom-llama (only when UX ships)

mom-llama ──contracts/black-box CLI──> capability-system-compiler
```

The arrows mean “is depended on by.” Source dependencies never point back up
the graph.

| Repository | Authoritative ownership |
|---|---|
| `llama-native-kit` | in-process llama.cpp DTOs, engine, resident host and cache contracts |
| `free-token-energy` | text gateway, protocols, hosted providers and optional authenticated loopback |
| `speech-native-kit` | STT/TTS contracts, routing, local/platform backends and optional speech Tauri plugin |
| `mom-llama` | product runtime, CLI, Personas, contracts, receipts and native interface |
| `capability-system-compiler` | Loom compiler/specs and black-box acceptance |

## Speech status

STT/TTS is version-controlled in
[`delysis/speech-native-kit`](https://github.com/delysis/speech-native-kit),
not hidden in this app or bundled into the provider gateway:

- `speech-native-types`: protocol-neutral contracts;
- `speech-native-router`: privacy/capability routing;
- `speech-native-host`: execution, cancellation and shutdown;
- `speech-native-platform`: platform discovery and working Apple TTS;
- `speech-native-backend-parakeet`: resident local Parakeet STT using the Hugging Face
  cache;
- `tauri-plugin-speech-native`: the optional, narrowly permissioned Tauri IPC boundary.

Mom Llama does not yet register a speech backend or expose microphone/read-aloud
UX. Consequently it installs only the FTE text gateway plugin and grants no
speech IPC permissions. When speech UX ships, Mom Llama may consume
`speech-native-kit` directly without inheriting hosted providers or loopback
authority. Audio attachments for multimodal llama.cpp input are a separate
product feature and are not STT.

## Enforced negative boundaries

`scripts/check-architecture.sh` rejects copied native crates, undeclared speech
dependencies, source patches, sibling dependencies, and product
network/process authority outside the bounded MCP adapter.
