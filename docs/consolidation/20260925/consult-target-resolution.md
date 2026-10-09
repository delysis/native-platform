# Consult target identity and overlapping @ groups

Parent: #105, `842226c28c51f9847fb14dbc5066280633cb0342`.

The inspected resolver conflated an already-seen valid direct target with an
unknown handle. Thus `@team @alice` could fail when Alice was already selected
through the group. Group expansion also bypassed the direct self-invocation
restriction, and a map keyed only by conversation ID silently selected the last
record when IDs collided.

The existing resolver entry now calls one checked implementation. The shared
reference lexer remains unchanged; no new quoting, path, inline-code, email or
escaping rule is invented. The resolver reads only the supplied in-memory
registry. It never reads documents, parses referenced content again, executes a
consult, or grants tool/network authority.

Known repeated targets are successful no-ops. First-occurrence order is stable.
Groups validate atomically: a missing member, repeated group member, non-persona
member or self target rejects the whole group. Duplicate conversation identities
are ambiguous even under different handles and when reached through a group.
Conflicting group/direct handles remain ambiguous. Unknown handles remain
explicit; the existing dispatcher still refuses unresolved/ambiguous admission.
No target count cap, source snapshot, model policy or application lease changes.

Twelve Rust regressions exercise the existing production entry, including both
group/direct orders, overlapping groups, self references, duplicate IDs, invalid
membership, unknown handles, stable ordering and immutable source records.

```sh
cargo fmt --all
cargo test --locked -p mom-llama-runtime target_resolution_tests
cargo test --locked -p mom-llama-runtime mentions::
cargo test --locked -p workspace-document references::
cargo clippy --locked -p mom-llama-runtime --all-targets -- -D warnings
```

Rust compilation/tests/Clippy and real model/native UI checks were not executed
in the authoring runtime. Source assembly changed only the inspected resolver
window (3 additions / 84 deletions in mentions.rs); this is not runtime proof.
