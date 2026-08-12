# W3 native path-cutover integration coverage

The default diagnostic harness resolves the five imported native crates from
`crates/native`. All other products remain exact Git dependencies. Loom's
incompatible SQLite dependency line is covered by a separately materialized,
authenticated, locked graph that also patches native dependencies to the
imported paths. No product repository or release manifest is modified.

## Default portable graph

The default workspace gate compiles these public boundaries:

- imported `llama-native-types`, `llama-native-cache`,
  `llama-native-engine`, `llama-native-host`, and `command-evidence`, whose
  preserved source main is `16168bd76a09f74fdee41d0e2fb0441e79ac1005`;
- `fte-types` at `67814e76659688fef61f311db588d17eddee0a66`;
- `attachment-native-types` at `2a8d3a9a1828162a51185d207822ceb1ba6283a8`;
- `speech-native-types` at `b836318f10a7e11f433ec3ea8dfa48707adc9b06`;
- `information-native-types` at `7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe`;
- `loom-types` at `223110bee4be72386d79306b444517371e4a9930`;
- W1 contracts, testkit, and vertical validator at
  `3ed1f3235edb6d481c324f05fe83b2379e3431e6`.

The test suite executes the shared deterministic lifecycle reference suite and
authenticates all eighteen accepted vertical manifests from the exact W1 Cargo
Git checkout against the accepted lock.

## Exhaustive portable-library probes

The eight first-party source workspaces contain 62 members. Fifty-one are
portable library members. Eleven non-library, UI, or platform-shell members are
outside this compile claim: `tauri-plugin-free-token-energy`,
`free-token-energy`, `mom-llama-cli`, `mom-llama-app`,
`attachment-native-cli`, `tauri-plugin-speech-native`,
`information-native-cli`, `tauri-plugin-information-native`, `loom-cli`,
`tauri-plugin-loom`, and `loom-app`. The two `llama-cpp-rs` library packages
are an additional external unsafe boundary, not part of the 62/51 counts.

Cargo empirically rejects all 51 current first-party libraries in one graph.
Loom's `rusqlite 0.39.0` requires `libsqlite3-sys 0.37.0`, while Mom and
Information use `rusqlite 0.32.1` and `libsqlite3-sys 0.30.1`; both native
packages declare `links = "sqlite3"`. The immutable current SHAs therefore
falsify the phase-one section 19 single-graph goal. The exact conflict is
recorded in `graph-boundaries.json`.

Two locked probes exhaust the portable inventory while the accepted repository
state retains one root lock:

- `current-product-graph` compiles the 36 non-Loom portable first-party
  libraries and the direct current `llama-cpp-2` boundary;
- `cargo xtask loom-probe` materializes an authenticated manifest and lock in a
  temporary directory, then compiles all 15 Loom portable libraries with the
  compatible W1 contracts, testkit, and vertical fixtures.

The probes establish exact-revision coexistence with every native dependency
rebound to the imported packages. They do not change product repositories.

Mom's accepted current revision still resolves its immutable pre-import
Attachment pin at `472900732ded5bcfb5cc639c49b3a4f77feece27`. Its former
native pin at `f7a69316c64d857b99bd847dd44cd852fc5b4ca4` is overridden by
the monorepo path policy and is absent from the locks.

This harness does not claim one-graph compatibility, full application
compilation, UI execution, hosted-provider behavior, or product-adapter
lifecycle execution. Real-model and host-join evidence is recorded separately
and never inferred from these compile probes.
