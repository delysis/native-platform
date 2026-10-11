# Speech Gateway Boundary

The speech module is a sibling to the text-generation gateway, not another
shape of `GatewayRequest`. Speech-to-text and text-to-speech have separate
typed requests, streams, responses, capabilities, and terminal events in
`speech-native-types`.

Live transcription returns a ticket with a bounded event receiver and a typed,
backpressured `TranscriptionAudioSink`. Complete-file transcription omits that
sink. Dropping either ticket invokes cancellation; terminal results remain
authoritative in Rust. Ticket drop never releases the host-wide request ID:
the host retains a private operation lease until the selected backend's final
response resolves, even when the consumer abandons its event or final channel.
Request IDs are unique across every registered backend for their complete
executor lifetime, not merely for the lifetime of a client handle.

This slice establishes five reusable crates:

- `speech-native-types`: protocol-neutral STT/TTS contracts, privacy and routing
  policy, model and voice descriptors, event tickets, cancellation, usage,
  typed errors, and the backend trait.
- `speech-native-platform`: deterministic platform adapter discovery and bounded
  aggregation of evidence from native or embedded runtime probes.
- `speech-native-router`: hard capability/privacy gates followed by deterministic
  policy ordering, exact model/voice routing, and complete rejection receipts.
- `speech-native-host`: a Tauri-independent registry and execution service that
  plans only across actually registered backends, pins the resolved
  backend/model/voice into each request, owns cancellation and shutdown, and
  relays backend finals through host-owned monitor tasks.
- `speech-native-backend-parakeet`: an executable, embedded Parakeet Realtime EOU 120M
  backend over `parakeet-rs` and ONNX Runtime. It loads one shared model handle
  from exact manifest-verified bytes in the selected pinned shared Hub snapshot
  or content-addressed managed storage, with independent decoder state per
  request.

On macOS, `AppleCapabilitySource` now performs a real, noninteractive runtime
inventory through safe Speech and AVSpeechSynthesizer bindings. It does not ask
for permission, activate a microphone, speak, or download voices. It reports
authorization and unavailable assets as readiness states for a later explicit
user action.

`AppleSpeechBackend` is the first executable adapter. It renders through
AVSpeechSynthesizer's buffer API, packages the native PCM as WAV, never plays
audio, and does not request permission. The current safe Rust wrapper requires
a live AppKit event loop for offline synthesis: its command-line test is
therefore explicitly ignored, while an opt-in launched-Tauri smoke is the real
acceptance boundary. The launch receipt must record real platform synthesis,
`network = never`, a real installed voice, a valid WAV, and
`fake_fixture = false` before this adapter is considered product-proven.

The executable Apple path currently advertises non-streaming WAV output. Its
upstream bridge collects buffers synchronously and has an internal timeout, so
it would be dishonest to claim live audio streaming or pre-emptive native
cancellation. Those capabilities remain false until a truly asynchronous
bridge is implemented and tested.

Synthesis responses carry an explicit output disposition. `Complete` owns the
returned bytes and format; `Streamed` records the format and byte count already
delivered through bounded audio events. An empty final audio buffer is never a
compatibility marker for streaming. Apple advertises and returns WAV bytes
only; playback remains a product responsibility.

The reusable speech crates have no Tauri dependency. W9 removed the generic
speech Tauri plugin because no current product installs it. A product that
ships speech must own its narrow IPC commands, permissions, capture/playback
UX, and host shutdown at the product composition root. The provider/text
gateway contains no speech state, dependency, command, permission, or shutdown
path. Ordinary Rust consumers use the service and backend traits without
Tauri.

The host lifecycle is one-way: `running -> quiescing -> closed`. Route
selection and global request-ID reservation occur under the same state lock.
The shutdown leader closes admission, cancels every exact active route, asks
every backend to stop, waits for all backend finals, and waits for supervised
relay activity to reach zero. Relay and blocking-worker task records self-reap
at completion; retained task state is one failure record plus an aggregate
failure count, rather than one join record per historical request. Async and
blocking panics are converted into shutdown evidence. The Parakeet and Apple
adapters use the same active-count contract, so neither adapter can report
shutdown while its native worker remains alive. Host and backend request nonces
use checked allocation and fail closed before wraparound.
A product edge must perform this joined shutdown during its native application
exit event and surface failure rather than discarding it. If live
transcription is exposed, the product edge must preserve both halves of the
typed contract: bounded output events and the backpressured input audio sink.
One bounded sink actor owns push admission and the irreversible finish commit;
an acknowledged finish closes downstream delivery and makes every later or
still-blocked push fail. Closing either side cancels only that request.

Applications that do not use speech need no speech crate, permission, backend
registration, or shutdown path.

## Default Selection Policy

The default profile is `private_balanced` under `local_only` privacy:

1. Use a platform-native backend only when it is ready, meets the requested
   operation features, explicitly reports `network = never`, and provides
   confirmed runtime or real-smoke evidence.
2. Otherwise use an installed embedded backend: Parakeet for transcription and
   Kokoro-class synthesis.
3. An already-resident Gemma 4 E2B, E4B, or 12B audio model may be a complete
   audio transcription fallback when that exact model/runtime combination has
   passed a real smoke. It does not inherit streaming, timestamps,
   diarization, or partial-result claims from a general audio-input flag.
4. Hosted speech never enters a local-only route. It can be added only by an
   explicit hosted-enabled privacy policy.

“Native” and “local” are deliberately different facts. Merely compiling for an
OS produces adapter candidates, not capabilities. A capability enters a
private route only after runtime evidence proves both availability and
never-network behavior.

## Platform Probe Matrix

| Platform | Native candidates | Private routing rule | Embedded fallback |
|---|---|---|---|
| macOS / iOS | SpeechAnalyzer/SpeechTranscriber, SFSpeechRecognizer, AVSpeechSynthesizer | SpeechAnalyzer assets and on-device operation must be present; older recognition must both support and require on-device operation; voices are inventoried individually | Parakeet STT, Kokoro TTS, eligible resident Gemma audio |
| Windows | Windows speech recognition and synthesis | Free-form dictation is treated as online and package-identity-dependent; it is not a private default. Installed TTS is eligible only after runtime audio/voice evidence | Parakeet STT, Kokoro TTS, eligible resident Gemma audio |
| Android | Explicit on-device SpeechRecognizer and TextToSpeech | Only `createOnDeviceSpeechRecognizer` with runtime availability is local; TTS voices requiring network are excluded | Parakeet STT, Kokoro TTS, eligible resident Gemma audio |
| Linux | Spiel providers | Each provider, voice, returned-audio path, and network behavior is probed; the platform name alone proves nothing | Parakeet STT and Kokoro TTS are the dependable default; eligible resident Gemma audio |

## Native Bridge Contract

Each platform adapter implements `PlatformCapabilitySource` directly or sends
a versioned `fte.speech.capability_report.v1` JSON payload to
`ReportedCapabilitySource::from_json`. The probe:

- enforces unique source, backend, and capability identifiers;
- validates capability ownership and runtime-evidence provenance;
- gives each source an independent deadline;
- preserves failures and timeouts as typed source reports;
- sorts reports and descriptors deterministically;
- never lets one failed source erase another working backend;
- never promotes documentation or build-target evidence to a private route.

Permissions and model downloads remain typed readiness states. Discovery must
not request microphone access, download a model, or trigger a platform consent
dialog. Those state changes happen only after a user action selects the
capability.

## Embedded Parakeet Runtime

`speech-native-backend-parakeet` is the first executable cross-platform fallback. Its
current model is `parakeet-realtime-eou-120m-v1-onnx` from immutable revision
`altunenes/parakeet-rs@a61d2818df4659c956b9661a9447f46e98c15126`.
The checked-in manifest binds the ordered three-file bundle to 480,708,981
bytes and combined SHA-256
`c710ae82b52aa969f89874e7e7b35ad570fec50cc3d943a4fdde0bb874948756`.
Source selection chooses `ParakeetBackendConfig::model_dir` first, then
`SPEECH_NATIVE_PARAKEET_MODEL_DIR` (with the current `FTE_PARAKEET_MODEL_DIR`
alias), otherwise the exact manifest revision in the selected shared Hub cache.
The Hub root comes from `desktop-model-defaults`: `HF_HUB_CACHE`, then
`HUGGINGFACE_HUB_CACHE`, then `HF_HOME/hub`, then
`XDG_CACHE_HOME/huggingface/hub`, otherwise `.cache/huggingface/hub` under the
available home directory. An override selects one cache; discovery does not
search the other roots after it.

The bounded metadata-only startup probe checks, in order:

1. the selected exact manifest-pinned Hub snapshot, when it matches that cache;
2. the exact content-addressed bundle already in managed storage;
3. the selected other candidate, if present.

The startup probe checks only the exact names, regular-file/private-file shape,
and manifest lengths; it does not read model bytes or construct ONNX sessions.
A shape-admitted source is reported as a nonresident
`CapabilityAvailability::DeferredLoad` with inconclusive system-inventory
evidence, never runtime proof. The router admits that state only for an exact
embedded-backend plus exact-model selector; automatic, fallback, hosted, and
non-embedded routes cannot select it.

Mutable refs, arbitrary snapshots, and cache locators alone are never admission
authority. On the first exact dispatch, one supervised single-flight loader
verifies the selected manifest-pinned shared Hub snapshot in place, including
its blob aliases and exact per-file and combined length/SHA-256. Otherwise it
reverifies an existing managed bundle, or streams the selected other candidate
through the manifest into private staging, verifies the staged bytes, and
atomically publishes them under the combined content hash in managed storage.
That copy path never hard-links candidate bytes. Both shared and managed model
bytes are verified immediately before and after ONNX session construction.
Only then does the descriptor transition to resident `Available` with confirmed
runtime evidence. Concurrent first-use requests share that load; cancellation
and quiesce wake waiters, and shutdown joins the non-preemptible loader before
completing. The managed root is selected from `ParakeetBackendConfig`, then
`SPEECH_NATIVE_PARAKEET_MANAGED_ROOT`, otherwise `local/speech/models` under the
selected shared Hub cache. When exact files are absent, the descriptor reports
an `asset_install_required` blocker. Neither discovery nor first use downloads
anything.

The model is English-only and advertises PCM/WAV input, streaming, and partial
results. It does not claim timestamps, diarization, translation, hotwords, or
generative transcription. Complete and live audio are downmixed and linearly
resampled to the model's exact mono 16 kHz input. After the first successful
exact dispatch, one verified ONNX handle remains resident, while each request
has separate encoder/decoder state and an independent cancellation flag.

The second embedded lane remains intentional rather than forgotten:

- `parakeet-rs`/ONNX is the currently executable default embedded lane.
- `parakeet.cpp` has a flat C API, in-memory PCM, timestamps, batching,
  streaming variants, and broad prebuilt targets. It remains a planned
  shared-library backend for targets where its GGUF packaging is advantageous;
  it must be dynamically isolated from llama.cpp's ggml symbols and pass the
  same backend equivalence suite before becoming selectable.
- `sherpa-onnx` remains a useful broader fallback direction for Parakeet-family
  and Kokoro models, but is not counted as implemented.

## Evidence and Acceptance

A platform adapter is ready only after it can produce a truthful descriptor
from the running machine. A production backend additionally needs real audio
fixtures covering output accuracy, streaming order, cancellation, no reload
for an identical resident configuration, network denial, and exactly one
terminal event. TTS needs returned-audio and voice-specific network tests; STT
needs exact feature tests for timestamps, partials, translation, and
diarization.

The pre-extraction FTE desktop produced real Apple and Parakeet launched-app
receipts through the former plugin-managed host. Those receipts are historical
provenance, not readiness for this repository's next release. A product-owned
launched-app smoke must rerun Apple synthesis and Parakeet transcription before
a release claims launched-app evidence. The real Parakeet integration
test uses `SPEECH_NATIVE_TEST_WAV` (with a legacy alias), streams PCM through
the input sink, and cancels one request without affecting its completing peer.

Primary references used for these rules:

- Apple SpeechAnalyzer and on-device SpeechTranscriber:
  <https://developer.apple.com/documentation/Speech/SpeechAnalyzer>
- Windows speech recognition constraints:
  <https://learn.microsoft.com/windows/apps/develop/input/speech-recognition>
- Android explicit on-device recognizer:
  <https://developer.android.com/reference/android/speech/SpeechRecognizer>
- Android voice network requirement:
  <https://developer.android.com/reference/android/speech/tts/Voice>
- Spiel provider model:
  <https://project-spiel.org/>
- Gemma 4 model card:
  <https://ai.google.dev/gemma/docs/core/model_card_4>
- parakeet.cpp:
  <https://github.com/mudler/parakeet.cpp>
- parakeet-rs:
  <https://github.com/altunenes/parakeet-rs>
- sherpa-onnx Rust examples:
  <https://github.com/k2-fsa/sherpa-onnx/tree/master/rust-api-examples>
