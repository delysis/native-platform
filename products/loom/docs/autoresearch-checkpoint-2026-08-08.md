# Native autoresearch foundation checkpoint

The first native autoresearch slice was based on clean, published component
checkpoints. These exact commits are provenance records, not floating branch names:

- Loom Native initial foundation parent: `82681fc120dbbc8ea0cfd9f9025db44e63e1e571`
- Loom Native current integration base: `1e39e05b31d04f70af50721f2225631b68587106`
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

The test-only Rust legacy translator pins the preserved invalidation ledger and
its referenced bytes. It reconstructs 111 historical assemblies exactly but
keeps them unbound and quarantined; 17 additional records are rejected because
at least one extracted model span is empty. The older planning estimate of 108
reconstructable records is therefore superseded by the audited 111/17 split.
