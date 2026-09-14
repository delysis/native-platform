# Turbo Danielle import validation

Validated locally on 2026-09-14, macOS Apple Silicon. The source capability
inventory and deliberate domain exclusions are in [the integration record](turbo-danielle.md).

## Tree and native bundle

- Base: `924ac64ef0a7512327406ec6490c5e017b5112a8`.
- Runtime and UI tested at: `0d54b938919ccfde16696257b9c15e0046840a7a`.
- Branch: `codex/turbo-imports`.
- Checkout: `/Users/george/.codex/worktrees/native-platform-turbo-imports`.
- QA product: `Loom Imports QA`; identifier: `app.delysis.loom.imports-qa`.
- Executable: `target/debug/bundle/macos/Loom Imports QA.app/Contents/MacOS/loom-app`.
- Executable SHA-256: `96a71721c7107b8eb0a3da5a32238c69b66d69de4b4042c15c1586da76cd2262`.
- Embedded frontend build input `index-B1eVkeQV.js` SHA-256: `7bfcb4b15de5d46f1e0be1e489f8eaec68562e73c6edc841b1723c9062058c27`.
- Frontend CSS `index-kS00p_7O.css` SHA-256: `d0629d979e8bc7d3bc8e2dbc9525c95836e5218aee316f25af4a135b2b3787be`.
- First process: PID `69556`, executable resolved to the bundle above. Restart: PID `78888`, same executable.
- Current-only native accessibility control: **Import sources**, then **Choose files**, **Choose folder**, **Paste sources**, and **Connected source**.

The separate running Loompad Preview instance was left alone. The QA app used
its own application-support directory and synthetic sources only; it was
closed after the restart check. The subsequent commit changes only validation
documentation and the reviewed build-script hash, not runtime or UI source.

## Automated gates

Rust 1.92.0 (`ded5c06cf`), Cargo 1.92.0, pnpm 11.19.0.

| Gate | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo test --locked --workspace --all-targets` | 1,580 passed, 0 failed, 41 ignored across 64 test binaries |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed |
| `cargo run --locked -p xtask -- policy` | Passed |
| CI metadata, planning, required-check, ignored-test, backup and workflow tests | 120 passed |
| Current-documentation tests and validator | 4 passed; validator passed |
| Native, attachment, information and speech architecture boundaries | Passed |
| `pnpm install --lockfile-only --frozen-lockfile --offline` | Passed |
| `pnpm --filter @delysis/loom test` | 448 passed across 60 files |
| `pnpm --filter @delysis/loom check` | 0 errors, 0 warnings |
| Native debug `.app` build | Passed; Vite retains its bundle-size advisory |
| `git diff --check` | Passed |

The full Rust portion of `scripts/check-shell-policy.sh` passed. Its later CI
checks initially rejected the stale reviewed digest of the plugin build script.
Review confirmed that its only change is registering the eight new import
commands; no test-harness/compiler directives were added. The registry now
pins `a7efc778ffcffc0a05038333729b9a134ef6d165034537846e345a1bf3e02a13`.
The remaining script commands, beginning with the CI Node tests, were then
rerun successfully. No checks or ignored-test requirements were weakened.

The new fixtures include ten complete AttachmentHost import tests, five Google
acquisition/OAuth tests, and two Loom folder tests. They cover MBOX boundaries,
MIME/8-bit charsets and Gmail labels, archive budgets, Claude roles and retained
metadata, Slack short/system/thread rows, LinkedIn columns, RTF Unicode and
omitted embedded content, malformed exports, response bounds, OAuth state and
fresh PKCE, endpoint confinement, folder deduplication, and symlink refusal.
Ignored tests retain their existing live-model/platform requirements and were
not counted as product acceptance.

Build command:

```sh
rustup run 1.92.0 pnpm --filter @delysis/loom exec tauri build \
  --debug --bundles app --config /tmp/loom-turbo-qa.json
```

The temporary config contained only the QA product name and bundle identifier.
Local command logs are `/tmp/loom-turbo-shell-policy.log`,
`/tmp/loom-turbo-policy-final.log`, `/tmp/loom-turbo-ui-tests.log`,
`/tmp/loom-turbo-svelte-final.log`, and `/tmp/loom-turbo-bundle-final.log`.

## Exercised native behavior

1. Disabled automatic generation in the isolated QA app and opened the new
   import controls through the completion-context panel.
2. Entered two synthetic sources with Unicode text and an explicit
   `===SOURCE===` separator. **Import pasted text** reported two imports and
   zero failures. The context and manuscript were still empty before selection.
3. Selected both results and chose **Add selected to context**. Both appeared
   as editable text. Appended `QA reviewer note.` in the native visual context
   editor and observed it save.
4. Used the native folder chooser to grant `/tmp/loom-turbo-import-fixtures`:
   a Claude conversation JSON, a two-message MBOX, and an unsupported binary.
   The UI reported two successful imports and one explicit failure; neither
   successful result was lost because of the unsupported member.
5. Added the two successful exports to context. Native accessibility exposed
   distinct `human` and `assistant` sections, both email subjects and bodies,
   and the decoded `Café mailbox evidence.` text.
6. Switched to Markdown and back to visual editing. The imported source text,
   role metadata, separate email messages, and manual note survived.
7. Submitted `invalid-client` in account setup. Loom rejected it locally with
   `Use a Google Desktop app OAuth client.` No browser authorization or Google
   request was performed by this check.
8. Quit and reopened the exact same bundle. All four imported text sources and
   the manual note were visible in context; the manuscript was still empty.

Disk verification after these interactions found an empty manuscript (SHA-256
`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`), the saved
manual note in `document-context.json`, and byte-identical original objects:

| Synthetic source | Raw bytes | SHA-256 |
| --- | ---: | --- |
| `conversation.json` | 480 | `e05d928b5f93327809e139d0c0e774503929f01cae677087993cba26a695b881` |
| `mailbox.mbox` | 471 | `a5f1761ee93cf545fcbe94a86f0240910625129dc2be863ec87560803998c852` |

The corresponding immutable `loom.file-import.v1` receipts retain source paths,
byte counts and hashes, with `network_used: false` and `human_reviewed: false`.

## Remaining acceptance boundary

Live Google consent, credential writes and refresh, Gmail/Drive downloads,
disconnect, and multi-account pagination require a configured Desktop OAuth
client and authorized account. They are implemented but were not exercised
with a real account. LinkedIn connected imports use authorized Gmail
notification emails, matching upstream's active sync path; this does not add
direct LinkedIn-account OAuth or authenticated scraping. Public URL acquisition
is covered by existing acquisition tests but was not exercised through the
native UI in this check. No model generation, native media inference, release
packaging, main-branch merge, or cross-platform runtime acceptance is claimed.
