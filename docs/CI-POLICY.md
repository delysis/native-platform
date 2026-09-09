# CI policy

Use focused package tests while changing code, then one combined workspace gate
when the patch settles. Formatting, source inspection, and a test listing do
not establish runtime behavior. A fixture-backed test establishes only the
boundary it actually exercises.

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

Every job listed by `full-summary` must succeed. Failure, cancellation, or an
unexpected skipped job fails the aggregate. Product absence is not an excuse
to skip a declared workspace member.

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
