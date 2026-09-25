# Whole context units and stranded history recovery

Base: PR #102, `d3b5ae45d8b37c4aa05419a978a15e2c2648ed41`.

`workspace-document::BranchIndex` is recovered byte-for-byte from PR #47
(`a43231626f5deb4ad8f6f08beb36dca40a236270`), blob
`d3c20b65b1e1329f2bd6d82fece7de2324b6c168`. Its three original tests are
retained. This is selective source reuse, not a merge of #47's older frontend,
storage schema, or SQLite dependency changes.

The new context selector operates on caller-owned whole units, not flattened
chat fragments. A message's attribution, text and media selection must travel
together. It preserves recent suffixes, measures full framing, propagates
tokenizer errors, reports omissions, and bounds repeated measurements. It is
not an optimal packing algorithm or proof of image/audio token accounting.

Mom gains checked branch adapters preserving its valid role-filtered sibling
metadata. Explicit stale heads, duplicate occurrence IDs, missing parents and
cycles (including unselected branches) fail instead of producing a plausible
truncated prompt. Existing storage codecs and display adapters are unchanged.
The subsequent consult integration consumes these APIs at inference admission.

## Qualification

Authored: 9 selector tests, 5 additional Mom adapter tests, and the unchanged
3 history tests. The original metadata-preservation test remains. No Rust
compiler is available in the authoring environment; none is claimed executed.

```sh
cargo fmt --all -- --check
cargo test --locked -p workspace-document
cargo test --locked -p mom-llama-runtime document::tests
cargo clippy --locked -p workspace-document -p mom-llama-runtime --all-targets -- -D warnings
```

No user data, permissions, external process admission, native lifecycle, EASL,
model policy, or acceptance requirement changes in this layer.
