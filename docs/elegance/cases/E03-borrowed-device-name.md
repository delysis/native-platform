# E03 — Compare the name you have

Unranked elegance archive. Base: `637e60b6b044230ed24ed3118615a2e5538cae83`.

The archive-name predicate allocated an uppercase String although its caller only needed an ASCII-insensitive Boolean classification. The candidate borrows the unchanged stem, compares fixed ASCII names without materializing uppercase text, and explicitly recognizes COM/LPT followed by one ASCII digit 1–9.

The original path.rs was reconstructed and matched Git blob `20e7a7bef73797e586ba2c61ec7244a690b77bda`. The proposed file is blob `1e894a731b8ab821cccd3c05645433aac1e60b5d`.

## Contract / tradeoff

No rule is broadened or narrowed intentionally. This is the existing bounded predicate, **not** a claim of complete Windows reserved-name semantics. COM0, COM10 and superscript digits retain their previous outcomes. Archive paths remain inert metadata; no filesystem authority is added. The surrounding rejection precedence, raw-name retention, budgets and sanitization logic are untouched.

Removing one explicit allocation is a structural observation, not a benchmark speedup. Extra comparisons against fixed short literals can matter to performance; actual end-to-end profiling remains outstanding. A larger path-parser redesign is not smuggled into this local example.

## Added evidence mechanism, not executed evidence

Two tests retain the previous predicate as a test-only oracle: ASCII case variants and suffix combinations; all 16,384 two-byte ASCII strings plus Unicode/empty/trailing-dot contrasts. Existing path tests remain.

**Rust, Cargo, rustfmt and Clippy were unavailable in the authoring container. This Rust candidate has not been compiled, formatted by rustfmt, or executed here.** The separately delivered Swift teaching analogue passed its own fixture checks; that is not Rust or production qualification.

Run focused attachment tests and formatting/lints, then the existing clean-tree component gate before promotion. Preserve all repository gates and native acceptance distinctions. No paid CI is dispatched. This proposal does not update Boom's incorporated copy; propagation requires a separately reviewed change with the source-provenance record kept accurate.

Historical affinity: problem-oriented economy and explicit representation, not a claimed direct historical transmission.
