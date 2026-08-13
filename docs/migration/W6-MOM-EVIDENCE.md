# W6 Mom import: candidate evidence

Status: **accepted, merged, tagged, and source-frozen**.

Accepted Mom source `3cf57941af6d523378e7fa8b24f5c24c8e50363f`
and tree `7670bc1bfb4b94959871d33f7487d3969b2a76c7` are imported under
`products/mom`. The deterministic rewrite maps all 59 source commits,
including five merges, to filtered head
`8189804d01be5d12384bcd6f01ceb2c7ef2d4fd7`. Raw merge
`cfa2d3c40e74e1d692c0cdb9354cc272249fd4ab` has exact parents W1/W5 base
`bc1c6cafe67d5cdbf2441c7155b89f129e8ba730` and that filtered head. Its
prefixed Mom subtree is byte-identical to the accepted source tree.

## Local graph and parity

Mom runtime, CLI, and Tauri packages are root workspace members. Their Native,
Gateway, Attachment, and W1 dependencies resolve through imported local paths;
the root lock contains no retired first-party Mom dependency and one
`libsqlite3-sys` line. Nested Mom workspace locks and workflows are historical
only. `scripts/check-mom-import-history.sh` authenticates commit mapping,
parent topology, author and committer identities, and every corresponding
tree.

The following local gates passed from the imported paths:

- 184 ordinary Mom tests passed; 14 real-hardware tests remained explicitly
  ignored;
- the W1 feature matrix passed 135 tests, with the same 13 real tests ignored;
- strict all-feature Clippy passed for all three Mom packages;
- architecture, contract parity, persona/product UX, frontend syntax, source
  lock, integration-current, workspace policy, and import-history checks
  passed;
- the exact local Qwen GGUF passed
  `real_native_chat_invokes_no_fixture_and_persists` with no fixture.

Two first-run defects were found by native interaction after the imported test
suites were green. Commit `bc5098c` now materializes a real conversation before
the landing composer dispatches. Commit `a601caa` raises the bounded default
generation budget to 512 tokens, so reasoning-capable models have room to emit
visible assistant text. Both corrections have focused regression coverage.

## Real macOS acceptance

The exercised source commit is
`a601caad0ecadf7f3a644103da10c0fdc5f08c60`. The uniquely identified bundle
was `Mom Llama W6 a601caa.app` with identifier
`com.delysis.mom-llama.w6.a601caa`. Its 95,491,264-byte executable has SHA-256
`601ab55cb522e10ec4fcedb5dd7980c3b1593334ac924dcbc59073679e4e4936`.

The real model was Qwen3 0.6B Q4_K_M: 484,220,320 bytes, SHA-256
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`.
No fixture was enabled. In the visible native UI:

- first-run submit created a real chat;
- reasoning and assistant text streamed from the local Metal model;
- the completed transcript persisted four user/assistant messages;
- the Stop control returned the typed status
  `The local model request was cancelled.`;
- a later long request showed `Stop`, then Cmd-Q was issued while generation
  was active;
- shutdown completed in 3,635 ms with the Gateway drained, application work
  drained, the operation supervisor closed, its one admitted operation worker
  joined, and the Native worker joined;
- immediate relaunch recovered the four-message transcript and exact unsent
  draft `W6 active quit draft a601caa: continue the lighthouse storm for many
  paragraphs.`;
- the relaunch then exited cleanly in 3 ms without loading workers.

## Promotion and source freeze

PR required run `31687282237` passed policy job `94406205848` and macOS job
`94406205909` at exact candidate head
`be08d82eb6d71681f78bd84bae6a37257d5c6d36`. W6 merged without squashing as
`78b5b1ecc44b3c79f79227613f027580032192cd`. Annotated tag
`w6-import-mom-v0-2026-08-13` has tag object
`5d8cd5e0cb9bba5db81d05687d0f63c9242b936c`, peels to that exact merge, and
is protected by no-bypass ruleset `20791545`. Linux and Windows jobs remain
informational under the Mac-first policy and were not acceptance gates.

The Mom source boundary remains accepted commit
`3cf57941af6d523378e7fa8b24f5c24c8e50363f`, tree
`7670bc1bfb4b94959871d33f7487d3969b2a76c7`. Annotated source tag
`native-platform-v2-horizon-b-2026-08-12` has object
`23ca2af17f4a0b7c9711eebf08c217ec44bd8d57` and no-bypass protection ruleset
`20791575`. README-only redirect commit
`c014f4681eccef19c040913b2fbedc4aee5cb299` passed source CI run
`31689626249` and shutdown run `31689626209`. No-bypass hard-freeze ruleset
`20791917` blocks branch creation, update, deletion, and non-fast-forward
operations. The source repository intentionally remains unarchived through
the two-release retirement window.

No Speech, Information, citation, attachment-format, or hosted-provider
product feature is included or claimed in W6.
