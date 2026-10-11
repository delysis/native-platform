# speech-native-kit contributor guidance

Write safe, direct, idiomatic Rust. Prefer small explicit state machines and
typed failures over abstraction layers that conceal ownership or authority.

## Ownership

- This repository owns speech-domain types, routing, local/platform backends,
  bounded streams, cancellation, and lifecycle.
- Product applications own microphone capture, permission prompts, playback,
  transcript insertion, and user-facing controls.
- Provider gateways own hosted credentials, quotas, retries, accounting, and
  public HTTP compatibility endpoints.
- `llama-native-kit` owns llama.cpp. Generative audio bridges belong downstream
  of both native kits.

## Hard boundaries

- No HTTP server, loopback listener, provider SDK, credential store, telemetry,
  microphone activation, or automatic playback in core crates.
- Discovery is noninteractive: it must not request permission, download assets,
  speak, record, or open a network connection.
- Local-only routing admits only runtime-proven `network = never` capabilities.
- Every request has one terminal result; cancellation affects only its request.
- Backends own their runtime objects and join or stop workers on shutdown.
- Do not merge text-generation and speech request models merely because fields
  such as request IDs or deadlines look similar.
- Products own explicitly granted Speech IPC operations. There is no generic
  Speech Tauri plugin in the current workspace; historical permissions do not
  authorize a current product edge.

## Required verification

Use the root [CONTRIBUTING.md](../../../CONTRIBUTING.md) tool pins and evidence rules.
From the repository root, run the affected group:

```sh
node scripts/ci/cargo-group.mjs test service-speech
node scripts/ci/cargo-group.mjs clippy service-speech
bash crates/services/speech/scripts/check-boundaries.sh
```

Also check affected reverse-consumer groups; this command selects only the named
group. The root clean-tree component gate and separate platform, fuzz, real-model
and packaged acceptance obligations remain in force.
