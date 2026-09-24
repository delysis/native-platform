# CI policy

Use focused package tests while changing code, then one combined workspace gate
when the patch settles. Formatting, source inspection, and a test listing do
not establish runtime behavior. A fixture-backed test establishes only the
boundary it actually exercises.

## Local macOS development gate

While GitHub Actions cannot start steps because the account has no credits, use
`cargo run --locked -p xtask -- local-ci <new-evidence-directory>` from a clean,
complete checkout. Follow CONTRIBUTING.md for pinned **actual** Rust 1.92.0
binaries, Node 22, cached pnpm 11.16.0, WebKit and native-tool prerequisites.
This replaces the hosted development-validation dependency, not hosted check
statuses, branch protection, merge authorization or cross-platform qualification.
The workflows below remain unchanged; unavailable jobs are not reported as green.
Do not dispatch paid CI or merge unqualified stacked/EASL drafts.

The runner always uses the full workspace, not a new path/package selector. In
sequence it runs pinned formatting; xtask policy; all `scripts/ci/test-*.mjs`
Node tests with file concurrency one; current-document validation and the SQLite
graph; workspace all-target tests with `--no-fail-fast`; doctests; strict workspace
Clippy; the existing guarded `validate-ignored-tests.mjs --cargo-list`; frozen
frontend dependencies and the existing FTE/Mom frontend-only checks/tests and
Loom tests/check/build; the WebKit suite; and `xtask macos-smoke-support`.
The last command owns Swift source discovery, compilation/linking and its existing
self-test selectors. There is no duplicate ignored-test, package-group or Swift
self-test selector, and no implicit packaged-product acceptance.

Unlike hosted setup steps, local-ci installs no global tools or browsers. Its
frontend package-manager operations use offline caches. Locked Cargo builds retain
normal native dependency resolution; forcing Cargo offline disables the pinned
ONNX build script's cache lookup as well as downloads and prevents linking.
Explicit setup outside the runner is documented in CONTRIBUTING.md. Workspace
all-target tests already include xtask's tests; there is no redundant xtask test
build in the final gate. Filtered frontend commands add `--fail-if-no-match`, so
absent packages cannot produce an empty success. Commands run sequentially and stop at the first failure;
remaining entries in `plan.json` are unexecuted, not passing or excused. Existing
individual test deadlines and the guarded inventory implementation are unchanged.

The ignored-test guard rejects compiler/runner/loader/flag overrides and Cargo
configuration that changes its harness boundary. The runner reuses its exported
prerequisite check, not a second implementation. It first resolves actual Rust
binaries with `rustup which`, verifies their versions and inherited compiler paths,
and prepends that toolchain's bin directory to child PATH. It verifies that
`CARGO_TARGET_DIR` and checkout/target resolve to the same cache before removing
Cargo-injected `CARGO`, `RUSTC`, `RUSTDOC` and the target override for Node/guard
children. Cargo gate commands receive the resolved paths explicitly. Cargo also
adds a macOS launcher `DYLD_FALLBACK_LIBRARY_PATH`; only shared-target paths, pinned
sysroot library paths and Cargo's standard macOS fallback locations are accepted.
That variable is recorded and removed for child commands; unexpected entries fail.
Other prohibited overrides are not silently discarded. A different physical target
fails preflight; the runner never rebuilds into a secret second cache or changes
the guard to accept it. This also matters when running the guarded Node tests by
hand from a shell that exports these variables.

### Local receipts and interruption

The evidence leaf must not exist and must be outside the checkout and build
cache, including through symlinked parents. The runner never overwrites an old
receipt. It creates `run.json` marked incomplete, source/tool records and a fixed command plan. Each
runner-started command has numbered `command.json`, `stdout.log`, `stderr.log` and,
on completion, `result.json` with elapsed time, exit status or launch/wait failure.
Nested work remains owned by existing scripts/helpers: its output is captured in
the parent step's streams, not reimplemented as another command inventory.
Streams go directly to files, not an unbounded in-memory buffer. Only explicit
child environment changes are recorded; inherited credentials are not dumped.

Initial and final Git checks retain exact HEAD/tree and raw status in both command
logs and `source-start.json`/`source-end.json`, reject staged,
unstaged/untracked changes and sparse/assume-unchanged index entries, and run again
after an ordinary command failure. Only the final published `summary.json` with
`status: component-pass`, a complete gate and clean unchanged source qualifies
this run. A known failure produces `status: failed`; an interruption, missing or
truncated terminal record remains incomplete. Partial successes never qualify the
whole gate. A changed target alias also invalidates the run. Keep failed logs and
a stopped process's lock for diagnosis; inspect all descendants before clearing
a stale lock. Bootstrap compilation failures occur
before a runner receipt exists and need their own retained command/output/status.

These are local observations, not signed attestations, a hermetic build, a sandbox
or a filesystem transaction. Boundary checks cannot detect an edit-and-restore
between observations, and a cooperative lock cannot prevent unrelated Cargo
commands. The single integrator must hold source/cache ownership for the whole
run. Cached tools, dependencies and repository tests/build scripts remain trusted.
No credentials are acquired and no product state is reset by the runner.

### What a local component pass does not establish

No packaged/native UI, loaded-model or hardware acceptance is claimed. Native
focus and completion reversal failures are not repaired by a component pass.
Run native journeys separately through the existing `release-macos.sh` and
`smoke-macos-app.sh`, retaining their exact artifact and interaction receipts.
The runner invokes neither packaging nor those journeys. Linux/Windows, the
optimized Linux GLib test, model-integration, fuzzing, dependency/advisory/license
audits, signing and hosted checks retain their separate obligations. macOS-local
strict Clippy does not replace checks of platform-specific code elsewhere.

## Pull requests

The PR planner uses locked Cargo metadata and reverse dependencies to select
changed packages and their consumers. Asset and platform rules live in
`ci/ci-path-exceptions.json`; missing metadata or unknown paths select full
coverage. There is one selection algorithm; obsolete path overlays and shadow
comparisons are retired. Selection is not runtime evidence.

Normal tests require no model downloads, account credentials, or hosted-provider
access. Opt-in tests are indexed in `ci/ignored-tests.json`. The registry binds
package, exact target and test ID, prerequisites, and supported platforms.
`validate-ignored-tests.mjs --cargo-list` reconciles compiled harness listings;
listing establishes inventory only. The source/build-script guards constrain
that listing operation, not trusted compiler or native-toolchain behavior.

## Full CI

`ci-full.yml` runs the workspace once on each of Linux, macOS, and Windows:
all targets and doctests, with strict Clippy on Linux. This covers the products
and services without rebuilding them in redundant product/group jobs. macOS
also runs the WebKit browser suite and packages Loom and FTE using that job's
existing Rust target directory. The separate frontend job runs only frontend
commands; it does not recursively invoke product Rust build scripts.
The macOS job and selected release-tooling lane compile and link the platform
programs in `scripts/macos-smoke-support` through `xtask macos-smoke-support`.
The smoke runner reuses those compiled helpers throughout its isolated run;
shell syntax checks are not substituted for checking the platform source.

The `model-integration` job downloads one immutable Qwen3 0.6B CPU fixture and
verifies its SHA-256. `cargo run --locked -p xtask -- model-check MODEL SHA256
PACKAGE TEST_ID ...` lists the exact registered test and requires one executed
passing test, with no ignored or filtered-zero substitute. The runner resolves
Rust 1.92 rustc and rustdoc explicitly. Its selected saved-prefix and strict
pre-cancellation checks do not establish Metal, other model families, operating
system credentials, or a packaged user journey.

The Attachment fuzz job executes `inspect` and `pipeline` with 60 seconds per
target, a ten-second input timeout, and a 2 GiB memory limit. Crashing inputs are
uploaded on failure. It uses the independent, locked fuzz workspace. A bounded
fuzz run is not proof that arbitrary inputs are safe.

`ci-required` gates pull requests on the planner, fast policy/frontend checks,
and selected macOS jobs. Linux, Windows, fuzzing, and cross-platform inventory
remain scheduled and report failures. They start after `ci-required` finishes,
so they cannot consume runner capacity ahead of the same run's development
gate. Other workflow runs can still share runner capacity. The
planner's existing selection still applies; a failed gate does not suppress
selected advisory checks. Cancelled or superseded workflow runs start no more
advisory work.

`macos-required` reports main's macOS acceptance independently of other platforms.
Each full CI run starts macOS, frontend, and policy checks first. Linux/Windows workspace,
fuzz, and model-integration work waits for those checks and the macOS report to
finish, then runs even if they failed, unless the workflow was cancelled.
`full-summary` remains a complete report: any failed, cancelled, or unexpectedly
skipped job makes it fail. It is advisory for macOS development. Repair failures
in scoped follow-up changes; never make a failing test silently pass or remove
coverage to obtain a green report.

## Dependencies and platform qualification

`dependencies.yml` runs daily and for relevant manifest, lock, policy, or vendor
changes. Hash-pinned cargo-deny checks both Rust locks for advisories, licenses,
and permitted sources; pnpm checks the actual JavaScript lock. Maintenance-only
exceptions name their reason and review date in `deny.toml`. GLib's compatible
security backport has a source/patch record in `vendor/glib/PATCH.md`; its real
iterator tests run with optimization on Linux.

There is one first-party Cargo workspace and root lock plus the independent
Attachment fuzz workspace/lock. The external GLib patch is not a first-party
workspace member. External llama bindings use an exact Git revision. Historical
migration receipts and seals do not gate ordinary changes.

macOS remains the product-acceptance target. Linux and Windows checks establish
only their executed capabilities. Unsupported private Information, Loom project, and
FTE database/token storage return errors before reading or creating state;
non-Unix tests assert that boundary, while portable protocol/schema tests remain
enabled. No platform is certified by compiling it. Signing, OS credentials,
loaded-model shutdown, and visible packaged interactions require their own
current evidence.
