# ADR-004: Final result versus progress

## Status

Accepted 2026-08-12. Set-level systems-steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

Streaming generation, speech, acquisition, and research operations need responsive progress without allowing a slow or absent progress consumer to block completion or process shutdown. Earlier implementations could blur a dropped or full progress channel with operation failure, duplicate terminal facts across channels, or publish a terminal-looking event that disagreed with the authoritative result.

Phase-one lifecycle evidence establishes that terminal accounting belongs to the executor and that consumer drop is cancellation-only. Progress therefore cannot be the authoritative completion mechanism.

## Decision

The final result is authoritative. Progress is observational.

Progress channels are bounded. A service contract may coalesce or drop nonterminal observations. Unread progress must never prevent final publication, terminal accounting, cancellation, quiescence, or shutdown.

Terminal delivery uses either reserved capacity or a separate final channel. A terminal progress event, when exposed, and the final result must be projections of one immutable terminal object; they may not be independently constructed.

Progress-channel closure is not by itself operation failure. Final-channel closure without a terminal object is an explicit protocol/worker failure. Consumer ticket drop requests cancellation under ADR-003 but does not release the operation or invent a terminal result.

Waiter timeout is also observational: it returns continued control of the ticket or handle and does not mark the worker failed or the operation terminal.

## Alternatives

### Unbounded progress channels

Rejected. Memory use can grow with output rate and historical requests.

### Backpressure all progress

Rejected. A stalled UI or observer could prevent authoritative completion and shutdown.

### Encode completion only as the final progress event

Rejected. A full, dropped, or closed observational channel would erase the authoritative result.

### Construct terminal event and final result separately

Rejected. The two representations can disagree on status, error, usage, or evidence.

### Treat waiter timeout as failure

Rejected. A caller's patience does not determine worker state.

## Migration

1. Identify every progress, event, stream, and final channel in native, Gateway, Speech, Information, Mom, and Loom operations.
2. Define one terminal object per operation and derive all terminal projections from it.
3. Bound progress queues and document each service's coalescing or drop policy.
4. Add or preserve an independent final-delivery path.
5. Make final publication attempt precede executor lease release.
6. Convert waiter timeouts to return the live ticket or handle.
7. Remove legacy duplicated terminal construction once parity tests pass; no general compatibility promise is required.

## Rollback

Rollback uses the last phase-one implementation that preserves authoritative final delivery and bounded progress. If a migration causes a terminal mismatch, blocked shutdown, or unbounded progress growth, disable that adapter and retain the prior implementation. Never roll back to progress-only terminal authority.

## Acceptance

- Progress storage and channels are bounded.
- Nonterminal drop/coalescing behavior is documented per service.
- Unread progress cannot block final delivery or successful shutdown.
- Exactly one terminal object determines final state.
- Terminal event and final result cannot disagree.
- Ticket drop and progress-channel closure do not prematurely release identity.
- Waiter timeout leaves the operation's true state unchanged.
- Race tests cover progress saturation with completion, cancellation, panic, and quiescence.

## Consequences

UIs may miss intermediate observations under pressure, but they always have a distinct authoritative completion path. Services must state progress-loss semantics and maintain a terminal object, yielding predictable memory use and shutdown behavior.
