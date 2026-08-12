# Compositional lifecycle conformance

The original `OperationModelAdapter` remains the strongest single-owner test
surface. The compositional surface permits a product to bind different real
components to different ADR-003 obligations without adding product state to a
test adapter.

| Suite | Normative proof | May be a component-local adapter? |
| --- | --- | --- |
| transition chain | exact Reserved -> Queued -> Running -> Terminal -> Released | yes |
| registry identity | duplicate rejection, generation-safe stale release, checked sequence exhaustion | yes |
| attempt hierarchy | two concurrent attempt identities under one still-active public operation; status and cancellation remain operation-anchored | yes |
| consumer cancellation | ticket drop and explicit drop request cancellation while executor identity remains | yes |
| terminal authority | one terminal/final projection and cancel/complete linearization | yes |
| waiter control | timeout is observational and control remains usable | yes |
| admission/quiesce/shutdown | admission race linearizes with quiescence; late admission closes; a nonblocking shutdown witness cannot complete before active release and join | bridge |
| progress/shutdown | bounded unread progress cannot block terminal/release/shutdown; terminal wins concurrent progress | bridge |
| panic/shutdown | panic becomes failed terminal and shutdown empties ownership | bridge |
| stable shutdown | repeated shutdown is deterministic and empty | yes |
| task reaping | retained task state returns to zero after each historical operation | yes |

Each adapter binds a `LifecycleImplementation` marker with fixed product and
implementation constants. Successful runners derive typed `CoverageEvidence`
from that associated type; ordinary callers provide only a component name and
cannot accidentally relabel reference evidence as product evidence.
`LifecycleCoverageManifest<I>` accepts only `CoverageEvidence<I>`, requires
each of the eleven suite names exactly once, and requires the complete
invariant union. A passing local suite is not an implementation acceptance
statement until that manifest accepts. A deliberately dishonest adapter can
still select a product marker while delegating to reference state, so source
review remains an explicit acceptance boundary.

Composition does not prove that an adapter is free of shadow state; code review
must still confirm every method reads or exercises the named production owner.
The API makes omissions and cross-boundary gaps visible, prevents partial suite
sets from becoming acceptance, and avoids demanding that one component expose
facts it does not own.

Every shutdown fact carries both `expected_workers` and `joined_workers`.
Successful closed/empty assertions require equality rather than assuming a
single worker, so a product may truthfully bind a fixed or dynamic multi-worker
owner. Bridge suites additionally require exact expected and joined worker-ID
sets after exercised work. Review must bind those IDs and scalar facts to the
production registry, retained task handles, and actual join results.
