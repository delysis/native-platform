# Exact reversal after full candidate acceptance

The native run at `4820dc9c32f15b831f70a9876002ea7adf5620cd` passed
preterminal streaming, idle/resume, fan cycling, word acceptance/reversal and
Shuttle acceptance/reversal. Full fan Return then inserted the exact 197-byte
candidate, but Option-Left navigated ordinarily and admitted a new family.
The failed trace and checkpointed database remain in the external evidence
directory; this run did not pass native acceptance.

The production controller filtered empty tails out of its active family and
therefore returned no selected candidate after complete acceptance. Its session
still held the accepted chunk, but the editor had no candidate identity or exact
end offset with which to request reversal. Existing editor-only fixtures did not
exercise this controller projection.

A controller regression and real WebKit Visual/Source integration regressions
failed before the correction. The controller now retains a hidden, empty-text
presentation for exact rollback, while keeping the visible family empty and
refusing pending-mutation or mismatched-context authority. Source fan placement
also distinguishes this empty rollback witness from a scrolled-offscreen preview;
it retains the physical Option state so reversal restores the fan.

The focused controller suite passes 15 tests and its WebKit integration suite
passes 22. The tests require exact manuscript restoration, retained editor focus
and four restored alternatives with Option still held. They do not establish
packaged native acceptance; a clean new artifact must exercise that separately.
