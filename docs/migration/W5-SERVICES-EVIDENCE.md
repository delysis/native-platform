# W5 service imports: accepted evidence

Status: **accepted on `main`**.

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
  `326d6723b8bcd7ae63cdff4a2c3e536a29a9d3a44e30f9dca7b65e58a9b4aa34`),
  completed streamed and non-streamed transcription, cancelled its peer, and
  joined shutdown;
- the Apple Tauri harness used installed voice
  `com.apple.eloquence.en-US.Eddy`, produced a 153540-byte WAV, emitted one
  terminal event, and completed with network denied.

Attachment fuzz targets are built by ordinary CI. No scheduled run is a W5
gate. Exact Mom revision `3cf57941af6d523378e7fa8b24f5c24c8e50363f`
also executes its ordinary-Markdown Attachment vertical against the imported
Attachment host and types through `scripts/check-mom-attachment-path.sh`.

`platform-runtime` adds the requested explicit `PlatformBuilder` composition
API. It returns one non-cloneable shutdown owner plus cloneable projected
handles, creates no service globals, and drains Gateway and Speech before the
final native join.

## Corrected cross-product boundary

Three bullets in the original W5 plan cannot truthfully be executed at the
accepted source revisions: Loom has no Information adapter, and Mom has no
Information or Speech adapter. Merely compiling these crates in one diagnostic
graph is not a product vertical. Implementing the adapters here would mutate
the products before their W6 and W7 history imports and invalidate the accepted
source heads.

Accordingly, the Loom source fixture moves with Loom to W7. The Mom citation
fixture and Mom Speech vertical move with Mom to W6. This is an explicit plan
correction, not evidence of execution. W5 accepts the three service histories,
their service-owned verticals, the genuine imported-path Mom Attachment
vertical, and platform composition; it does not claim those absent product
integrations.

## Promotion and source-retirement boundary

Exact candidate `a7017421209176659035f7e407cb356615257fd5` passed GitHub Actions
run `31668250934`. Pull request 7 then merged it without squash as
`5baa238fc7c9676c06d36941d053d53020d21287`; the merge tree is byte-identical
to the candidate tree. Post-merge run `31670590658` passed Attachment fuzz and
the Ubuntu, macOS, and Windows workspace jobs.

Protected annotated tags `w5-import-services-candidate-v0-2026-08-12` and
`w5-import-services-v0-2026-08-12` preserve the candidate and functional-main
boundaries. Ruleset `20781374` protects those exact tag names with no bypass.

Each old service repository now has protected boundary tag
`native-platform-v2-horizon-b-2026-08-12`, peeled to its accepted imported
source commit. README-only freeze commits on each source `main` passed their
own CI, after which no-bypass rulesets denied branch creation, update,
deletion, and non-fast-forward pushes. The repositories remain deliberately
unarchived until two stable native-platform releases have been observed; zero
such releases are claimed here. Exact tag objects, freeze commits, run IDs,
and rulesets are recorded in `migration/service-imports.json`.
