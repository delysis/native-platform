# H3 W1 contract import evidence

Status: accepted local migration candidate, 2026-08-13.

The accepted `delysis/w1-platform-contracts` source at
`3ed1f3235edb6d481c324f05fe83b2379e3431e6` was filtered only by prefixing it
under `crates/platform/contracts` and merged as unrelated history at
`018aa483dbe34ecb3a62f70adc6bfebe99684acc`. Its accepted source tree
`55ad1fa9b7e3938043a153710922304022601a67` is byte-identical at the imported
prefix. The raw import merge contains no source mutation.

The import preserves all 25 source commits, including seven merge commits.
`migration/w1-platform-contracts.commit-map` records every source-to-filtered
identity. `scripts/check-contract-import-history.sh` verifies source and
prefixed trees, parent topology, authors, committers, dates, and subjects
against the live accepted source commit.

Commit `1c79381f9111dfd2d266291db243c7a5091a7fe4` cut the root graph and the
imported Native, FTE, Information, and Speech reverse dependencies from the
historical W1 Git revisions to the single imported source. The isolated Loom
probe patches its accepted external graph to the same local packages. Locks no
longer contain a W1 Git source.

The reverse-dependency run exposed one real compatibility mismatch: two FTE
observations written against the earlier vertical protocol did not repeat the
manifest's exact `omitted_claims`, which the accepted validator now enforces.
Commit `5a40a1669ae60973bd4fe1cf8b42d475d6e9f68a` makes those observations derive
the omissions from their authenticated manifest. Both FTE vertical suites then
pass against the single imported package; no historical Git revision or forked
compatibility crate is retained.

Local macOS verification covers the imported contract workspace, the shared
integration lifecycle and all 18 authenticated vertical manifests, Native and
FTE lifecycle adapters, Information publication and vertical adapters, Speech
lifecycle and vertical adapters, and the isolated Loom dependency graph. The
ignored Native real-GGUF tests remain hardware/model-dependent evidence and are
not promoted by this migration.

The source repository is frozen without archival after this accepted import.
Archival remains deferred until two consolidated releases, as specified by the
retirement policy.
