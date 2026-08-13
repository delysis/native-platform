# R4/R5 store schema reconciliation

Date: 2026-08-11

## Canonical order

R4 and R5 originally proposed the same numeric migration slot in independent
worktrees. The combined lineage resolves that collision additively:

1. schema 9 is the research execution ledger baseline;
2. migration `0010_token_piece_evidence.sql` adds immutable exact token-piece
   evidence for R4;
3. migration `0011_foreground_command_receipts.sql` adds the nonce-free,
   immutable foreground-command audit receipt for R5;
4. `CURRENT_STORE_SCHEMA_VERSION` is 11.

No R4 table, trigger, or migration ledger row is rewritten by R5. A database
at version 9 executes 10 and then 11 in one migration transaction. Reopening a
version-11 database is idempotent. Future-version refusal remains unchanged.

## Behavioral overlap resolution

- R4 owns exact generated token-piece bytes and boundaries.
- R5 owns one-use foreground command authority and the final research
  promotion transaction.
- The R5 transaction validates the exact live subject lease and durable
  pending request, consumes move-only host authority, and commits the derived
  foreground receipt, manuscript revision, provenance operation, ordinary
  command receipt, and visible-file outbox row together.
- Native focus listeners are installed before initial sampling; confirmation
  rechecks native focus at the consume edge.
- Project and application close revoke pending authority before worker/model
  drain, and both host and plugin pending maps are bounded.

## Combined regressions

The combined tests cover the version-9 to version-10 to version-11 path,
version-11 reopen, immutable receipt shape, absence of persisted nonce,
restart/replay rejection, native-focus recheck, bounded registries, and a
production command path that commits and projects the selected manuscript
revision. Full workspace and frontend gates are recorded in the R5 audit
receipt only after they have actually completed.
