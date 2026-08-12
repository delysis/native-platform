# Wave 1 vertical fixture protocol

This additive protocol freezes observable behavior before production source is
moved. It does not revise or supersede the accepted
`w1-contracts-v0-2026-08-12-r3` contract source. It is test-only data and pure
validation, not a product runtime and not an acceptance authority.

## Ownership boundary

Product repositories own fixture inputs, execution, production adapters, and
the projection from production facts. This repository owns only:

- closed identifiers for the eighteen migration-plan section-16 rows;
- versioned manifest, observation, and lock envelopes;
- byte identity and semantic validation;
- exact before/after projection comparison.

The validator receives bytes and observations from its caller. It cannot read a
file, execute a replay recipe, open a network connection, inspect a platform,
load a model, access a Keychain, or mint a capability or operation lease.
Source review must prove that each product projection comes from the named
production owner rather than fixture-owned shadow state.

## Freeze sequence

1. Merge and tag the protocol/schema revision. Product test dependencies pin
   that exact commit; the accepted r3 lifecycle-contract pin need not move.
2. Add product-owned fixture cases and expected projections. Each case records
   the production baseline commit and a SHA-256 production-tree identity. A
   test-only descendant must prove that production paths did not change.
3. Review and merge every product fixture. Preserve rejected, unavailable, and
   superseded evidence separately.
4. Create `verticals/v0/W1-VERTICALS.lock.json` only after all rows exist. The
   lock names all eighteen manifest byte identities exactly once. Seal it in a
   later central commit and steward receipt.

This two-revision sequence avoids a manifest referring circularly to the commit
that contains that manifest. This protocol PR deliberately contains no final
row manifests or lock.

## Exact row catalogue

| Class | Rows |
| --- | --- |
| Model-free (8) | Mom chat/cancel/retry; Mom attachment; FTE hosted fixture/loopback; Speech peer cancellation; Information install/query; Loom suggestion/promotion; Loom research diagnostic/admitted distinction; quit/relaunch using fake owners |
| Real (4) | current exact Qwen; current exact Gemma; current Parakeet model/audio; Apple installed voice |
| State (6) | Mom prior release store; Loom prior project store; FTE legacy database; Information resource store; corrupted disposable caches; partial publication states |

One row may contain multiple repository-owned cases. Cross-product rows such
as quit/relaunch and corrupted caches therefore remain one locked requirement
without creating a cross-repository executable before W2.

## Replay and equivalence

A replay recipe is inert data: a closed program kind, argv values, names of
required environment variables, and either a denied or loopback-only network
boundary. It is never a shell command. Environment values are absent, and
secret- or credential-shaped names are rejected.

`validate_baseline` requires the baseline source revision and production-tree
digest, authenticates the expected projection bytes, and compares the observed
projection exactly. `compare_candidate` permits a later implementation
revision but requires the same observable projection. The projection includes
ordered events, durable-state effects, terminal and release facts, worker
ownership, invariant-level output facts, and fail-closed facts. Volatile time,
temporary paths, and nondeterministic prose or audio bytes do not belong in the
projection.

Passing comparison is not acceptance. Negative evidence remains reviewable but
cannot pass baseline validation. Only a later steward receipt can accept
`W1-VERTICALS` and authorize W2.

## Explicit non-requirements

The FTE hosted fixture covers provider-independent chat and raw-completion
protocol behavior with deterministic data and loopback-only lifecycle. It
requires no Cerebras credential and makes no live hosted-provider claim.

The Mom attachment fixture is an ordinary deterministic import, bounded
inspection, prompt-projection, commit, and reopen replay. Scheduling and
calendar execution are not protocol concepts.

The Loom suggestion projection requires one visible caret-local ghost. Tab
promotes only that visible ghost at the exact source/caret boundary; otherwise
Tab performs ordinary editor tab or indentation behavior. Additional
candidates remain hidden until explicit review. Persistent candidate-count
chrome, a `Skip to manuscript` control, and a primary `Use this` control are
forbidden. The ghost and exact-boundary Tab behavior are the ordinary
autocomplete interaction.
