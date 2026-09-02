# Free Token Energy separation boundary

Free Token Energy remains a standalone product owning routing, protocol codecs,
provider adapters, and optional authenticated loopback interfaces. Mom Llama
does not compose it. Ordinary Mom chat calls its one product-owned `NativeHost`
directly. Local speech belongs to `speech-native-kit`; Mom composes only its
product-owned Apple complete-WAV Read Aloud and verified Parakeet
attachment-transcription edge.

## Dependency rules

- FTE packages remain rooted at `products/fte` for the standalone FTE process.
- Mom child manifests contain no FTE crates or Tauri plugin.
- Mom grants no FTE renderer permission, stores no FTE response adapter state,
  and has no gateway drain branch.
- Mom owns one `NativeHost`; FTE owns its own process composition.
- Mom child manifests inherit local dependencies from the root workspace, and
  the root lock contains no retired first-party Git source.
- Mom registers only Apple complete-WAV synthesis and verified Parakeet
  complete-input transcription behind product-owned path-free commands. It
  grants no microphone or speech-plugin permission and installs no generic
  speech Tauri plugin.
- Mom exposes no loopback or hosted-route control. Those remain FTE product
  concerns.

`scripts/check-architecture.sh` validates the locked Cargo graph against these
rules. Historical FTE receipts remain unchanged and are not current composition
evidence. The lower-level native request/cache contracts belong to
`llama-native-kit/docs/FREE_TOKEN_ENERGY_INTEGRATION.md`.
