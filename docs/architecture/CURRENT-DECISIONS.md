# Current architecture decisions

This index governs new work. The dated [ADR bodies](adr/) preserve the original
decisions; their import checksum is historical provenance, not a live build seal.
“Current” describes the intended boundary and implementation, not test acceptance.

| Record | Status and current decision |
| --- | --- |
| [ADR-001](adr/ADR-001-first-party-monorepo.md) | Current. One first-party Cargo/pnpm workspace, lockfiles, and [package catalogue](../../ci/package-groups.json). Migration ledgers are archives. |
| [ADR-002](adr/ADR-002-owner-handle-and-shutdown-hierarchy.md) | Current with implementation-specific owners. Product composition roots own native/service hosts and worker joins. Mom passes explicit operation scopes; there is no process-global native host lookup. |
| [ADR-003](adr/ADR-003-operation-lifecycle-v2.md) | Current with per-operation arbitration. Tests live beside each actual host and adapter, not in a universal conformance implementation. Cancellation requests, terminal results, task panics, and completed joins remain distinct. Loom may abort close and resume admission; irreversible commits retain their result even if cancellation follows. |
| [ADR-004](adr/ADR-004-final-result-versus-progress.md) | Current. Final results are authoritative. Lost progress cannot become a successful complete transcript; HTTP projectors preserve final output identities or report unsupported/incomplete output. |
| [ADR-005](adr/ADR-005-evidence-tiers-and-claim-naming.md) | Superseded as a mandatory tier system. Report the actual result, source/runtime identity needed to interpret it, checks run, and limits. Add runtime state types only when a caller uses them. Persisted records never recreate live authority. |
| [ADR-006](adr/ADR-006-artifact-publication-outcomes.md) | Current. Visibility and durability are different outcomes. Errors after rename preserve committed identity and permit exact retry. Unsupported synchronization cannot report durable success. |
| [ADR-007](adr/ADR-007-one-modern-gateway.md) | Current for FTE. Mom-through-FTE is superseded: Mom composes native inference directly and has no hosted provider or loopback dependency. |
| [ADR-008](adr/ADR-008-product-store-separation.md) | Current separation; speculative upgrade machinery retired. Stores accept fresh/current identities and reject incompatible data before mutation. Loom uses one current schema, without research tables or migration replay. |
| [ADR-009](adr/ADR-009-speech-sibling-service.md) | Current. Speech is a sibling service with separately owned admission, cancellation, task supervision, and platform qualification. |
| [ADR-010](adr/ADR-010-loom-writing-and-research-modes.md) | Superseded research architecture. Loom owns writing, readable manuscripts, provenance, and explicit revision-bound promotion. The research engine and its unused database schema are retired. |
| [ADR-011](adr/ADR-011-frontier-diagnostic-quarantine.md) | Historical. Retired research/critic code is not a production dependency or an instruction to restore it. |
| [ADR-012](adr/ADR-012-rust-baseline-and-resolver.md) | Current. [Rust 1.92.0](../../rust-toolchain.toml), edition 2024, resolver 3, workspace lint inheritance. The state-buffer import is the narrow documented unsafe exception. |
| [ADR-013](adr/ADR-013-credential-storage.md) | Current with explicit modes. Mom release uses the configured key or supported macOS credentials; debug/test keys never become release authorization through a path override. Wrong keys do not rotate themselves. See [SECURITY.md](../../SECURITY.md). |
| [ADR-014](adr/ADR-014-component-release-and-versioning.md) | Direction, not an existing-user compatibility promise. These products are unreleased. Review source and consumers together; do not add migration frameworks without a real use. |
| [ADR-015](adr/ADR-015-local-first-macos-release-candidates.md) | Current qualification target. Packaged interaction, credentials, hardware, quit/join, and reopen need their own observed results on the tested build. Compilation and fixture tests do not establish them. |

See [CI policy](../CI-POLICY.md) for executable checks and the owning product
contracts for behavior. Keep historical receipts attached to their original
revision rather than treating them as acceptance of later changes.
