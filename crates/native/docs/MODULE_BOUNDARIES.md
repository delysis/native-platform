# Ecosystem module boundaries

The dependency graph is intentionally one-way:

```text
llama-native-kit ──> free-token-energy ──> mom-llama
       └──────────────────────────────────────^ 

mom-llama ──contracts/black-box CLI──> capability-system-compiler
```

The arrows mean “is depended on by.” There are no reverse source dependencies.

| Repository | Owns | Must not own |
|---|---|---|
| `delysis/llama-native-kit` | llama.cpp DTOs, owner-thread engine, resident host, cache contracts | products, Tauri, protocols, providers, STT/TTS |
| `delysis/free-token-energy` | protocol gateway, routing, providers, loopback, optional speech modules | product UX or llama.cpp internals |
| `delysis/mom-llama` | Mom Llama runtime, CLI, contracts, receipts and Tauri app | reusable provider implementations or copied native engines |
| `delysis/capability-system-compiler` | compiler, Loom specs and black-box product acceptance | a second Mom Llama/runtime implementation |

## Native-kit public boundary

Downstream callers construct one `NativeHost`, inject any persistent prefix
store, load bounded resident models and submit `llama-native-types` requests.
This repository never selects a database, key manager, network policy, hosted
provider, application data directory or user interface.

`scripts/check-architecture.sh` enforces the negative half of this contract.
