# W8 SQLite 0.39 compatibility scout

Status: **prepared and locally verified before workspace flattening**.

The root, FTE, Mom, and Information graphs can converge on exact
`rusqlite = 0.39.0` and `libsqlite3-sys = 0.37.0` without a schema or migration
change. FTE and Mom require no Rust source adaptation. Information requires
small, explicit error propagation for APIs that became fallible in rusqlite
0.39, plus the replacement of the removed `DatabaseName::Main` spelling with
the generic database name `"main"`.

## API inventory

| Package | Source | SQLite surface | Features | Fixture/gate | 0.39 change |
|---|---|---|---|---|---|
| `fte-store` | `products/fte/crates/fte-store/src/lib.rs` | `Connection`, `OptionalExtension`, `params!`, ordinary execute/query | `bundled` through root | response roundtrip and delete | None |
| `free-token-energy` | `products/fte/src-tauri/src/db.rs` | `Connection`, `OptionalExtension`, `params!`, schema classification and migration | `bundled` through root | fresh/current/legacy/unversioned database tests, provider metadata, usage logs | Manifest now inherits root; no Rust change |
| `mom-llama-runtime` | `products/mom/crates/mom-llama-runtime/src/store.rs` and cache/attachment tests | `Connection`, `OptionalExtension`, `TransactionBehavior`, `params!`, encrypted document and cache metadata | `bundled` through root | settings, conversation, persona, attachment, cache quarantine, reopen, runtime suite | None |
| `information-native-host` | `crates/services/information/crates/information-native-host/src/lib.rs` | test-fixture `Connection` | inherited | W1 install/query and store reopen vertical | None |
| `information-native-backend-sqlite` | `crates/services/information/crates/information-native-backend-sqlite/src/lib.rs` | read-only URI `Connection`, `OpenFlags`, `OptionalExtension`, `params!`, `params_from_iter`, hooks, runtime limits, typed `ErrorCode` mapping | `bundled`, `hooks`, `limits` | Alexandria immutable/live identity, WAL, schema, search/read/lookup, deadline tests | Propagate `set_limit`, `limit`, and `progress_handler` errors |
| `information-native-backend-community` | `crates/services/information/crates/information-native-backend-community/src/lib.rs` | same read-only/hook/limit/error surface | `bundled`, `hooks`, `limits` | archive identity, rights, schema, search/read/lookup, deadline tests | Propagate `set_limit`, `limit`, and `progress_handler` errors |
| `information-native-backend-encyclopedia` | `crates/services/information/crates/information-native-backend-encyclopedia/src/lib.rs` | same read-only/hook/limit/error surface | `bundled`, `hooks`, `limits` | immutable/live identity, rights, schema, search/read/lookup, deadline tests | Propagate `set_limit`, `limit`, and `progress_handler` errors |
| `information-native-backend-scripture` | `crates/services/information/crates/information-native-backend-scripture/src/lib.rs` | same surface plus database read-only verification | `bundled`, `hooks`, `limits` | immutable/live read-only, WAL, schema, passage/occurrence retrieval | Propagate fallible limit/hook APIs; use `is_readonly("main")` |

No non-Loom consumer uses backup, collation, scalar-function, raw-handle, or
custom-VFS APIs. No production transaction shape, schema, migration, query,
error-classification rule, or compile-time SQLite feature is intentionally
changed.

## Disposable integration result

The isolated scout upgraded only the exact SQLite dependency family:

```text
rusqlite         0.32.1 -> 0.39.0
libsqlite3-sys   0.30.1 -> 0.37.0
hashlink         0.9.1  -> 0.11.1
hashbrown        0.14.5 -> 0.16.1
```

The root reverse tree contains one `libsqlite3-sys v0.37.0`. Focused FTE and
Mom tests passed: 27 FTE app tests plus one store test, and 131 Mom runtime and
integration tests; 15 environment-bound tests remained explicitly ignored.
The complete Information workspace test suite passed after the source
adaptation, and strict all-target Clippy passed with `-D warnings`.

W8 must still regenerate the single final root lock after all nested workspace
members are flattened, then record the bundled SQLite version and complete
`PRAGMA compile_options` digest from that final graph. This scout does not
claim the one-workspace HOME state.
