# Native autoresearch foundation checkpoint

The first native autoresearch slice was based on clean, published component
checkpoints. These exact commits are provenance records, not floating branch names:

- Loom Native: `cec7dcfe9eec9916af3cd81ffcf8ff97016d0498`
- llama-native-kit inference baseline: `c61692d48b0768bb242bcecb7a80c3318fc476b4`
- llama-native-kit controlled generation: `4fd76f8a54652bdc219b4b87b42a8639af91fa71`
- llama.cpp Rust wrapper: `a74dbb79f96e0ebad8b0737ee1d3c9c1deb185af`
- Mom Llama: `30ce93ff4d5f8f0ab5e39da98fd2359df5ac5c13`
- Free Token Energy: `9d98d6e0c079e5730cb8f5cd0a71cc89d22c96fe`
- legacy Python research evidence: `cacbcc9689101d5b774d25c0acbf6857ac719c27`

Serializable research records are declarations and diagnostic evidence. They
never regain live admission through deserialization. `loom-inference` alone
consumes the native backend's opaque generation seal and mints a
`VerifiedInferenceEnvelope`. `loom-store` may persist and adopt that envelope;
it must never recreate authority from receipt fields, JSON, hashes, or replay
of these serializable records.
