# Context publication and Signal source packaging

This is scoped evidence, not acceptance of the complete networking feature.

## Native context publication

The owner application was built from `11898f4c221254344851868952ccd7759bc7f68a`,
with main `90349a54061790954cf8a88160e4e29a8a325d4d` merged. Its distinct test
bundle identifier was `app.delysis.loom.context11898f4.owner`, PID 93642, executable
SHA-256 `31c3e90262d10cd1f922885931239f559f01b09b1b971c2e04e7ce8cbf5539ce`.
The built frontend was `index-oYuB4ho8.js`, SHA-256
`2ebfcd0d03d48de022eb0a161ab650c8180889f9e5b3881f837a493b8c9af449`.
The current Materials sidebar's **Share context as a document…** control was
observed through native accessibility before exercising it.

In the existing synthetic cabal, a new `Context Check.md` preserved the earlier
manuscript and its user's edits. Native controls imported a 55-byte text file
and a generated 32,044-byte WAV. The source text ended with the synthetic marker
`PRIVATE ORIGINAL ENDING`. Materials retained the instruction `Keep the prose
simple.` separately from the edited excerpt `The path curves around a pond.`.

The native publication review named only Moss and Fern, showed the instruction
and quoted edited excerpt, and listed only `Tone.wav` for full-file sharing.
Publication created
`Context/Context Check-ae3c47c9218043cf9828bc72fadd4f0e.md`. **Open shared context**
opened an ordinary editable document with the quoted excerpt and a one-second
audio control. The source manuscript remained exactly
`A synthetic garden for sharing context.`. The UI provided the quoted `@` path.

A read-only check of the cabal asset database confirmed:

| Object | SHA-256 | Result |
| --- | --- | --- |
| Original text | `b1f2bc871583bd248d81ccd79e6b4f83b0f98489264807a96330485605a4da11` | Absent from shared assets |
| Synthetic WAV | `13f720e7bd01cf20167cd10866e8e1dbbbd17ef8d361179a78c31539e8848a0c` | Complete; stored chunks equal the original bytes |

The same bundles later restarted as owner PID 99424 and peer PID 99410. Their
separate profile locks and cabal database handles were checked. Opening the
saved cabal folders restored the connection without another invitation. The
peer received and opened the published document and its one-second audio control.
Its stored WAV chunks equalled the original bytes; the text original remained
absent from the peer's shared assets.

The peer added `Fern adds a lantern.` through its native editor. On the owner,
adding `Later private scratch stays here.` to the source document's Instructions
and using **Find shared context** reopened the same publication without replacing
that peer edit. The owner then added `Moss adds a bridge.` to the shared document;
the peer's native editor showed both edits. Both Markdown files were byte-identical,
SHA-256 `6f3d938fe7ded147f2e264b0071b554eecda7fab7ce97eb78aec8c385fdd8557`.
The original source manuscript remained unchanged. The later private note and
excluded original text were absent from the shared document; opening the source's
Materials on the peer showed empty Instructions and no private source cards.
Both application processes exited after native Quit commands, verified without
relaunching them. No real Signal account, message, group, or physical second
network was used. This proves the packaged interaction on one Mac, not physical
Internet connectivity or simultaneous hardware input.

## Signal distribution

At `11898f4`, the extracted source archive built the actual Signal worker using
`cargo build --frozen`, an empty Cargo home, and a fresh target directory. The
original worktree and cached Cargo dependencies were not build inputs. This was
a macOS development build, not an optimized or signed release build.

Inspection then found that Cargo's libsignal workspace packages omit their
repository-root license. The subsequent notice change retains that license and
upstream desktop acknowledgments, with their original hashes and exact Git pin.
The source task checks both against the resolved dependency graph. Git's normal
Markdown line-ending conversion initially changed those upstream bytes; the
archive verifier rejected them. Scoped attributes now preserve the exact bytes.

At `82647ab488e858d4075919c618f7457a290aab73`, the real source task again verified
all 426 packages with an empty Cargo home and frozen offline resolution:

| Artifact | Evidence |
| --- | --- |
| Source archive | `loom-signal-source-82647ab488e858d4075919c618f7457a290aab73.tar.gz` |
| Archive size | 90,527,071 bytes |
| Archive SHA-256 | `d7a5f99f4637a0f9de04b02419a4d37005532ab2dcfa5ca3f6422c6da4143f39` |
| Signal lockfile SHA-256 | `f3105a51599038a5711065b4efd83fbcd634deff43ea57fd19ac2cf1f4dfc7fd` |
| Dependency build | Prior `11898f4` archive; worker source and lock unchanged |

The `Loom Notices 82647ab.app` development bundle contained all nine configured
license/notice resources, byte-for-byte equal to their source files, and the
Signal sidecar. This was resource inspection; this bundle was not launched for
product acceptance. The complete candidate/stable release script and notarization
were not run. Distributors must keep the app, source archive, and receipts together;
temporary CI artifact retention is not permanent source hosting.

The focused gate passed ten xtask tests, strict Clippy, repository policy,
formatting, and documentation validation. A fixture archive built and ran after
both original repositories were removed, using an empty Cargo home and `--frozen`.
It also rejected dirty source and existing output. Notice tests rejected altered
license text and a different Git pin. The release workflow checks passed 46 tests.
The context-command build-script review hash was refreshed after inspection;
all 25 ignored-test policy checks passed.

The first complete candidate run at `200df70` built and signed the release app,
checked notices, produced source and app archives, and wrote its release receipt.
Inspection rejected that candidate: its Tauri hook had compiled the Signal worker
in the development profile. An actual installed-CLI hook probe confirmed that
Tauri 2.11.4 leaves `TAURI_ENV_DEBUG` unset for release and sets it to `true` for
debug. The old helper incorrectly expected `false` for release. The rejection
is recorded alongside that candidate rather than changing its immutable receipt.

The repaired hook explicitly selects Tauri build mode; direct helper calls still
default to development. The release script also compares the staged sidecar with
the release worker before signing and records its profile. A process-boundary
fixture verifies both compiler arguments and copied artifacts: its unset-flag
release case failed before the fix and all four cases passed afterward. Combined
with the workflow checks, 50 tests passed. The subsequent complete candidate
execution is recorded below.

Local detailed receipts are `/tmp/loom-context-acceptance.json` and
`/tmp/loom-signal-packaging-acceptance.json`. Generated build caches and extracted
copies were removed after verification; source archives and user-edited test
workspaces were retained.

## Optimized release candidate

The full candidate pipeline completed at
`be2e8ef40dccafe314fa27b815eae4abdcb64ad8`, using Rust 1.95.0 and pnpm 11.16.0.
The receipt records a clean source tree, the optimized Signal worker, exact
notice resources, frozen offline source resolution, frontend tests, native
promotion/close checks, and a verified ad-hoc signature. Notarization was not
requested. The earlier `200df70` candidate remains rejected.

| Artifact | SHA-256 |
| --- | --- |
| `Loom.app.zip` (34,654,594 bytes) | `420c3e6f061551652d227ab1042780958817df985e142d86609609cd2ca025c0` |
| Signed app executable | `511290a5621039b577640b4b68205b72053b553da57e783b6b03c6dff981d2c2` |
| Packaged Signal worker | `748c26f4f0f4bff9de5bd4378ccfff1fea22d090acb44c84805ed1171a260722` |
| Corresponding-source archive | `0eafe814d313881282543a1c9c9cdaf35432541c1305bc909b8b0458a636d30b` |
| Corresponding-source receipt | `10a27f4139b6eea12cdf863e4ac243c2a15a54af810c275c8ca1c8a173ad73d5` |

After extracting the actual ZIP, its signature, both executable hashes, all nine
notice files, and the adjacent source-artifact hashes were checked independently.
The packaged worker created a dedicated unlinked SQLCipher store and reported
protocol version 4. A concurrent worker refused the same vault with
`vault_unavailable` and exited nonzero; the first worker continued serving Status.
It then reported Stopped and exited successfully while its parent's stdin was
still open. A subsequent process reopened the encrypted store, remained unlinked,
and exited cleanly on EOF. All owned processes exited. No account was linked and
no message was sent. Detailed evidence is `/tmp/loom-release-worker-acceptance.json`.

Two ad-hoc-signed copies derived from that exact extracted archive used separate
existing synthetic profiles and identifiers `app.delysis.loom.releasebe2e8ef.owner`
and `app.delysis.loom.releasebe2e8ef.peer`. The owner executable hash was
`93f0a08e2e2ef05556bde6ebe9a7e62e643af48bfd8cc43478b9261d7a31de1a`;
the peer hash was
`15302718092bdab729edb10d8b0785870f962f4e9229970a6ea60244d11f4877`.
Owner PID 34698 reopened the saved workspace and displayed the current shared
recording notice. Its Signal pane showed Unlinked, backed by child PID 35032
inside that bundle. Both applications reopened their saved cabal folders without
another invitation; the owner's cabal pane showed Fern connected. The owner
verified and loaded the existing Gemma 4 12B model from its isolated model library.
Native Quit closed both apps and the Signal child; process inspection confirmed
that none remained. No peer grant or job was submitted during this run, so it
does not establish native host preemption or cancellation.

These receipts precede the upstream quiet-sidebar and Drive-history changes at
`fb3a1b2d5d64b85d148d064cda908912b689347c`. They do not certify the later merged UI.

## Upstream integration checks

The subsequent merge incorporates that main revision, preserving its single
writing-mode control, ghost-choice cycling and quiet sidebar. Signal's shortcut
and the Materials/suggestion shortcuts coexist; context publication remains
inside Materials, opened with Cmd/Ctrl+Shift+C.

The merged tree passed 345 Rust library tests across `loom-document`, `loom-cabal`
and `tauri-plugin-loom` (five existing plugin tests ignored), strict Clippy for
those crates and all their targets, the native app check, workspace formatting
and current-documentation validation. It also passed 491 frontend unit tests,
84 focused WebKit tests covering editor interactions, Loompad, cabal editing,
connection settings, Materials and context publication, and Svelte checking
with zero errors or warnings. These are integration checks, not native acceptance
of the merged application.
