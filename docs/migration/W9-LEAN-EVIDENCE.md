# W9 lean consolidation evidence

Status: **accepted and promoted through GitHub**.

W9 is measured from the protected W8 HOME merge
`058c4622fac8813417a5e6637091e63e1ef01ea2` (tree
`fb0362faf422090ec8d8f936d5b31b95da0b01ac`) to the final implementation and
documentation checkpoint `7cd3627683061f9f51a56b4d61a73c3767819d30`
(tree `88173c1ff488aaa9b4356b6fe5c158e352d28eb7`). The machine-readable receipt is
`migration/w9-lean.json`.

## Result

| Metric | W8 | W9 | Change |
| --- | ---: | ---: | ---: |
| first-party Rust nonblank lines | 293,326 | 179,910 | -113,416 (-38.66%) |
| production Rust nonblank lines | 261,075 | 169,206 | -91,869 (-35.18%) |
| test Rust nonblank lines | 29,975 | 8,476 | -21,499 (-71.72%) |
| lexical public Rust items | 5,579 | 2,567 | -3,012 (-53.98%) |
| workspace packages | 65 | 47 | -18 (-27.69%) |
| first-party Git dependencies | 0 | 0 | unchanged |
| tracked files at the Rust checkpoint | 1,096 | 693 | -403 |
| fixture bytes at the Rust checkpoint | 1,381,962 | 1,122,996 | -258,966 |

Across the final implementation checkpoint, Git records 1,789 inserted lines
and 136,898 deleted lines: net -135,109. These are repository line deltas, not
a substitute for the classified Rust metrics above.

The default Loom writing graph contains no research package. The research
classification fell from 89 files to one retained store boundary,
`loom-store/src/research_admission.rs`. That file remains because old project
stores can contain migration 7-10 history; it cannot mint new foreground
writing authority. The unowned scheduler, frontier, evaluator, benchmark,
learning, inference-admission, and research-import product paths were deleted.

## Deleted compatibility and scaffolding

W9 removed:

- 18 packages that were unused, migration-only, or unowned research edges;
- protected-import replay gates, product-local CI copies, W8 integration
  harnesses, and transient source snapshots;
- executable W1 contract/testkit/vertical-fixture packages, W1 adapters,
  feature flags, replay manifests, and the speech test application;
- the generic speech Tauri plugin, because no product installed it;
- Loom's research packages and foreground research-import UI/IPC;
- duplicated or research-only fixtures and superseded implementation plans.

Compact provenance remains: imported commit maps, the migration ledger, the
seal manifest, HOME evidence, and the protected source/HOME tags. Compatibility
fixtures that still prove user-data behavior were renamed and retained for
Loom prior-v10 opening/suggestion promotion, Mom store/cache reopening, and FTE
loopback behavior.

## Shipping profile

The selected first release profile is Mac-first and lean:

- Mom local chat, Personas, Attachment, and joined quit;
- Loom foreground writing, exact local suggestion promotion, and joined quit;
- FTE gateway/library and authenticated loopback;
- Information install/query core;
- Speech core and selected backends, without a generic desktop plugin.

Linux and Windows remain source-compatibility follow-ups. Their informational
GitHub jobs do not block local work or the required macOS gate.

## Build footprint and binaries

No model weights are embedded in the repository or product binaries. The
checkout was 114 MB and contained no GGUF, ONNX, safetensors, or model `.bin`
file. Mom and Loom open GGUFs by path; Parakeet resolves a Hugging Face cache
snapshot in place. Loom embeds only a small generated build-policy JSON.

A stale W9 `target/` reached 41 GB and exhausted the host volume. The cause was
full debug information, retained incremental objects, many test executables,
and repeated native-dependency feature/profile builds. `cargo clean` removed
exactly that reproducible worktree output. Root dev/test profiles now retain
line-table backtraces but disable incremental object retention.

A clean `cargo build --locked --workspace` then completed in 119.35 seconds and
used 5.1 GB. Building the renamed Loom GUI separately raised the directory to
6.4 GB because Cargo emitted a second feature-set variant. The prior W8 local
reference was 62.48 seconds with a different cache/toolchain state, so the two
times are recorded but not presented as a speedup. Required macOS CI wall time
will be measured on the W9 pull request; it is not a local acceptance blocker.

The W8 Loom macOS executable was 21,127,840 bytes. The W9 stripped release
`loom-app` executable is 20,685,776 bytes (-442,064, or -2.09%) with SHA-256
`0d5293b2988872efa2d5f88e472a89d736be4532da70f00b1869d0947aa77aed`;
its clean release build took 187.92 seconds. The GUI binary is now `loom-app`,
leaving the user-facing `loom` name unambiguously owned by `loom-cli`.

## Verification boundary

Passed locally on Apple-silicon macOS:

- locked workspace check across all targets;
- locked workspace strict Clippy across all targets with warnings denied;
- `cargo test -p xtask` and the lean policy;
- focused Loom prior-v10 and exact promotion/reopen tests;
- focused Mom prior-store and persistent-cache reopen tests;
- real FTE loopback socket security, streaming, storage, and restart tests;
- Loom frontend: 31 files / 178 tests, Svelte check with zero errors/warnings,
  and production Vite build;
- Node CI/policy tests: 27 passed;
- post-profile and binary-rename Loom build, strict Clippy, and tests: 3 passed.

One `scripts/check-shell-policy.sh` run compiled broadly and then failed with
`No space left on device (os error 28)` before the bounded profiles were added.
This receipt does not relabel that run as passing. After the profile correction,
a fresh run passed the complete locked workspace/all-target test suite, strict
Clippy, lean and migration policy, 27 Node CI tests, all four architecture
boundary scripts, and the offline frozen pnpm lock check. The complete gate
peaked at roughly 13 GB of generated target data rather than 41 GB.

PR [#17](https://github.com/delysis/native-platform/pull/17) passed its complete
required workflow at candidate `aca1f850f6ef37d23a06b7a17f26c334674aef95`
([run 31774222096](https://github.com/delysis/native-platform/actions/runs/31774222096))
and merged as `eaa318cf2e104e23b6dc0038b27508a1313526d5`. The required
workflow took 19 minutes from its first job start through the aggregate gate.

Signed/notarized distribution and source-repository retirement belong to W10,
not W9.
