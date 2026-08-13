# Real Gemma desktop acceptance receipt

Date: 2026-08-11  
Evidence class: designated-machine product acceptance, not cross-platform certification

## Immutable inputs

- Runtime source commit: `1fb2a6f131e2082cf85d1c40b51ab0ae66cd459c`
- Successor under review: `fe6f836a9d0c6b74eb9752e09732767ecce67fdd`
- Difference from the runtime source: test-only serialization of synthetic frontier-critic subprocess tests; no production source or bundle input changed
- Native dependency: `delysis/llama-native-kit@2d69f086e922ed7bdfd6236baf5a1ad0ed568360`
- Bindings dependency: `delysis/llama-cpp-rs@a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391`
- Bundle identifier: `app.delysis.loom.c1fb2a6f.acceptance`
- Executable SHA-256: `33d73ba4f49cc9ef91d56b4ebac2f836502db14daca6c8f3423c61f4ed9fe1fd`
- Model: Gemma 4 E2B base Q8_0 GGUF, 4,954,576,032 bytes
- Model SHA-256: `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`
- Device observed by llama.cpp: Apple M4 Max Metal

The model path is intentionally omitted because it is an operational local path,
not model identity or portable evidence.

## Exercised sequence

1. Launched the uniquely identified native bundle into a new app-local writing workspace.
2. Confirmed focus was already in the full-height visual manuscript surface.
3. Entered the exact human prefix `At dawn, the locked observatory began to breathe.`
4. Waited for the verified quiet-default writer to load the pinned model and complete its three-candidate family.
5. Observed a multiline caret-local ghost and the accessibility announcement `Suggestion available. Tab accepts; Escape dismisses.`
6. Pressed Tab once. The ghost became ordinary manuscript text and the accessibility announcement changed to `Suggestion accepted`.
7. Queried the authoritative project SQLite store. It contained one immutable `promote` selection event binding the candidate, source revision, resulting revision, and promotion command.
8. Sent Cmd+Q with the model resident. The application exited with status 0 after `ggml_metal_free: deallocating`.
9. Immediately relaunched the same bundle. The exact accepted manuscript reopened, the model loaded again, and a second Cmd+Q exited 0 with the same native deallocation boundary.
10. Confirmed no matching process and no new Loom crash report remained.

## Durable evidence

- Selection ID: `01KZQZHD7B2E7GJC2Z0W0HSSZW`
- Candidate ID: `01KZQZG3REP3P3J97MWA2F3FJG`
- Source revision: `01KZQZG2EZBCV7ER84ND4Q06HD`
- Resulting revision: `01KZQZHD7B1NY5ASZH7X39E1M8`
- Promotion command: `01KZQZHD6R8NPFKKB5ASG1J1KM`
- Reopened manuscript SHA-256: `51679f0d34e0029d43907e611bde2ea03af9b3acabc83d4e8e8a923ae0639b61`

The accepted continuation was unedited model output. The store, not this receipt,
is authoritative for its exact bytes and provenance graph.

## Boundary of the claim

This proves one exact local Metal product slice: model discovery/load, real base-
model generation, three durable candidates, rendered ghost presentation, explicit
acceptance, provenance-preserving promotion, loaded-model quit, persistence, and
immediate relaunch. It does not prove independent branch cancellation through the
desktop UI, other hardware backends, latency or thermal targets, signed packaging,
or the five-function autoresearch qualification program.
