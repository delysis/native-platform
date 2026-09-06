# Current architecture decisions

The [accepted ADR set](adr/README.md) is an immutable historical import. Its
pre-shell statements describe W1, not the current repository. This index records
subsequent scope changes without rewriting the sealed originals.

| Earlier decision | Current scope and evidence |
| --- | --- |
| ADR-001, first-party monorepo | Implemented by the root workspace and [package groups](../../ci/package-groups.json); source import history remains in the [migration ledger](../migration/). |
| ADR-007, one modern Gateway | FTE desktop, plugin, and loopback use the reusable FTE Gateway. The requirement that Mom compose FTE is superseded by Mom's [current product boundary](../../products/mom/AGENTS.md): Mom composes native inference directly and does not include hosted providers or loopback. This does not authorize a second router inside FTE. |
| ADR-010 and ADR-011, writing/research separation | The research engine was retired in [W9](../migration/W9-LEAN-EVIDENCE.md). Loom's supported writing paths remain separate from archived experiments; archive presence does not establish a current runtime. |
| ADR-012, Rust baseline | The root [toolchain](../../rust-toolchain.toml) and [workspace manifest](../../Cargo.toml) set Rust 1.92.0 and the shared lint policy. Imported historical build receipts retain their original compiler identities. |
| ADR-015, local macOS candidates | Historical release receipts describe their exact builds. Current release behavior is governed by [release-macos.yml](../../.github/workflows/release-macos.yml); successful tests alone do not establish signing or platform certification. |

The remaining lifecycle, result/progress, storage, credential, and evidence
boundaries remain applicable. This index describes architectural scope; it is
not a claim that every implementation invariant or acceptance gate has passed.
