# Optional speech bridge

Free Token Energy does not own local STT/TTS execution. The independently
versioned [`speech-native-kit`](https://github.com/delysis/speech-native-kit)
owns speech contracts, local/platform backends, request lifecycle, and its
optional Tauri plugin.

An FTE speech integration, when implemented, is an optional edge adapter with
this dependency direction:

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
