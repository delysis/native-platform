# Shared desktop speech composition and response validation

Base comparison: `1cee70d53605ba3bfba6a479e15eda308066ad21`; follows lifecycle commit `b86b869`.

## What is best in each implementation

| Class | Mom | Loom | Replacement or retained behavior |
| --- | --- | --- | --- |
| Backend ownership | `src-tauri/src/speech.rs::MomSpeech::discover` creates one application-owned SpeechHost with Parakeet and optional Apple synthesis. AppRuntime drains it on shutdown. | `speech_input.rs` caches a lazy transcription host, but `audio_io.rs::apple::synthesize` separately discovers and tears down another host for every read-aloud request. | New `desktop-speech::discover_local_host` builds the same local backend inventory for both products. Loom read-aloud now uses the existing SpeechInputService host. Synthesis no longer owns or closes a per-request host. |
| Lazy initialization versus close | Mom construction occurs before the composed application root is usable. | Lazy transcription initialization could happen independently of shutdown's `host.get()` observation. Sharing that host with synthesis makes the lifecycle edge especially important. | Loom serializes lazy discovery with shutdown and permanently closes lazy-host admission. A waiting initializer cannot create a host after shutdown observed none. A deterministic test drives both futures while holding their shared lifecycle guard. |
| Synthesis result identity | Mom verifies request ID, exact backend, backend kind, never-network response, no model, and exact selected voice before publication. It rejects streamed, empty, wrong-format, and oversized outputs. | Loom's prior synthesis branch accepted complete output labeled WAV below its size cap without checking the returned request and route. | `desktop-speech::complete_apple_wav` is used by both. Mom retains exact selected-voice checking; Loom retains native automatic voice selection. Both now validate returned request/backend/kind/network/model and bounds. Original bytes and backend-reported duration are preserved. |
| Transcription result identity | Mom checks exact request, Parakeet backend/model, never-network route, embedded-model kind, no voice, and native-inference evidence; its product provenance also binds source artifact hashes and resident descriptor. | Loom uses an exact local request and serializes publication with cancellation, but previously its result acceptance only bounded transcript bytes. | Shared `validate_parakeet_response` gives both products the exact response identity and native-inference check. Source evidence and local text bounds remain product-specific. Loom rejects mismatched response provenance before recording a completed transcript. |
| Read-aloud source/playback | Mom binds the assistant message and text hash, validates again before serving bytes, retains bounded revocable playback records, and suppresses late publication on Stop. | Loom reads arbitrary selected prose rather than assistant messages, returns audio without autoplay, and holds a synthesis close guard. The Apple backend's automatic voice choice can use quality/language ranking rather than Mom's lexical default selection. | Keep these complementary behaviors. A unified document UI should retain Mom's source-revalidation/revocation model for selected text while permitting Loom's selection semantics and automatic voice policy. This slice does not erase source ownership into a bare string or implement new playback controls. |
| Microphone and capture persistence | Mom intentionally has no microphone composition; audio attachments can be transcribed and inserted only as explicit edits. | `microphone_capture.rs` owns the device on a joined thread, preallocates callback buffers, detects overflow, bounds capture to five minutes, and preserves a failed-to-save capture for exact retry through `audio_io.rs`. | Retain Loom's capture owner and exact pending buffer. No substitute implementation is added to Mom. These become an optional template capability when the application roots merge. |
| Cancellation and cleanup | Mom separates operation stop from playback revocation, remembers bounded pre-admission stops, and joins the non-preemptive Apple call. | Loom binds capture/transcription to project/session/document, retains JoinSet tasks, drains even after microphone errors, and checks cancellation before publishing the transcript. | Keep both product scopes. Core SpeechHost continues to own speech requests, cancellation, and backend workers. No text-generation request type is imposed on speech. |
| Input and codec validation | Mom transcription starts from an Attachment-validated artifact; Apple backend owns native WAV encoding. | Loom capture produces PCM16 mono 16 kHz; speech input currently performs a WAV header/size preflight; Parakeet performs actual WAV decoding. | No second audio decoder is introduced. Shared response validation is explicitly an envelope/provenance/bounds check, not a claim to decode arbitrary WAV bytes. Rich codec validation remains in Attachment, the speech backend, and the capture encoder. |

## Files and authority

- Shared production crate: `crates/desktop-speech`.
- Consumers: Mom desktop `speech.rs`; Loom plugin `speech_input.rs` and `audio_io.rs`.
- Existing SpeechHost, SpeechRouter, Apple backend, Parakeet backend, capture device, playback buffers, and transcript insertion contracts remain their owners.
- Discovery is noninteractive and registers local descriptors. It does not activate the microphone, synthesize, play, download assets, or load Parakeet inference weights.
- Byte and text limits remain visible product policy: Mom synthesized audio 64 MiB, Loom 32 MiB; Mom transcript 200,000 characters, Loom 4 MiB. No heuristic converts those different limits into an assumed equivalent.
- Core lifecycle and desktop speech packages are registered in `ci/package-groups.json`.

## Boundaries still to converge

The application composition roots still instantiate their own shared-host owner. Once those roots merge into one binary, pass the same speech service to both templates; do not retain per-template hosts. A unified source-bound playback object needs document-selection identity alongside message identity, expiry/revocation, and explicit playback. Transcript insertion should use the shared document edit/promotion path but never mutate a document merely because transcription completed.

No microphone device, actual utterance, model transcription, playback, or window-close acceptance was performed in this slice. Tests establish request policy, result validation, cancellation ordering, and host lifecycle behavior only.

## Validation receipt

- `cargo test -p desktop-speech --lib`: 3 passed.
- `cargo test -p mom-llama-app speech::tests -- --test-threads=1`: 6 passed.
- `cargo test -p tauri-plugin-loom speech_input --lib -- --test-threads=1`: 13 passed, including deterministic lazy-host/shutdown exclusion.
- `cargo test -p tauri-plugin-loom audio_io --lib -- --test-threads=1`: 4 passed, including exact pending capture retry.
- `cargo clippy -p desktop-speech -p operation-lifecycle --all-targets -- -D warnings`: passed.
- Follow-up verification of lifecycle commit `b86b869`: `cargo test -p mom-llama-runtime operation_scope --lib -- --test-threads=1`: 3 passed.
- All Cargo commands used the team's shared target cache; no full workspace build or live desktop acceptance was claimed.
