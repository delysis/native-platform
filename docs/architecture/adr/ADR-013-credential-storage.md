# ADR-013: Credential storage

## Status

Accepted 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

FTE previously had duplicate routing/storage paths and a legacy plaintext credential table. Phase-one R7 replaced production routing with one Gateway and proved a disposable macOS Keychain migration: write a synthetic value, read back its exact hash, remove the plaintext table, exit normally, and delete the disposable item without recording secret bytes. Mom separately proved its canonical Keychain-backed encrypted store and same-store recovery without a test key or plaintext bypass.

Provider metadata, usage, and product state have different confidentiality and transaction needs from provider credentials. Storing a secret in an application database, log, receipt, loopback response, renderer state, command line, or environment snapshot expands the disclosure boundary. Live paid-provider execution is not needed to validate storage correctness.

## Decision

Production credentials are stored only through an injected operating-system credential service:

- macOS Keychain;
- Windows Credential Manager/DPAPI-backed credential storage;
- Linux Secret Service through a supported locked-session provider.

The application database retains only nonsecret provider identity, configuration, capability, usage, and a stable credential reference. Secret resolution occurs at the trusted host/backend boundary and returns a scoped, zeroizable value that is neither serializable nor cloneable by default. Renderers, loopback clients, diagnostics, and receipts may observe presence/status but never the secret.

Legacy migration is transactional in outcome:

```text
read legacy secret
-> write OS store
-> read exact bytes back
-> mark the stable reference migrated
-> delete the legacy secret
-> retain nonsecret metadata and a hash-only migration receipt
```

Failure before verified readback leaves the legacy source recoverable. Failure after OS-store verification reports an explicit committed/migration-uncertain outcome and is idempotently recoverable. The application must never keep two writable credential authorities in steady state.

## Alternatives

1. **Keep secrets in SQLite, including an encrypted column.** Rejected because application-managed keys and database copies retain a larger theft and logging surface and duplicate OS credential policy.
2. **Use environment variables or config files.** Rejected for persistent product credentials because process inheritance, diagnostics, shell history, and file backups broaden exposure.
3. **Maintain OS and legacy stores as writable peers.** Rejected because conflict resolution can resurrect deleted or rotated secrets.
4. **Require live provider calls to prove migration.** Rejected because exact OS-store write/readback and protocol fixtures establish the storage contract without paid credentials or network spend.

## Migration

1. Inventory every credential field, resolver, renderer command, log path, loopback response, and backup/export path.
2. Introduce the injected credential interface and platform implementations with fake-store contract tests.
3. Migrate one provider reference at a time using exact write/readback-before-delete semantics and crash injection at every boundary.
4. Remove plaintext columns/tables and legacy write commands only after recovery tests pass.
5. Scrub renderer DTOs, receipts, diagnostics, support bundles, and loopback schemas so they carry presence and stable references only.
6. Preserve only an encrypted pre-migration credential backup until the
   recovery window closes. Access restriction alone is insufficient. Record
   the ciphertext identity, key custody, and disposal decision without
   recording plaintext secrets.

There are no users requiring generic compatibility with the old credential schema. Recovery of existing data and proof that no credential is silently lost remain mandatory.

## Rollback

Before legacy deletion, return to the prior component tag while leaving the verified OS item intact and preventing dual writes. After deletion, restore the protected backup into an import-only recovery tool, verify the OS item or rewrite it, then delete the recovery copy. Never export credentials into ordinary JSON, logs, or the release manifest. A product rollback may restore binaries and nonsecret databases, but credential ownership remains with the OS store.

## Acceptance

- Platform contract tests cover create, exact readback, update, delete, locked/unavailable store, duplicate reference, and cancellation.
- Crash-injection tests cover every migration boundary and prove idempotent recovery without dual writable authority.
- Source/schema scans find no production plaintext credential table or file path.
- Renderer, loopback, log, diagnostic, receipt, and decrypted-export tests prove
  plaintext secret bytes are absent. Encrypted recovery ciphertext is allowed
  only under the bounded backup protocol above and is tested for authenticated
  encryption, access control, restoration, and disposal.
- At least one real OS-store migration/readback and cleanup receipt exists per supported release platform before claiming that platform's distribution support.
- Hosted protocol behavior is fixture-tested; acceptance requires no live paid-provider spend.
- The W1 ADR set-level steward receipt reviews platform scope, recovery, and redaction boundaries.

## Consequences

Credential handling becomes platform-specific and can fail when the user's credential service is locked or unavailable; products must expose truthful recoverable status. Backups no longer contain self-sufficient provider credentials. The trusted host boundary becomes smaller, and secret rotation/deletion has one authoritative implementation.
