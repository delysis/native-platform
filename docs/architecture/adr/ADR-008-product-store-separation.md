# ADR-008: Product store separation

## Status

Accepted — 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

Mom, Loom, FTE, and Information persist materially different objects under different privacy and transaction rules: Mom owns encrypted conversations, personas, tools, and permissions; Loom owns projects, revisions, provenance, and research evidence; FTE owns provider metadata, route configuration, usage, and response state; Information owns resource catalog, source, and index state. A universal database would couple migrations, permissions, backup, corruption, and release cadence without creating a real shared transaction boundary.

There are no existing users requiring general schema compatibility. Existing durable product data, exact provenance, secret handling, and recoverability must nevertheless survive deliberate migrations.

## Decision

Keep independent product stores and schemas for Mom, Loom, FTE, and Information. No platform or service core may read or mutate a product store directly. Products persist references to immutable artifacts and services expose typed operations/results.

Standardize only storage practices and envelopes: versioned migrations and exports, strict tables where available, foreign keys, bounded reads, idempotent commands, explicit commit receipts, corruption quarantine, and backup/recovery harnesses. Hosted secrets are not product database rows; they use injected OS-backed credential storage. Preserve content digest, occurrence ID, operation ID, revision ID, and selection ID as distinct identities.

## Alternatives

- One global SQLite database: rejected because domain, privacy, and transaction boundaries differ.
- One schema per product in one physical database: rejected because it still couples lifecycle, backup, corruption, and release.
- Shared ORM entities across products: rejected because apparent reuse would leak domain authority.
- Preserve every legacy schema indefinitely: rejected because there are no users requiring that cost; only evidence-bearing and recoverable data needs controlled migration.

## Migration

1. Record each current schema version and produce versioned state fixtures and exports.
2. Define ownership for every durable object and secret.
3. Introduce common migration, receipt, quarantine, and recovery contracts without merging databases.
4. Move cross-product relationships to stable typed IDs and immutable artifact references.
5. During cutover, permit exactly one writable store for each object class; verify exported data before retiring the old writer.

## Rollback

Restore the prior product-specific store from a versioned backup/export and revert that product's adapter. Never operate old and new stores as writable peers. Preserve migration receipts, rejected/corrupt rows, and provenance even when the application schema is rolled back.

## Acceptance

- A ledger assigns every durable object and secret to exactly one authority.
- Each product has schema-version, export/import, backup, corruption, and recovery tests.
- Cross-product services cannot open product databases by architecture test.
- Migration fixtures preserve exact user-created content, artifact identity, occurrence history, and provenance.
- Secrets are absent from ordinary product database exports.

## Consequences

The monorepo will contain multiple stores and migration streams. This is deliberate bounded-context separation, not duplication. Shared operational quality improves through common contracts while product privacy, recovery, and release boundaries remain independently testable.
