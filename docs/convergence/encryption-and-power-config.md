# Mine private storage and configuration

New macOS desktop projects protect private history, drafts, suggestions, completion
context, attachment originals/derivatives, co-writers, and function/chat receipts.
Existing projects remain readable in their current format. No read silently
converts or erases a project.

Manuscripts, authored dotfiles/persona sources, public Markdown assets, and the
ordinary-manuscript captures used by rename/delete/outbox recovery remain readable.
This is private-sidecar encryption, not whole-folder encryption. File names,
sizes, content-addressed identifiers and timestamps remain observable. Private
data requires the installation's Keychain key; copying the folder to another
machine alone does not transfer that credential. The production credential
backend currently supports macOS. Other platforms retain their existing ordinary
project initialization; an encrypted project never falls back to plaintext when
the credential backend is unavailable.

## One unlock across operations and test builds

`desktop-vault` uses one random installation key in the macOS login Keychain,
under service `app.delysis.mine.vault`, account `installation-key-v1`. Each project
has a separate random data key wrapped in its public `.loom/vault.json` header.
Moving a project does not change its cryptographic identity. No password, raw
key, or predictable development key is stored in a dotfile or environment
variable. Available keys are shared in process; concurrent startup requests and
cancelled/failed requests are coalesced. Only the explicit startup Retry clears
a failed unlock. Partial initialization containing actual payloads is refused;
an empty initialization skeleton can be retried without erasing anything.

Run `pnpm --filter @delysis/loom bundle:dev` for repeated local builds, or sign a
review artifact with `scripts/sign-mine-development.sh /absolute/path/to/App.app`.
The script uses the single installed signing certificate, or an explicitly
selected `MINE_SIGNING_IDENTITY` certificate SHA-1. It retains code-signing
identifier `app.delysis.mine.development` while review bundle IDs may differ.
Keychain tracks the creating application's designated code requirement, so a
stable signed identity can retain access across rebuilds. Ad-hoc signatures do
not provide that continuity. A locked Keychain or changed signing identity may
still require an OS authorization; there is no attempt to bypass it.
[Apple's code-signing and Keychain rules](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

An opt-in `desktop-vault` example, `unlock_probe`, verifies credential continuity
without opening a user project. On the development Mac, separately built and
signed probes A and B opened the same vault; another B launch completed in
0.04 seconds. Their executable SHA-256 values were
`d6eca6af78613ebdd7b356b7b973e59921c8c0f7a8643fb5ab7877570c21a16b` and
`6e83b4623c58c4154a7ded1df1c1bd42b081d611c4b5c72b94f0aa83c677e7b9`.
This proves the OS credential path for those signed executables; it is separate
from editor, inference, and bundle acceptance.

## Storage and preservation

SQLCipher protects database pages and WAL records. The key is applied before
schema access, a real page read authenticates it, and SQL temporary storage is
memory-only. File payloads use XChaCha20-Poly1305 with fresh 192-bit nonces and
project/namespace authentication. HKDF-SHA256 separates database and file keys.
Ciphertext is produced before any private temporary file is written. Readers
bound ciphertext allocation, authenticate, then parse/check the original bytes.
Immutable retries compare authenticated plaintext rather than randomized
ciphertext. Missing/tampered headers, swapped files, wrong keys and plaintext in
a secured namespace fail closed. No receipt grants model or tool authority just
because it decrypts successfully.

The original rusqlite 0.39.0 dependency bundled SQLCipher with SQLite 3.50.4,
which lacked main's WAL-reset corruption fix. The exact pin is now rusqlite
0.40.2 / libsqlite3-sys 0.38.2: SQLCipher 4.14.0 uses fixed SQLite 3.51.3;
ordinary bundled SQLite is 3.53.2. Secure store admission requires both an actual
SQLCipher engine and SQLite at least 3.51.3. This avoids regressing the other
workspace consumers when Cargo unifies SQLite features.
[SQLCipher 4.14 release](https://www.zetetic.net/blog/2026/03/17/sqlcipher-4.14.0-release/),
[SQLite 3.51.3 fix](https://sqlite.org/releaselog/3_51_3.html),
[SQLCipher API](https://www.zetetic.net/sqlcipher/sqlcipher-api/).

To create an encrypted project explicitly through the CLI:

```sh
loom init /absolute/path/NewProject --name NewProject --encrypted
```

To protect an existing project's private data, first close it in the app, then:

```sh
loom protect-copy /absolute/path/ExistingProject --to /absolute/path/ProtectedProject
```

The destination must be new and outside the source. The command stages and
authenticates a separate copy, retains exact semantic identities/history and
active drafts, and publishes without clobbering another destination. It refuses
unknown private formats, symlinks, pending manuscript recovery and changed source
bytes. The source opener does not replay recovery or checkpoint on close. The
original plaintext project is preserved and must not be described as encrypted
or securely erased by this command. Normal manuscript recovery keeps its exact
inode/no-clobber behavior; those readable captures are an explicit boundary.

Attachment links now offer explicit Save Original: verified decrypted bytes go
only to a user-selected new file, after session revalidation. There is no
automatic decrypted temporary file or reveal of ciphertext as if it were an
original. Export refuses private `.loom` destinations and existing files.

## Configuration and the remaining chat work

The remaining custom-download and Google-client setup forms have moved to strict
`[downloads.<name>]` and `[imports.google]` definitions. Exact retry, cancellation,
model loading, account connection, selection, sync and import remain deliberate
commands. Opening or editing configuration executes none of them. Google client
secrets and OAuth tokens do not enter the webview configuration snapshot. See
[Mine configuration](mine-configuration.md) for live examples and limits.

Typed chat means explicit speaker/content/attachment/branch fields, rather than
extracting speakers from a text prompt. `workspace-document` already supports
those parts; Mine's current live chat still builds a raw prompt. It needs native
chat/template dispatch and version-bound append/edit/select operations within
the common document model, not a second storage type.

One `@` resolver can subsume persona prose, examples, context and reusable prompt
functions. A person should be a document reference plus a configured execution
profile and history selection; a group should be an ordered invocation recipe.
Referencing a person, invoking them for an attributed response, and authorizing
an external effect remain distinct. Mine's `=@Function(...)` currently performs
bounded inference over document text. It does not implement Mama's persona
model/template selection, consult groups, tool permission lifecycle or Information
grants. Those live protocols remain unported; no inert settings pretend otherwise.
