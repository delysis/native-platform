# Security policy

Report vulnerabilities privately to the Delysis maintainers through GitHub's
private vulnerability-reporting channel once the repository is published. Do
not open a public issue containing credentials, private data, exploit details,
or model artifacts.

The workspace contains native inference, hosted-provider, desktop, and sibling
service runtimes. First-party Rust is safe by default, with a documented
exception for the native engine's state-buffer module. Its unsafe calls require
explicit export/import safety arguments; native execution also uses reviewed
external FFI-bearing dependencies. Never commit secrets. Hosted-provider and operating-system
credential tests must be opt-in and must not run in portable CI.

Mom release storage resolves an explicitly configured
`LLAMA_NATIVE_KIT_STORE_KEY_HEX` or the supported macOS credential store.
Changing the data directory never authorizes a predictable test key. Debug
builds have a separate, deliberately insecure development-store mode; these
are not credential-store acceptance. Unsupported OS credential stores fail
closed unless an explicit key is configured. Wrong-key failures never generate
a replacement key.

Automatic migration from Mom's legacy plaintext JSON files is retired. Opening
a store with those files present reports the exact residual path and refuses
to proceed; the application does not claim to have encrypted or deleted them.
The reusable FTE response store similarly rejects unversioned and unsupported
schemas without migrating them. These are pre-release formats.

Information managed storage and FTE private database/token storage currently
require Unix permission and directory-synchronization support. Other platforms
receive an unsupported result before existing state is read or new state is
created. Portable protocol and in-memory database operations remain available.
A publication or removal already committed by rename is reported as such even
if subsequent hardening, cleanup, or synchronization fails. macOS is the current
product acceptance target; compilation on other platforms is not proof of their
privacy, durability, OS credentials, or packaged behavior.

MCP configuration records the executable digest. Every spawn checks it before
starting the process; a missing or changed digest requires reconfiguration.
Managed Persona commands additionally require staged direct native binaries.
Ordinary configured commands may use scripts or arguments: their interpreters,
libraries, and external inputs are not covered by the executable digest. Neither
path is a sandbox or an atomic guarantee about which inode the OS executes.
Any failure after a successful spawn retains outcome-unknown semantics.

Dependency CI scans both Rust locks and the JavaScript lock. The GLib 0.18
backport is documented in `vendor/glib/PATCH.md`; maintenance-only exceptions in
`deny.toml` have a 2026-12-09 review date. The pinned ort-sys 2.0.0-rc.13 source
selects ONNX Runtime 1.28.0 artifacts from its bundled target/hash table and
verifies the downloaded archive before publishing its extracted cache. It
trusts an already existing hash-named cache directory, which is local build
state, not authenticated runtime input. No new artifact downloader is needed.
