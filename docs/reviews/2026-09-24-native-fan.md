# Native acceptance: compatible fan after word insertion

The clean `53ee2dd5a737efbaa1e934c854f7c6125c127584` Loom candidate passed
packaging and archive verification with Rust/Cargo 1.92 and pnpm 11.16.0.
Approved Gemma model: `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`.

The initial invocation had an incorrectly transcribed expected SHA and stopped
before launch. Its log is retained. The corrected invocation read HEAD directly
and ran the unchanged real-model smoke against the exact archive.

The previous `AXFocused=false` setup failure did not recur. The driver reached
four runs at byte 103, correlated a 22-byte DOM Ghost with durable native output
while all four runs remained open, retained the same family over idle/resume,
cycled down/up, then persisted a 16-byte word acceptance. No fifth run or
project-busy failure occurred. This is partial native evidence, not a complete
Visual/Source or product acceptance result.

The run stopped at `Option-Right did not retain physical Option and exact
cached-session authority`. Its diagnostic has one accepted chunk, frozen
authority, the same four original run IDs, `optionHeld=true`, and three remaining
alternatives with `fanVisible=true`. The failing helper required `fanVisible=false`.

That expectation disagrees with production: `compatibleCompletionPresentations`
retains candidates whose bytes agree with the accepted prefix, and both editors
keep the Option lens visible when at least two alternatives remain. A family with
distinct first words concealed the helper bug by leaving only one alternative.

The driver now independently computes remaining run IDs from the persisted byte
delta and SHA-verified candidate bodies. It requires exact original ordering and
the corresponding visible/hidden fan. When visible it also requires native AX
option ordinals, count and selected identity to match that subset. Unconsume
still must restore the original four-option fan and exact manuscript bytes. The
four-run guard, model, deadlines, controller and production renderer are unchanged.

Two new WebKit tests exercise held Option, four-to-three prefix filtering and
three-to-four unconsume in the real Visual/Source editors and controller. The
focused browser file passes 20 tests before changing the helper, establishing
the existing product behavior. The compiled helper passes 23 contract assertions;
an old hidden-fan mutation is expected to fail the shared-prefix case. Neither
fixture evidence nor that negative control replaces a fresh native journey.

Local evidence is retained under
`/Users/george/.codex/acceptance/native-platform-local-integration-20260924`.
Raw native state remains in
`/private/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-loom-smoke.XXXXXX.EOX5ZyVHsz`.
An independent SQLite backup is `loom-failed-53ee2dd5.sqlite3` in the evidence
directory. Original stores, WALs, blobs and logs were not reset or edited.
