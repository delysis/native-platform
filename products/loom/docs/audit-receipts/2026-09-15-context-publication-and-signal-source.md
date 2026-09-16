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
with the workflow checks, 50 tests passed. Complete candidate execution after this
repair remains pending until its own receipt is inspected.

Local detailed receipts are `/tmp/loom-context-acceptance.json` and
`/tmp/loom-signal-packaging-acceptance.json`. Generated build caches and extracted
copies were removed after verification; source archives and user-edited test
workspaces were retained.
