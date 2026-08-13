# W5 service imports: raw-history evidence

Status: **raw import candidate; not promoted**.

The accepted Attachment, Information, and Speech source histories have been
rewritten deterministically beneath distinct service prefixes and merged as
three separate unrelated-history merges. The source workspaces, lockfiles,
workflows, crate boundaries, fixtures, and receipts remain present in the raw
imports. No crate consolidation is performed in W5.

| Service | Accepted source | Imported commits | Prefix | Raw merge |
| --- | --- | ---: | --- | --- |
| Attachment | `2a8d3a9a1828162a51185d207822ceb1ba6283a8` | 7 | `crates/services/attachment` | `5e82ed646bad0f57480f809cedf0cc2745b39dc6` |
| Information | `7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe` | 21 | `crates/services/information` | `b73feb2649c2096505f6489023acf325117c267c` |
| Speech | `b836318f10a7e11f433ec3ea8dfa48707adc9b06` | 25 | `crates/services/speech` | `4a45947508cf33fb0f8043e0507f2dda86d5d75c` |

`migration/service-imports.json` records the exact source trees, filtered
heads, commit-map identities, merge parents, and imported lock identities.
`scripts/check-service-import-history.sh` authenticates every mapping, parent
edge, and byte-identical prefixed tree against the canonical source commits.

## Current boundary

This document deliberately does not claim W5 acceptance. At this point:

- path dependency cutover is not recorded;
- candidate and post-merge CI runs are not recorded;
- the three source repositories remain active and unfrozen;
- real Parakeet and Apple runtime replays have not been repeated from the
  imported paths;
- Attachment fuzz targets are configured for ordinary CI build coverage, but
  no scheduled or bounded fuzz campaign is required or claimed.

These fields must remain pending until their corresponding changes and gates
have genuinely completed.
