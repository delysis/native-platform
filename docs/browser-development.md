# Browser development and bounded CI repair

Substantial research, design and implementation should begin in an ordinary
browser chat with repository access. Commit changes on a feature branch and
let the existing focused GitHub CI close the build/test loop. The private
[account controller](https://github.com/delysis/repo-maintenance) discovers this
repository automatically, prepares exact-source failure handoffs, and reserves
Codex allocation for one small, directly evidenced cleanup attempt per UTC day
across the account. It cannot merge its draft repair PRs.

Use the controller's [browser brief](https://github.com/delysis/repo-maintenance/blob/main/prompts/browser.md).
Its [adoption ledger](https://github.com/delysis/repo-maintenance/blob/main/docs/phil-adoption.md)
records the published Phil practices behind the guidance, pinned to the source
revision. These instructions are our adaptation, not a recovered Meredith
L. Patterson prompt. An indexed file is not a reviewed theorem.

For each consequential change, name the contract and observable acceptance
obligation. Keep the following evidence separate:

| Evidence | What it can establish |
|---|---|
| Specification or formal model | Properties under the model's explicit assumptions |
| Correspondence check | Agreement at the tested or proved representation boundary |
| Production binding | The real entry point uses the checked implementation |
| Component test or fixture | The exercised component behavior in that configuration |
| Packaged runtime interaction | The actual artifact works through the exercised user path |

Do not infer a later row from an earlier one. Native model generation, raw
output/provenance, cancellation, media authority and UI acceptance retain their
own product-owned obligations. A replacement backend must earn its own evidence;
sharing an interface does not transfer the previous backend's qualification.

Use a relevant successful case and a relevant rejecting case at a changed
boundary. Keep source SHA, target, features, tools and artifact identity with
the result. Label shims, simulations, missing real-model runs and outstanding
hardware interactions. A digest binds bytes; it does not prove correctness.

Keep authored-code warnings fatal. Any generated/vendor exception must be
narrow, documented and unable to suppress warnings in its consumers. When a
generated kernel matters, compare a fresh generation with the production bytes
and exercise the real handwritten adapter as well.

Keep fast affected-package checks as the default. A path filter must cover all
inputs to its obligation, including scripts, fixtures, generators and manifests.
Consolidate workflows only after demonstrating equivalent old/new coverage at
the same revision; preserve negative cases and separately attributable outcomes.
Do not alter test meaning while calling the change orchestration cleanup.

Batch related edits after focused local checks before pushing a new CI revision.
Reserve a manually dispatched full run for settled cross-platform qualification;
the ordinary PR, main and nightly workflows already provide their own coverage.
Automatic full runs cancel superseded pushes. A documentation-only main push
keeps policy and documentation checks and may skip native builds only when its
code matches the last successful main qualification. Missing history, unknown
inputs and mixed code/documentation changes retain full coverage. Nightly and
manual qualification always run the full suite.

Stable Rust jobs restore compatible dependency caches written by full main
runs, retaining Cargo's toolchain, target, environment and dependency keys.
PR jobs do not publish duplicate caches for each product and pull request.
Policy, fuzzing and the pinned OMP runtime keep separate cache families.
Linux's ignored-test inventory runs beside an already selected complete
workspace test build. Otherwise it retains its standalone job; macOS and Windows
retain their separate inventory jobs. The guarded listing and registry checks
are unchanged, and their failures remain visible in the owning job and step.
Execution limits include headroom above measured cold builds; they do not alter
individual test deadlines.

For releases, test the package being delivered through ordinary installation,
signing/quarantine handling and the real user-visible operation. Keep external
acceptance separate from CI. Formal verification beyond the existing proofs
requires a real design and implementation effort; Clippy is not a theorem prover.

Native-kit's current source is `crates/native`; frozen standalone repositories
remain provenance, not alternative destinations for new fixes. The controller
does not replace this repository's test plan or the owning product contracts.
