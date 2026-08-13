# W5 service imports: promotion-candidate evidence

Status: **path cutover complete; not promoted**.

The accepted Attachment, Information, and Speech source histories have been
rewritten deterministically beneath distinct service prefixes and merged as
three separate unrelated-history merges. The source workspaces, lockfiles,
workflows, crate boundaries, fixtures, and receipts remain present in the raw
imports. No crate consolidation is performed in W5. Cutover commit
`a73df93428334dcfd5b302b598e7b9d7be1539ab` rebinds the root graph to
the imported paths while preserving the three nested source workspaces and
their accepted locks for the W8 lock-flattening boundary.

| Service | Accepted source | Imported commits | Prefix | Raw merge |
| --- | --- | ---: | --- | --- |
| Attachment | `2a8d3a9a1828162a51185d207822ceb1ba6283a8` | 7 | `crates/services/attachment` | `5e82ed646bad0f57480f809cedf0cc2745b39dc6` |
| Information | `7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe` | 21 | `crates/services/information` | `b73feb2649c2096505f6489023acf325117c267c` |
| Speech | `b836318f10a7e11f433ec3ea8dfa48707adc9b06` | 25 | `crates/services/speech` | `4a45947508cf33fb0f8043e0507f2dda86d5d75c` |

`migration/service-imports.json` records the exact source trees, filtered
heads, commit-map identities, merge parents, and imported lock identities.
`scripts/check-service-import-history.sh` authenticates every mapping, parent
edge, and byte-identical prefixed tree against the canonical source commits.

## Local acceptance

The root lock is reproducible across consecutive offline generations at
SHA-256 `832d978b9c4aad1fb0cd20da50da17775c97f7b4e505a4cb5e9dd3aa27624d30`.
The source-lock gate covers all 24 imported service packages and rejects the
three old service Git sources. The imported nested workspaces pass their
portable, platform, W1 vertical, boundary, pin, and strict-Clippy gates.

The imported Speech path also passed both host-bound real replays:

- Parakeet processed the exact retained WAV (305580 bytes, SHA-256
  `326d6723d9e793974338737bba586e9c742ffb2ed05fd6c9e4d2404df34d7ac0`),
  completed streamed and non-streamed transcription, cancelled its peer, and
  joined shutdown;
- the Apple Tauri harness used installed voice
  `com.apple.eloquence.en-US.Eddy`, produced a 153540-byte WAV, emitted one
  terminal event, and completed with network denied.

Attachment fuzz targets are built by ordinary CI. No scheduled run is a W5
gate. The accepted W1 catalogue remains the authority for product-owned Mom
and Loom fixture projections until those products move in W6 and W7.

`platform-runtime` adds the requested explicit `PlatformBuilder` composition
API. It returns one non-cloneable shutdown owner plus cloneable projected
handles, creates no service globals, and drains Gateway and Speech before the
final native join.

## Current boundary

This document deliberately does not yet claim W5 acceptance. At this point:

- candidate and post-merge CI runs are not recorded;
- the three source repositories remain active and unfrozen;
- no release or protected final tag is claimed.

These fields must remain pending until their corresponding changes and gates
have genuinely completed.
