# Historical speech bridge design

<!-- current-service-surface: speech -->
<!-- retired-edge-parent: 84f65a9c1313bc0e4218156507cedd9fd905903f -->

Free Token Energy does not own local STT/TTS execution. The independently
composed `crates/services/speech/crates/speech-native-host` owns transport-
neutral host execution. FTE has no current Speech bridge and the workspace has
no current `crates/services/speech/crates/tauri-plugin-speech-native` crate.
That plugin's last-present tree is retained at exact parent
[`84f65a9c1313bc0e4218156507cedd9fd905903f`](https://github.com/delysis/native-platform/tree/84f65a9c1313bc0e4218156507cedd9fd905903f/crates/services/speech/crates/tauri-plugin-speech-native).

The remainder of this document is a design constraint for a possible future
FTE speech integration, not a shipped command, permission, or product surface.
If implemented, it is an optional edge adapter with this dependency direction:

```text
speech-native-types ◄── fte-speech-providers
speech-native-host  ◄── fte-loopback-audio
```

FTE may provide:

- hosted OpenAI, Google, ElevenLabs, or other speech backends using FTE's
  injected secret resolver, quotas, retries, and accounting;
- strict codecs for `/v1/audio/transcriptions`, `/v1/audio/translations`, and
  `/v1/audio/speech`;
- an opt-in composition helper for applications that want both service
  families.

FTE must not provide:

- Apple/Windows/Android/Linux speech framework ownership;
- ONNX or local speech model lifecycle;
- microphone capture or permission prompts;
- automatic playback;
- implicit speech permissions in the text gateway plugin;
- a conversion of audio requests into text `GatewayRequest` values.

The bridge may share injected facilities such as secrets or asset-cache
location. It must not create a common “AI request” abstraction that erases
audio formats, timing, diarization, voice selection, or streaming semantics.
