# Llama Native Kit implementation contract

This subtree owns the reusable, process-free llama.cpp runtime in the canonical
`delysis/native-platform` workspace. Product behavior belongs in `products/mom`,
`products/loom` and `products/fte`; Speech belongs in `crates/services/speech`.
The old standalone repositories are frozen provenance, not destinations for fixes.

## Non-negotiables

- Normal inference is in-process. Inference crates may not use networking,
  subprocesses, shell commands, or executable discovery.
- All llama.cpp objects stay on their owning worker thread. Binding objects and
  raw pointers never cross the public Rust API.
- Project-owned unsafe code is forbidden except in the documented native state
  buffer module, where every block must state its safety argument.
- Fixture engines are permanently labeled fixtures and never unlock runtime
  readiness.
- Cache reuse is an optimization. A fingerprint mismatch invalidates the cache
  and falls back to ordinary generation.
- The host accepts injected cache storage. It does not choose product databases,
  key managers, routing profiles, UI policy or protocol compatibility layers.
- `llama-native-*` crates may not depend on Free Token Energy, Mom Llama, Tauri,
  HTTP clients, network servers or process execution.

## Verification

Use the root [CONTRIBUTING.md](../../CONTRIBUTING.md) tool pins and evidence rules.
From the repository root, run the affected group:

```sh
node scripts/ci/cargo-group.mjs test native
node scripts/ci/cargo-group.mjs clippy native
bash crates/native/scripts/check-architecture.sh
```

Also check affected reverse-consumer groups; this command selects only the named
group. The root clean-tree component gate and separate platform, fuzz, real-model
and packaged acceptance obligations remain in force.
