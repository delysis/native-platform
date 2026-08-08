# Ecosystem module boundaries

```text
llama-native-kit ──> free-token-energy ──> mom-llama
       └──────────────────────────────────────^

mom-llama ──contracts/black-box CLI──> capability-system-compiler
```

The arrows mean “is depended on by.” Source dependencies never point back up
the graph.

| Repository | Authoritative ownership |
|---|---|
| `llama-native-kit` | in-process llama.cpp DTOs, engine, resident host and cache contracts |
| `free-token-energy` | text gateway, protocols, hosted providers, loopback and separately installable speech plugin/backends |
| `mom-llama` | product runtime, CLI, Personas, contracts, receipts and native interface |
| `capability-system-compiler` | Loom compiler/specs and black-box acceptance |

## Speech status

STT/TTS is version-controlled in Free Token Energy, not hidden in this app:

- `fte-speech-types`: protocol-neutral contracts;
- `fte-speech-router`: privacy/capability routing;
- `fte-speech-gateway`: execution, cancellation and shutdown;
- `fte-speech-platform`: platform discovery and working Apple TTS;
- `fte-speech-parakeet`: resident local Parakeet STT using the Hugging Face
  cache;
- `tauri-plugin-free-token-energy-speech`: the optional Tauri IPC boundary.

Mom Llama does not yet register a speech backend or expose microphone/read-aloud
UX. Consequently it installs only the text gateway plugin and grants no speech
IPC permissions. Audio attachments for multimodal llama.cpp input are a
separate product feature and are not STT.

## Enforced negative boundaries

`scripts/check-architecture.sh` rejects copied native crates, FTE speech crates,
source patches, sibling dependencies, and product network/process authority
outside the bounded MCP adapter.
