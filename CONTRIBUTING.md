# Contributing

Keep changes small, safe, and evidence-bound. Rust must remain safe and
idiomatic, compile on the pinned 1.92.0 toolchain, inherit workspace lints, and
use workspace dependencies. Do not add a nested Cargo workspace or lockfile.

Every package must declare exactly one package group from the root catalogue.
Portable CI must not depend on credentials, external networks, installed
models, platform inventories, or real hardware. Tests needing those authorities
belong in the appropriate diagnostic or real-hardware group and must state what
they do not establish.

Production imports require a reviewed ledger update naming the immutable source
revision, destination prefix, import commit, and later path-dependency cutover.
Never copy production source into this shell speculatively.

Before proposing a change, run `./scripts/check-shell-policy.sh`.
