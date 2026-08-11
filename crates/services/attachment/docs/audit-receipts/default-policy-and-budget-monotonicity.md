# Default-policy and budget-monotonicity receipt

## Identity

- Repository: `delysis/attachment-native-kit`
- Branch: `codex/default-policy-invariant`
- Base: `a7702f423102716d9fa21b64c51c331d4044a31d`
- Status: steward successor candidate

## Correction

`Inspector::default()` is now derived directly from
`InspectionPolicy::default()`. It no longer calls a fallible constructor and
silently reconstructs the same policy on validation failure. Permanent tests
prove the default policy validates and the default inspector contains that
exact policy. User-provided policies still pass through `Inspector::new`.

`BudgetUsage::dominates` defines the monotonic receipt relation across all
eleven counters. Tests perturb every field independently, prove successful
ledger charges never decrease any dimension, prove depth and edge limits are
independent, and prove rejected/overflowing derived work saturates or returns
`budget_integer_overflow` without wrapping.

## Gates

```text
Rust 1.88.0: cargo test --locked --workspace --all-targets
  PASS: 179 passed
Rust 1.88.0: cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
  PASS
cargo fmt --all -- --check
  PASS
./scripts/check-boundaries.sh
  PASS
git diff --check
  PASS
```

## Deferred evidence

The existing hostile archive/parser regressions remain enabled and green. The
successor workflow now exposes the cross-OS matrix, cargo-audit, fuzz-build,
and scheduled bounded-fuzz gates; only their public GitHub runs can establish
cross-platform evidence.
