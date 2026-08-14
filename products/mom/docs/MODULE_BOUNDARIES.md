# Ecosystem module boundaries

```text
llama-native-kit ───────> free-token-energy ──> mom-llama
       └───────────────────────────────────────────^

speech-native-kit ────────────────────────────────> mom-llama (only when UX ships)

attachment-native-kit ────────────────────────────> mom-llama

mom-llama ──contracts/black-box CLI──> capability-system-compiler
```

The arrows mean “is depended on by.” These are package ownership boundaries
inside `delysis/native-platform`; source dependencies never point back up the
graph.

| Repository | Authoritative ownership |
|---|---|
| `llama-native-kit` | in-process llama.cpp DTOs, engine, resident host and cache contracts |
| `free-token-energy` | text gateway, protocols, hosted providers and optional authenticated loopback |
| `speech-native-kit` | STT/TTS contracts, routing, and local/platform backends |
| `attachment-native-kit` | content-first bounded inspection, recursive container graph, canonical artifacts, provenance and capability-aware media/transform planning |
| `mom-llama` | product runtime, CLI, Personas, contracts, receipts and native interface |
| `capability-system-compiler` | Loom compiler/specs and black-box acceptance |

## Speech status

STT/TTS lives under `crates/services/speech` in this workspace, not hidden in
this app or bundled into the provider gateway:

- `speech-native-types`: protocol-neutral contracts;
- `speech-native-router`: privacy/capability routing;
- `speech-native-host`: execution, cancellation and shutdown;
- `speech-native-platform`: platform discovery and working Apple TTS;
- `speech-native-backend-parakeet`: resident local Parakeet STT using the Hugging Face
  cache.

Mom Llama does not yet register a speech backend or expose microphone/read-aloud
UX. Consequently it installs only the FTE text gateway plugin and grants no
speech IPC permissions. When speech UX ships, Mom Llama may consume the speech
crates directly and own the narrow IPC/permission edge without inheriting
hosted providers or loopback authority. Audio attachments for multimodal
llama.cpp input are a separate product feature and are not STT.

## Attachment status

Attachment parsing lives under `crates/services/attachment` in this workspace.
Mom Llama supplies already-authorized bytes and owns the file picker, encrypted
records, draft/branch lifecycle and presentation. The attachment kit owns the
safe authority-free boundary: content-first detection, one monotonic recursive
budget, content-addressed provenance, bounded canonical text, exact media
validation and typed transform requests.

The core never performs OCR, transcription, video decoding, network fallback or
subprocess conversion. Mom may satisfy a typed audio-transcription request with
`speech-native-kit`, send payload-decoded image/audio bytes only when the pinned
native model advertises that exact capability, or surface a typed blocker.

## Enforced negative boundaries

`scripts/check-architecture.sh` rejects copied native/attachment crates,
undeclared speech dependencies, retired first-party Git sources in Mom's
locked graph, child-manifest path overrides, and product network/process
authority outside the bounded MCP adapter.
