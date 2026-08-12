# W1 native lifecycle ownership

## Scope

- Repository: `delysis/llama-native-kit`
- Candidate branch: `codex/w1-contract-host-native`
- Merge base: `7ad18cdd616d97145564ee925235d27e2369319f`
- Immutable W1 contract revision: `cbab33555ab9355a6ac453d659c55ec9e0666821`
- Rust toolchain: `1.92.0`
- Status: candidate pending pull-request review and remote CI

## Production authority

`RequestRegistry` is the only lifecycle authority. It owns request and attempt
identities, transitions, bounded progress, terminal records, cancellation,
operation hierarchy, worker `JoinHandle`s, reaping, and the canonical shutdown
receipt. The same registry now drives generation, controlled generation, and
embedding admission and execution.

`NativeModelOwner::shutdown_joined` joins the actual model thread and returns
the exact expected and joined worker counts and ID vectors recorded by the
production registry. A caller cannot construct `JoinedNativeModel`, and a
panicking model worker still cannot mint joined authority.

The W1 adapter contains conversions and trait wiring only. It does not create,
reverse, or otherwise synthesize worker IDs; maintain a shadow registry; or
invent closed-state counts. Contract-controlled work uses real OS threads and
the registry-owned `JoinHandle` ledger.

## Accepted manifest

One typed `LifecycleCoverageManifest<NativeRequestLifecycle>` accepts evidence
from exactly these 11 compositional suites:

1. transition chain
2. registry identity
3. attempt hierarchy
4. consumer cancellation
5. terminal authority
6. waiter control
7. admission/quiesce/shutdown bridge
8. progress/shutdown bridge
9. panic/shutdown bridge
10. stable shutdown
11. task reaping

The accepted manifest reports product `llama-native-kit`, implementation
`request-registry-owner-v1`, and all 18 normative lifecycle invariants covered.

## Defect found during composition

The first full composed run exposed a release race: an observer could see the
`Released` phase before the attempt disappeared from the active registry. The
repair performs terminal release, attempt removal, operation-hierarchy removal,
and the visible `Released` transition while holding the registry and lifecycle
locks in a consistent order. Abandoned final executor leases use the same
atomic ownership rule.

## Local evidence

```text
rustup run 1.92.0 cargo test --locked -p llama-native-engine \
  --features unstable-w1-contract-tests \
  operation_registry::w1_contract_adapter::real_native_request_owner_satisfies_complete_compositional_manifest \
  -- --exact
  PASS: 1 passed; 0 failed

rustup run 1.92.0 cargo test --locked --workspace \
  --features unstable-w1-contract-tests
  PASS: 179 passed; 0 failed; 8 intentionally ignored real-GGUF tests
  PASS: 18 compile-fail doc tests

rustup run 1.92.0 cargo clippy --locked --workspace --all-targets \
  --features unstable-w1-contract-tests -- -D warnings
  PASS

rustup run 1.92.0 cargo fmt --all -- --check
  PASS

./scripts/check-architecture.sh
  PASS

git diff --check
  PASS
```

The ignored GGUF tests require external model paths and hashes. They are not
used as evidence for this deterministic ownership/lifecycle acceptance.
