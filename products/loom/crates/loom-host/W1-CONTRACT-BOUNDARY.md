# W1 contract boundary audit

This crate's `unstable-w1-contract-tests` feature tests only facts currently
owned by Loom production types. It is not full `OperationModelAdapter`
conformance and therefore does not satisfy the W1 lifecycle gate by itself.

## Bound production facts

`GenerationRegistry` owns and exposes:

- bounded family and branch reservation;
- rejection of duplicate live request, run, and branch identities;
- project/session-bound cancellation routing, including cancellation retained
  before a concrete backend handle is attached;
- active route and branch counts;
- bounded session-idle observation;
- terminal-persistence failure visibility; and
- release of a family after its owner reports durable terminal persistence.

`BuildModelPolicy` is a closed allow-list whose real policies all declare
`InferenceBoundary::LocalOnly` and `HostedFallback::Forbidden`. The test-only
adapter maps those declarations to the W1 local-only privacy envelope. It does
not perform a hosted call, carry credentials, or mint network authority.

## Facts missing from one honest adapter

The full testkit cannot currently be implemented without inventing state:

- `GenerationRegistry` does not own Reserved -> Queued -> Running -> Terminal
  transitions. `CampaignJournal` owns Reserved -> Dispatched -> Finished ->
  Released for research attempts, while interactive generation terminal state
  is persisted elsewhere.
- `GenerationRegistry` retains cancellation routes, but a consumer ticket with
  drop-triggered cancellation and a separate cloneable executor lease are not
  exposed as one production operation abstraction.
- No one production owner exposes bounded progress projection together with
  the authoritative terminal and final projection required by
  `OperationSnapshot`.
- `GenerationRegistry::wait_for_session_idle` is observational and bounded,
  but the waiting control ticket required by the shared trait is owned by a
  different layer.
- Application quiescence and joined-worker evidence live in
  `tauri-plugin-loom`; they are not owned by `GenerationRegistry` or
  `CampaignJournal`, and the current shutdown proof is not exposed through a
  constructible cross-platform test adapter.
- `CampaignJournal` has durable attempt identity and a single event chain, but
  its diagnostic constructor is private to that crate and its real constructor
  requires an exclusive store session lease and a complete frozen campaign.
  Even if constructed, it would not supply progress, task retention, or joined
  worker counts.
- No current production API admits an arbitrary operation ID with deterministic
  sequence-exhaustion control, as required by `TestConfig`.

The W1 gate therefore needs a product refactor that gives one real Loom owner
the missing lifecycle, progress, quiescence, and join facts (or an accepted
revision of the shared contract that composes independently owned proofs).
Adding a test-only state machine here would be a shadow implementation and is
explicitly out of scope.
