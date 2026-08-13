# CI and fuzz hardening receipt

- Every third-party action is pinned to a reviewed immutable revision.
- Rust 1.88 is used for the MSRV and Linux/macOS/Windows test/Clippy matrix.
- Dependency audit and both fuzz-target builds are first-class pull-request
  jobs. An optional manual workflow gives each fuzz target a bounded 60-second
  run when parser or canonicalizer work warrants a fresh robustness campaign.
- The inspector fuzz target validates every successful bundle, checks monotonic
  usage, and repeats inspection to compare graph ordering, status, issues,
  coverage, and accounting after normalizing only the random job ID.
- Repository-owned policy fixtures reject mutable action references and live
  `ssh-keyscan`.

Local builds do not substitute for the public OS matrix or dependency audit.
The calendar-triggered fuzz requirement was removed by the repository owner on
2026-08-12 because recurring execution is not intrinsic to attachment
flattening or phase-one promotion.

## Local evidence

- `cargo audit` scanned 228 locked dependencies against 1,207 advisories and
  reported no vulnerability.
- Both fuzz targets built with `cargo-fuzz 0.13.2` and
  `nightly-2026-07-26`.
- `inspect` and `pipeline` each completed a 10-second live libFuzzer run with
  no crash or timeout. A later designated-machine campaign ran both targets for
  60 seconds and is the accepted phase-one bounded robustness evidence. Manual
  retained-artifact runs remain available but are not a promotion gate.
