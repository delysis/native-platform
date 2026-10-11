# EASL experiment reference audit — 2026-09-14

The incident record below describes the preserved obsolete checkout at
`/Users/george/.codex/worktrees/native-platform-easl-studio`. Its backup files
remain there. The recovery state is recorded at the end of this document.

The experiment was built against an obsolete Loom baseline. Its titlebar port
faithfully copied that obsolete source. It does not represent the current Loom
interface, and prior experiment tests do not establish parity with current Loom.

## Provenance

| Source | Verified revision | Meaning |
| --- | --- | --- |
| Experiment | `9f0ed98f27f7b7c9f0d9e68957f495714d4f6c22` | September 14 EASL Studio commit, based directly on September 5 Loom |
| Experiment/main common ancestor | `924ac64ef0a7512327406ec6490c5e017b5112a8` | September 5 merge of PR 42 |
| Current main | `8ce8948f3bcfc24df87ed0346801dedc7402feb0` | Local main, origin/main and a fresh `git ls-remote origin refs/heads/main` agree |
| Newer Mine integration | `a43231626f5deb4ad8f6f08beb36dca40a236270` | Open draft PR 47, not merged into main |

The experiment is missing **64 commits** reachable from current main. Its branch
reflog records creation from `9f0ed98` at 10:31:08 EDT on September 14. The recent
date of that EASL commit concealed the age of its application baseline.

`build.rs` reads `../../apps/loom/src/App.svelte` from this worktree and extracts
its titlebar SVGs. It verifies neither the intended application revision nor the
current control set. The EASL view then copies that source's older layout. Thus a
freshly built binary can reproduce the wrong interface exactly; proving which
binary ran did not prove the intended reference was current.

Current main's titlebar has the manuscript-outline and new-document controls,
configured pane toggles, recording control, and Ghost text/Loompad selector. The
old co-writer, completion-context, Shuttle, appearance, and editor-mode titlebar
controls are no longer the current control set. The EASL Studio label is an
experiment addition, not part of current main's titlebar.

The extracted titlebar markup is byte-identical in current main, committed Mine
integration, and the working `native-platform-loom-dotfile` tree. SHA-256:
`151f96bcd50186205652454582975ef7558a81ba1e1b983ecf8d844b7ed0ede0`.
This verifies the source reference; no new live original-app acceptance was
performed during this audit.

## Application and configuration impact

This is wider than a titlebar defect. The experiment predates ordinary-folder
admission, current pane and Loompad behavior, and store schema 15. Its store still
uses schema 14 and the retired migration sequence. Current main includes the
newer generation, lifecycle, file-watching, and attachment changes as well.

The newer Mine branch additionally owns `.mine.toml` in `loom-config`, with
shared native context/generation services and protected storage. That branch is
still an open draft and must be distinguished from current main. The experiment's
separate preferences policy must be reconciled with those consumers, rather than
becoming another source of persistent settings. The native text, Markdown, and
session work is preserved, but it has not been validated against these current
application contracts.

The parity table in `PARITY.md` predates this audit. Its retired controls and
research-era requirements cannot be used as current product requirements.

## Preserved work and recovery boundary

Before any source correction, the audit preserved all **28 tracked changes and
137 untracked files**, including deletions in the tracked patch. Recovery files
are under `target/reference-audit-2026-09-14/`:

- `audit.json`: source identities and per-file SHA-256 manifest.
- `tracked.patch`: complete binary-capable tracked diff from experiment HEAD.
- `untracked-source.tar.gz`: all untracked source files.
- `current-main-titlebar.svelte.txt`: the verified titlebar reference excerpt.

These are preservation and source-comparison records, not runtime acceptance.
The audit did not change product code, main, other worktrees, user projects, or
the running review process. This report and the parity notice are documentation
corrections following the audit.

A simulated merge of the committed experiment with main reported conflicts in
eight files: both CI workflows, Cargo.toml, Cargo.lock, App.svelte, app.css,
scripts/ci/ci-plan.mjs, and xtask/src/main.rs. That simulation did not include the
165 changed/new files. A blanket reset, copying only App.svelte, or accepting all
old-side merge resolutions would lose work or retain the obsolete baseline.

Continue by integrating the preserved native EASL work into an isolated checkout
of the verified current application baseline, retaining the current application
and configuration authorities. Refresh the behavior inventory from those actual
consumers before resuming view implementation. The next native review must bind
both its implementation source and its current reference, then exercise the
current controls in the running app. Full frontend parity remains unfinished.

## Recovery checkpoint — 2026-09-15

Work now continues in an isolated checkout:
`/Users/george/.codex/worktrees/native-platform-easl-current`, branch
`codex/loom-easl-current`, based on `a43231626f5deb4ad8f6f08beb36dca40a236270`.
This includes current main and the explicitly unmerged Mine draft PR 47. No main
or PR merge was performed. The original experiment and user manuscripts were
preserved.

The EASL/native text/Markdown work was brought into that checkout without copying
the obsolete App.svelte, app.css, store schema or separate preferences service.
The current App/CSS and store source remain unchanged. Shared Rust configuration,
Untitled creation and the existing store are consumed by both adapters; their
checkpoint/draft orchestration still needs consolidation. Both prose and
verse now retain exact source bytes. The native host creates no synthetic context
document; auxiliary editors bind existing document identities.

The current native build checks the reviewed App/CSS hashes and emits the
reference revision in `--build-info`. Current titlebar SVGs are extracted as data.
The older EASL label and retired controls have been removed from the native view.
The build check guards the source reference; it does not establish behavior or
visual equivalence. At this checkpoint, recording and suggestion controls lacked
native adapters and configured pane controls were still missing. Subsequent
pane work and its limited native interaction evidence are tracked in PARITY.md
and NATIVE-REVIEW-2026-09-15.md.

README.md, PARITY.md and the text-session README now describe this corrected
baseline and its explicit gaps. Existing contract checks exercise the new baseline;
the old appearance/performance/runtime evidence remains historical. No corrected
native application has been presented as a finished replacement or accepted UI.

Recovery validation in this checkout:

- 146 regular native text, Markdown, session and host-view tests passed across
  the consolidated run and the final 32-test host-view rerun. Two manual tests
  remain excluded from the ordinary run.
- The existing Tauri settings tests (6) and document-creation tests (2) passed.
- CI metadata/selection/workflow tests (79), xtask policy tests (6), current-doc
  and ignored-test inventory validation passed.
- Strict Clippy passed for owned EASL/native text/session/Markdown/view packages;
  imported Studio/Cast dependencies retain their existing warnings.
- The synthetic appearance test rendered light and dark pixels, which were
  inspected. No native-window interaction or release acceptance is claimed.

Pixel rendering caught escaped SVG strings being deserialized as borrowed JSON
strings. The loader now owns decoded strings, and an ordinary test renders the
actual current scene in both appearances. The final suite also exposed one
draft-recovery fixture entering macOS Keychain setup; that fixture now initializes
an explicit synthetic plaintext store, consistent with the other non-credential
tests. Production encryption and the application/store reference remain unchanged.

Logs and synthetic pixels are in this checkout's `target/easl-integration/`.
The work is local and uncommitted. These checks establish the recovery checkpoint,
not completion of the parity inventory.

## Upstream integration — 2026-09-16

The recovery work was preserved in local commit `dc7c654a816bf99f1885142f6a8ef3a24c405e91`
before integrating main `90349a54061790954cf8a88160e4e29a8a325d4d`. The preceding
uncommitted status describes the historical recovery checkpoint. The combined
source, conflict decisions, validation and remaining runtime boundary are in
[UPSTREAM-INTEGRATION-2026-09-16.md](UPSTREAM-INTEGRATION-2026-09-16.md).
The titlebar and PaneDivider reference did not change in this upstream update.
