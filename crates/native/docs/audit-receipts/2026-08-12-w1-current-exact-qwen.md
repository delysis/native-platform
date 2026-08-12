# W1 current exact Qwen baseline — 2026-08-12

This test-only descendant freezes the Native owner-thread slice of W1 row 9
against production source `b36e6e9ed9efde020515f66611daeeb6f7bfc84a`.
The source identity is the exact `git ls-tree -r` byte listing for
`crates/llama-native-engine` at that commit. The vertical dependency is pinned
to merged protocol `9fd803f5efcc46ac0256dab876e7c0b1f03bb448`; the accepted lifecycle-contract
pin remains `cbab33555ab9355a6ac453d659c55ec9e0666821`.
`fixtures/w1/MANIFEST.sha256` authenticates every bundle member and itself has
SHA-256 `14c195d4ae97990f705ac8d8386a2d50fb6622083c633c4b3bc79dd10a251d5a`.

The external prerequisite is exactly 484,220,320 bytes with SHA-256
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`.
The ignored operational test streams those bytes independently through the W1
verifier, then asks the production loader to enforce the same digest before
admission. It tokenizes the checked-in prompt through the production model,
submits those exact token IDs through `generate_batch`, and consumes the
non-forgeable `wait_verified` seal. That seal performs the strict post-generation
artifact identity check before it returns. The test derives completion and
ownership facts from the seal, live status, and `JoinedNativeModel`; proves
nonempty text, real-engine evidence, non-fixture evidence, and in-process
transport; and rejects unverified fill-in-middle.

Local verification on an Apple M4 Max passed repeatedly using the exact GGUF;
the two strict-seal projection runs passed 1/1 in 3.78 and 3.88 seconds. They selected the CPU
runtime as requested by the fixture; llama.cpp also inventoried the host's
Metal device during process initialization. Accelerator availability,
performance, and exact generated prose are intentionally omitted claims.

Portable CI authenticates the manifest, checked-in request, frozen production
tree listing, and expected projection, and compiles/lints the exact protocol
integration. It cannot substitute for the recorded local GGUF execution and
does not claim to do so.
