# Loom's Presage SQLite lifecycle extension

This crate, its migrations, and SQLx query metadata are from the immutable
revision in UPSTREAM.json. LICENSE.md retains the upstream AGPL-3.0-only license.
The manifest records original hashes for reviewing future upgrades.

The patch adds `SqliteStore::open_with_pool` and forbids unsafe Rust. Existing
constructors delegate to that method. Loom retains the pool owner and explicitly
awaits SQLx shutdown after its receiver stops, before the process can exit.
Without that ownership, dropping the upstream store detached SQLite connection
shutdown; SQLCipher cleanup raced process exit and crashed in sqlite3FreeCodecArg.
No schema, cryptography, trust default, or protocol behavior is changed here.
