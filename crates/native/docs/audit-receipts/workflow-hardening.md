# Workflow hardening receipt

- Third-party actions are pinned to reviewed 40-hex revisions.
- CI uses Rust `1.88.0`, matching workspace `rust-version = "1.88"`.
- A model-free policy job rejects mutable action tags and live `ssh-keyscan`
  before the three-OS native matrix.
- The native matrix runs locked all-target tests, locked strict Clippy, and the
  existing architecture boundary check where a POSIX shell is available.
- Real GGUF/Metal evidence remains an explicit local/manual gate and is not
  mislabeled as portable CI.

Local policy fixtures, formatting, strict Clippy, all-target workspace tests,
architecture checks, and diff checks passed. Cross-platform execution remains
GitHub-hosted evidence; this macOS checkout does not claim Windows execution.

The first exact-MSRV check used the inherited declaration, Rust 1.85.0, and
failed in `llama-native-types` because the canonical controlled-generation
code already uses stable let-chains. Let-chains stabilized in Rust 1.88, which
was also the repository's documented local acceptance toolchain. The manifest
and CI now state the real minimum instead of preserving a false 1.85 claim.
