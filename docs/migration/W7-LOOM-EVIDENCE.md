# W7 Loom import: candidate evidence

Status: **accepted, merged, tagged, and source-frozen**.

Accepted Loom source `223110bee4be72386d79306b444517371e4a9930`
and tree `89eeaa6129d42d31ebb16b425189b3ffefb16724` are imported under
`products/loom`. The deterministic rewrite maps all 77 source commits,
including eight merges, to filtered head
`fe35a14bfad7fd1958f29edd9d209e3c72bd1692`. Raw merge
`19147c74bbe6335331f3fdad256663906c122dc3` has exact parents W6 candidate
base `be08d82eb6d71681f78bd84bae6a37257d5c6d36` and that filtered head. Its
prefixed Loom subtree is byte-identical to the accepted source tree.

## Local graph and parity

Loom remains an explicitly excluded nested workspace for W7 solely because
its accepted rusqlite 0.39 / libsqlite3-sys 0.37 link target cannot coexist
with the root rusqlite 0.32 graph. Native and W1 dependencies resolve through
imported local paths. The external Loom graph probe has been removed; CI and
root scripts execute the imported Rust and frontend workspaces directly.
`scripts/check-loom-import-history.sh` authenticates every commit mapping,
tree, parent topology, author and committer identity, date, and subject.

The following local gates passed from the imported paths:

- 637 ordinary Loom Rust tests passed; eight environment-bound tests remained
  explicitly ignored;
- strict all-target Clippy and Rust formatting passed;
- three W1 fixture-manifest tests, the exact-Gemma source authentication, and
  the loaded-owner close/join fixture passed after their source identities
  were rebound through the recorded filtered-commit map;
- 32 frontend test files containing 182 tests passed;
- Svelte check passed with zero errors and zero warnings;
- the Vite build passed with only its existing large-chunk advisory;
- root source-lock, integration-current, workspace-policy, shell-policy, and
  imported-history checks passed.

The exact W1 Gemma vertical used a real 4,954,576,032-byte
`gemma-4-E2B-base-Q8_0.gguf`, SHA-256
`aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`.
It passed with real model, generation evidence, release, and worker join. That
focused executable vertical used CPU and does not itself claim Metal.

## Real macOS product acceptance

The exercised source commit is
`a300c71d609c2085d5b281e5edbb1955b3006ce1`. The uniquely identified bundle
is `Loom W7 a300c71.app`, identifier `app.delysis.loom.w7.a300c71`. Its
20,924,464-byte executable has SHA-256
`1bbbdd20523a8a8e2a8eeaa95bd2118be5de307746b89fcb72abd995d29e58f7`
and is ad-hoc signed; no distribution-signing claim is made.

The bundle loaded the exact Gemma model on Apple M4 Max Metal with all 36 of
36 layers resident on the GPU. In the visible native UI:

- the exact prefix `At dawn, the locked observatory began to breathe.` was
  typed into the ordinary manuscript surface;
- a real multiline caret-local suggestion became visible;
- accessibility reported `Suggestion available. Tab accepts; Escape
  dismisses.`;
- one ordinary Tab accepted the suggestion and accessibility reported
  `Suggestion accepted`;
- the resulting 279-byte manuscript persisted with SHA-256
  `274f169492f9f2e0ff0fc3fc6a093a871ee066f45887ab7bcea4b56682172ac3`;
- SQLite binds the selected candidate, source revision, resulting revision,
  foreground command, exact model environment, and imported Native backend
  identity. The source revision contains exactly three candidate IDs.

The loaded-model lifecycle also passed twice through the exact bundle. The
first visible Cmd-Q reached process exit zero and Metal deallocation within an
observed upper bound of 7,204 ms. The same bundle immediately relaunched,
loaded the same exact model on Apple M4 Max Metal, and visibly reopened the
exact four-paragraph manuscript. Its on-disk state remained 279 bytes with the
same SHA-256. A second visible Cmd-Q again reached process exit zero and Metal
deallocation, this time within an observed upper bound of 14,747 ms. These are
polling upper bounds, not claims of internal shutdown latency.

## W8 boundary

W7 consolidates Loom source and history but intentionally does not unify the
complete first-party Cargo graph. `products/loom/Cargo.lock` and
`products/loom/pnpm-lock.yaml` remain authoritative until W8 converges SQLite,
flattens the Cargo workspaces, and creates one root frontend lock. No new
Information integration, research profile, hosted critic, mobile UI, source
ingestion, or crate consolidation is claimed.

## Promotion and source freeze

PR required run `31693665960` passed policy job `94426458613` and macOS job
`94426458695` at exact candidate head
`746576b88859f7ec4ca9a03f86ef34c17aaebc13`. W7 merged without squashing as
`99c49908ef8ccdb39fbbb1f710331e8a4161bc43`. Annotated tag
`w7-import-loom-v0-2026-08-13` has tag object
`87e59891465545962ee06b7033634265acaefbbc`, peels to that exact merge, and is
protected by no-bypass ruleset `20794241`. Linux and Windows remain
informational under the Mac-first policy.

The accepted Loom source boundary remains commit
`223110bee4be72386d79306b444517371e4a9930`, tree
`89eeaa6129d42d31ebb16b425189b3ffefb16724`. Annotated source tag
`native-platform-v2-horizon-b-2026-08-12` has object
`39580736ad0f3bace6753ba83cdb5ffe25571274` and no-bypass protection ruleset
`20794265`. README-only redirect commit
`5fc31a787d208586951afec88bc11a975170fe37` passed workflow-policy job
`94432427919` and macOS Rust job `94432481217` in run `31695560107`.
No-bypass hard-freeze ruleset `20794407` blocks branch creation, update,
deletion, and non-fast-forward operations. The source repository intentionally
remains unarchived through the two-release retirement window.
