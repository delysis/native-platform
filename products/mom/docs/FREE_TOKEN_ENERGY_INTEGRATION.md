# Free Token Energy consumer boundary

Mom Llama consumes Free Token Energy as an embedded Rust gateway. FTE owns
routing, protocol codecs, provider adapters, optional authenticated loopback
interfaces and speech modules. Mom Llama owns product policy and defaults to
`local-only`.

## Dependency rules

- Every FTE package is pinned to one immutable Git revision.
- That FTE revision pins the same immutable `llama-native-kit` revision Mom
  Llama uses directly, so Rust sees one `NativeHost` and one DTO identity.
- Release manifests contain no sibling path overrides and no `[patch]` section.
- Mom installs `tauri-plugin-free-token-energy`, which is text-only.
- Mom does not install `tauri-plugin-free-token-energy-speech`, register speech
  backends or grant speech permissions until the product has an intentional
  speech UX.
- Loopback remains disabled at startup. FTE's typed Rust/IPC boundary can start
  it explicitly, but Mom Llama does not yet expose a loopback Settings control.
- Hosted routes cannot enter the `local-only` candidate set.

`scripts/check-architecture.sh` validates the locked Cargo graph against these
rules. The lower-level native request/cache contracts belong to
`llama-native-kit/docs/FREE_TOKEN_ENERGY_INTEGRATION.md`.
