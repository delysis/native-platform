# macOS releases

macOS is the only supported distribution target. Linux and Windows builds are
compatibility checks, not release gates.

## Candidate

From a clean commit, build an ad-hoc signed candidate without GitHub:

```sh
scripts/release-macos.sh {mom|loom|fte} candidate
```

The command runs the component's focused migration, reopen, shutdown, and
frontend checks; builds the `.app`; rejects bundled model weights; verifies its
identity and signature; and writes the ZIP and `release-receipt.json` under
`dist/macos/`. Run the exact ZIP smoke command printed at the end. It launches,
quits, and relaunches the extracted app against isolated product state.

Before tagging a stable release, use that candidate to exercise an active local
operation and the applicable backup/restore rollback. These are product checks,
not fields to rubber-stamp in a manifest.

## Keychain review

Use a consistent Apple Development or Developer ID signing identity for local
Keychain review. Candidate packaging already accepts this identity:

```sh
export DELYSIS_SIGNING_IDENTITY='Apple Development: Your Name (TEAMID)'
scripts/release-macos.sh mom candidate
```

Ad hoc signatures bind authorization to one executable version; rebuilding
changes that identity. They do not qualify prompt counts across builds. Preserve
the same data directory and signing identity when testing first access, reopen
and a subsequent build. Do not replace credentials or relax access controls to
suppress prompts. Apple describes this identity behavior in
[TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).

### Existing ad hoc Mom credential

If an existing Mom item was created by an ad hoc build, its partition can remain
bound to the old code hash after moving to a signed app. macOS then asks for
both ordinary item authorization and partition authorization. Inspect the
`securityd` ACL/partition diagnostic first; a fresh-store reset is not a repair.

For that confirmed case, repair only the existing credential's signer partition:

```sh
scripts/repair-mom-keychain.sh /absolute/path/to/Mom\ Llama.app /exact/data/directory --check
scripts/repair-mom-keychain.sh /absolute/path/to/Mom\ Llama.app /exact/data/directory
```

The script validates the bundle identity and stable TeamIdentifier, hashes the
exact data-directory spelling used by the runtime, and addresses one generic
password item by service and account in the default user keychain. It changes
only that item's partition guard to the app's team identity. Its trusted-app
ACL remains in force; other apps still need ordinary item authorization. It
never retrieves or replaces key bytes, deletes credentials, changes the login
keychain password, or updates other items. Apple requests the Keychain password
interactively; do not supply it through arguments, environment variables or a
file. See Apple's
[security tool manual](https://github.com/apple-oss-distributions/Security/blob/main/SecurityTool/macOS/security.1).

After repair, check the original encrypted store with the signed app and count
actual dialogs. The remaining ordinary authorization should require at most
one dialog; verify this before claiming acceptance.

## Stable package

Stable packaging requires an exact annotated component tag at `HEAD`:

| Component | Tag |
| --- | --- |
| Mom Llama | `mom-llama-v<version>` |
| Loom | `loom-v<version>` |
| Free Token Energy | `fte-desktop-v<version>` |

Store App Store Connect credentials in a local `notarytool` Keychain profile;
do not put them in the repository. Then run:

```sh
export DELYSIS_SIGNING_IDENTITY='Developer ID Application: Example (TEAMID)'
export DELYSIS_NOTARY_PROFILE='delysis-notary'
scripts/release-macos.sh {mom|loom|fte} stable
```

The command fails early if the tag, Developer ID identity, or notary profile
name is missing. It signs with the hardened runtime and a secure timestamp,
waits for Apple notarization, staples and validates the ticket, asks Gatekeeper
to assess the app, creates the final ZIP after stapling, and automatically
smokes that exact ZIP twice. A passing output directory contains:

- the notarized `.app.zip`;
- `release-receipt.json`, binding source, locks, bundle identity, signature,
  notarization, and artifact hashes;
- `notarization-receipt.json`, Apple's response; and
- `smoke-receipt.json`, binding the two-launch smoke to the exact ZIP and
  release receipt.

This command packages a stable artifact; it does not publish or upload one.
Publication remains a separate, intentional action.

## GitHub

`.github/workflows/release-macos.yml` creates a distinct ad-hoc **candidate**
for a tag or manual dispatch. It is asynchronous convenience, never a pull
request requirement and never the authority for the locally notarized package.
