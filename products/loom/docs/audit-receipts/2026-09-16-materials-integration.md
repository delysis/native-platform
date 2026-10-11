# Materials, cabals and peer recovery integration

Functional source: `ddb314b8917f89e4621d6ca4d8e17617b916f263`, including
main `4aabe9adf9ec152d15c81c7576f4bcfe71b0e004` (PRs #52 and #55).
The materials integration preserves upstream's compact source navigation,
pane controls, retained-output pins and recorded search events. Cabal context
publication is reachable from the cabal pane and names its current document.

The merge required authority and recovery repairs:

- Explicit document references resolve visible inline media, excluding private
  scratch attachments. The existing QUIC publication test now exercises the new
  material resolver; the native-media regression also checks that resolver.
- Attachment links carry the originating document identity. A shared document
  cannot bind a private retained hash into a material. Opening a material original
  instead uses its exact registered material identity. Explicit file imports retain
  their separate existing binding path.
- Peer searches and budgeted consultations save immutable results before dispatch.
  Check reads existing results; it cannot perform a previously unreached lookup.
  Resume may continue only against the admitted source version.
- Every peer step freezes its private source evidence before its job is prepared.
  Cancellation can retain a completed job without replaying the expression or
  changing the result's provenance. Local model ownership and remote grants remain
  separate; peer evaluation does not require a local model.
- Pure references remain local derivations. A final material/evidence result has a
  separate idempotent output identity from model steps, preserving upstream's
  generated-versus-derived attribution.

Regression tests use actual retained stores and local QUIC with a labeled fixture
executor. They remove the source after the first peer result, reopen the requester,
Check without dispatching a later job, then Resume using the exact original
evidence. Cancellation retains the earlier result and search events. These tests
do not claim real-model inference or physical Internet acceptance.

Frontend validation passed 510 assertions, 172 WebKit browser assertions, Svelte
checking with zero errors/warnings, and the production build. Rust validation
covered 688 tests across the affected Loom packages, with 11 opt-in tests ignored.
This includes 319 native plugin tests and all cabal storage, history, transport,
restart, ownership and compute cases.
After main advanced with PR #55, all 380 adapter/app/plugin consumer tests passed
again at `ddb314b`, including the numeric-output provenance regression.
Strict all-target Clippy for the complete `product-loom` group, formatting and
repository policy also passed. Generated app/plugin permissions include both
material commands and the cabal/Signal/compute command sets.

The first combined run hit the compute test's five-second settlement timeout for
a 4 MiB opaque media fixture. No transport code or timeout was changed. The exact
case then passed in isolation (6.58 seconds total), and the exact previously
failing test executable passed all 11 compute cases (8.54 seconds total). The
cause of that transient timeout was not established. Previously passed history
stress tests were preserved; the remaining package tests then completed.

Local logs:

- `/tmp/loom-materials-integration-loom-gate.log`
- `/tmp/loom-materials-integration-media-recheck.log`
- `/tmp/loom-materials-integration-compute-recheck.log`
- `/tmp/loom-materials-integration-cabal-remaining.log`
- `/tmp/loom-materials-integration-remaining-gate.log`
- `/tmp/loom-materials-integration-upstream-consumers.log`
- `/tmp/loom-materials-integration-final-clippy.log`
- `/tmp/loom-materials-integration-policy.log`
- `/tmp/loom-materials-integration-frontend-check-3.log`
- `/tmp/loom-materials-integration-frontend-tests.log`
- `/tmp/loom-materials-integration-browser-tests.log`
- `/tmp/loom-materials-integration-frontend-build.log`

No native GUI interaction or real Signal account operation was performed for this
integration. The [packaged Signal candidate](2026-09-16-signal-history-release.md)
is separately bound to its earlier `9cad13b` source. Final packaged/native
acceptance and the remaining live checks in the networking acceptance map remain
open; PR #49 stays draft.
