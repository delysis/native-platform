# Architecture decision: speech is a sibling service

Speech recognition and synthesis are one coherent audio domain, but they are
not text-generation provider protocols. This repository therefore owns the
speech host independently from Free Token Energy and `llama-native-kit`.

```text
                              ┌────────────────────────┐
                              │      product app       │
                              │ capture · playback · UX│
                              └───────────┬────────────┘
                                          │
                         ┌────────────────┴───────────────┐
                         │                                │
               ┌─────────▼──────────┐          ┌──────────▼─────────┐
               │ speech-native-kit │          │ free-token-energy │
               │ local STT / TTS   │◄─────────│ optional hosted   │
               │ transport-neutral│  future  │ speech + /audio/*│
               └───────────────────┘          └────────────────────┘
```

The dependency arrow is a future design boundary, not a currently composed
edge. Speech never imports FTE's text request model, loopback server, provider
store, or secret resolver.

<!-- current-service-surface: speech -->
<!-- retired-edge-parent: 84f65a9c1313bc0e4218156507cedd9fd905903f -->

The current executable surface is the transport-neutral
`crates/services/speech/crates/speech-native-host` plus its types, router,
platform, and backend crates. There is no current generic
`crates/services/speech/crates/tauri-plugin-speech-native` crate. Its last
present source is preserved at exact parent
[`84f65a9c1313bc0e4218156507cedd9fd905903f`](https://github.com/delysis/native-platform/tree/84f65a9c1313bc0e4218156507cedd9fd905903f/crates/services/speech/crates/tauri-plugin-speech-native); commit
`17c59f51ce55b86f4b5bbbe57d79ebe9a5963a7f` retired that compatibility edge.
Historical receipts remain evidence about the historical bundle only.

## Boundary decisions

- `SpeechHost` is in-process and transport-neutral.
- Products must compose a narrow Speech edge explicitly; no generic Tauri
  Speech plugin is shipped in the current workspace.
- Hosted speech adapters may implement `SpeechBackend`, but live with the
  provider gateway that owns their credentials and accounting.
- OpenAI-compatible audio endpoints are codecs at FTE's loopback edge.
- Microphone capture and playback remain product responsibilities.
- Curated model assets are admitted by immutable manifest identity and copied
  into private content-addressed managed storage before load; mutable cache
  filenames are compatibility discovery hints, never authority.

The legacy `fte.speech.*` serialized schema identifiers remain stable in the
0.1 line so stored receipts and fixtures do not become unreadable. Rust package
and Tauri permission namespaces use the new ownership names immediately.
