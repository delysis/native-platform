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
