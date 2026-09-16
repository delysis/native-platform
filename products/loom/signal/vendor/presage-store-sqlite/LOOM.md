# Loom's Presage SQLite lifecycle extension

This crate, its migrations, and SQLx query metadata are from the immutable
revision in UPSTREAM.json. LICENSE.md retains the upstream AGPL-3.0-only license.
The manifest records original hashes for reviewing future upgrades.

The patch adds `SqliteStore::open_with_pool` and forbids unsafe Rust. Existing
constructors delegate to that method. Loom retains the pool owner and explicitly
awaits SQLx shutdown after its receiver stops, before the process can exit.
Without that ownership, dropping the upstream store detached SQLite connection
shutdown; SQLCipher cleanup raced process exit and crashed in sqlite3FreeCodecArg.
Group cache updates now accept only strictly newer revisions. A reproduced
out-of-order fetch could otherwise restore removed members or old permissions.
The UPSERT preserves cached avatar rows while their path is unchanged, and
invalidates a changed avatar in the same transaction as the group snapshot.
The upstream SQLx metadata remains as provenance; this changed query uses bound
parameters and is checked against the actual migrated schema by worker tests.
No schema, cryptography, or trust default is changed here.
