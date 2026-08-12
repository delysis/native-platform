# R1 real-Qwen and same-store recovery acceptance

Candidate head: `b152635596ece5bff07fda4b8b73891284f1bbe6`

Implementation under test: `44a6f5bde09be51a315b4ba665195ba06e0ba1d9`

Exact local bundle:

- path: `target/release/bundle/macos/Mom Llama R1 44a6f5b.app`
- identifier: `com.delysis.llama-native-kit.mom-llama`
- executable bytes: `35,163,056`
- executable SHA-256: `c1d338ae69412b5f72b69e1bae6325b83af5a3ff6da73368bbcb31d6996c2fa4`
- signature boundary: ad hoc, no TeamIdentifier; this is native functional
  acceptance and is not signed-release acceptance

## Secure-store boundary

The release data directory was the unchanged canonical
`/Users/george/.local/share/llama-native-kit/mom-llama`. Its Keychain item was
resolved through service `com.delysis.llama-native-kit.mom-llama.store.v1` and
the path-derived account identifier already used by the product. No test key,
alternate data directory, development store, or plaintext bypass was supplied.
No Keychain secret bytes were read or recorded.

The exact app first displayed `Unlocking Mom Llama's encrypted local data…`,
then hydrated the existing encrypted settings, conversations, and drafts from
the same release store. The selected model path was visibly the real local
Qwen GGUF:

`/Users/george/Documents/llama-native-kit/target/test-models/Qwen_Qwen3-0.6B-Q4_K_M.gguf`

## Negative result retained

An earlier attached launch surfaced a fidelity defect in the acceptance setup:
the selected conversation displayed a Qwen label while its persisted
conversation execution profile loaded SmolLM2. That run is not counted as
Qwen evidence. A fresh conversation was created after confirming the global
model path, and the app was relaunched before the accepted run.

## Accepted native generation

The attached fresh process loaded the configured GGUF directly through the
in-process Metal backend. The runtime metadata identified:

- GGUF version 3
- architecture `qwen3`
- model name `Qwen3 0.6B`
- file type `Q4_K - Medium`
- file size `456.11 MiB`

With the prompt `/no_think` followed by an instruction to return a unique
marker, the visible assistant response was exactly
`R1-QWEN-NATIVE-20260812-0127`. The visible message metadata identified
`Qwen-Qwen3`, `Q4`, `local`, and 25 generated tokens. No fixture or hosted
provider was involved.

## Same-store draft recovery and final join

The unsent marker `DRAFT-R1-QWEN-20260812-0128` was entered in that Qwen-backed
conversation and allowed to pass the draft debounce. Native Cmd-Q then closed
the attached process with exit code 0. The emitted shutdown receipt was:

```json
{"Ok":{"elapsed_ms":32,"gateway_drained":true,"native_host_joined":true,"joined_native_worker_count":1,"application_work_drained":true}}
```

Relaunching the exact same bundle without a data-directory override restored
the same conversation, its real-Qwen response, the selected Qwen model, and the
exact unsent draft marker. The acceptance marker draft was then cleared and the
app was closed normally.

This closes the current-code Keychain-authorized real-Qwen generation,
same-store draft recovery, and model-loaded native Cmd-Q join gates. Dock Quit,
direct AppleEvent quit, and stable signed-release acceptance remain separate
distribution/lifecycle observations rather than prerequisites for this
functional gate.
