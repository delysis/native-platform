# Phase-one fuzz gate disposition

Date: 2026-08-12

Authority: repository owner and phase-one steward

The requirement for a genuine calendar-triggered GitHub Actions fuzz run was
removed from phase-one promotion. Attachment inspection handles attacker-
controlled bytes, so the fuzz targets and their ordinary-CI build gate remain
valuable. A weekly cron, however, is not an intrinsic correctness requirement
for attachment-to-canonical-content processing and does not establish a
product behavior unavailable from the same bounded runner.

This disposition does not claim that a scheduled run occurred. The accepted
phase-one evidence is:

- Linux, macOS, and Windows Rust test/Clippy jobs on the reviewed promotion;
- MSRV, dependency audit, workflow policy, and both fuzz-target builds;
- the designated-machine 60-second campaigns at audited base `4729007...`:
  - `inspect`: 712,442 executions, exit 0, no crash or timeout artifact;
  - `pipeline`: 234,724 executions, exit 0, no crash or timeout artifact;
- the reviewed retained-artifact implementation now available through an
  explicit manual workflow dispatch when a new parser risk justifies it.

The workflow no longer has a schedule trigger. Its manual bounded-fuzz job is
optional robustness evidence, not a phase-one promotion or release gate.
