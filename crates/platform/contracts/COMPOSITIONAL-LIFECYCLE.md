# Compositional lifecycle conformance

The original `OperationModelAdapter` remains the strongest single-owner test
surface. The compositional surface permits a product to bind different real
components to different ADR-003 obligations without adding product state to a
test adapter.

| Suite | Normative proof | May be a component-local adapter? |
| --- | --- | --- |
| transition chain | exact Reserved -> Queued -> Running -> Terminal -> Released | yes |
| registry identity | duplicate rejection, distinct attempts under one public identity, generation-safe stale release, checked sequence exhaustion | yes |
| consumer cancellation | ticket drop and explicit drop request cancellation while executor identity remains | yes |
| terminal authority | one terminal/final projection and cancel/complete linearization | yes |
| waiter control | timeout is observational and control remains usable | yes |
| admission/quiesce/shutdown | admission race linearizes with quiescence; late admission closes; shutdown waits for active release and join | bridge |
| progress/shutdown | bounded unread progress cannot block terminal/release/shutdown; terminal wins concurrent progress | bridge |
| panic/shutdown | panic becomes failed terminal and shutdown empties ownership | bridge |
| stable shutdown | repeated shutdown is deterministic and empty | yes |
| task reaping | retained task state returns to zero after each historical operation | yes |

Each successful runner returns `CoverageEvidence`, whose constructor is private
to the testkit. `LifecycleCoverageManifest::accept` requires one product and
one lifecycle-implementation identity on all evidence, plus the complete
invariant union. A passing local suite is not an implementation acceptance
statement until that manifest accepts. Evidence for two independent
implementations inside one product cannot be combined.

Composition does not prove that an adapter is free of shadow state; code review
must still confirm every method reads or exercises the named production owner.
The API makes omissions and cross-boundary gaps visible, prevents partial suite
sets from becoming acceptance, and avoids demanding that one component expose
facts it does not own.

Every shutdown fact carries both `expected_workers` and `joined_workers`.
Successful closed/empty assertions require equality rather than assuming a
single worker, so a product may truthfully bind a fixed or dynamic multi-worker
owner.
