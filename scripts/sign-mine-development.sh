#!/bin/sh
# Stable Keychain trust across local test builds, including distinct review
# bundle IDs. This signs development artifacts; it does not notarize a release.
set -eu

ARTIFACT=${1:?usage: sign-mine-development.sh /path/to/Mine.app [certificate-sha1]}
IDENTITY=${2:-${MINE_SIGNING_IDENTITY:-}}
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$(uname -s)" != Darwin ]; then
  echo 'Mine development signing requires macOS.' >&2
  exit 1
fi
if [ -z "$IDENTITY" ]; then
  IDENTITIES=$(security find-identity -v -p codesigning | awk '/[0-9]+\) [A-F0-9]{40} / { print $2 }')
  COUNT=$(printf '%s\n' "$IDENTITIES" | awk 'NF { count++ } END { print count+0 }')
  if [ "$COUNT" != 1 ]; then
    echo 'Set MINE_SIGNING_IDENTITY to one installed code-signing certificate SHA-1.' >&2
    exit 1
  fi
  IDENTITY=$IDENTITIES
fi
case "$IDENTITY" in
  *[!a-fA-F0-9]*|'') echo 'Signing identity must be a certificate SHA-1.' >&2; exit 1 ;;
esac
if [ "${#IDENTITY}" != 40 ]; then
  echo 'Signing identity must contain 40 hexadecimal characters.' >&2
  exit 1
fi
if [ -d "$ARTIFACT/Contents" ]; then
  codesign --force --sign "$IDENTITY" --identifier app.delysis.mine.development \
    --options runtime --timestamp=none \
    --entitlements "$ROOT/products/loom/apps/loom/src-tauri/Loom.entitlements" "$ARTIFACT"
else
  codesign --force --sign "$IDENTITY" --identifier app.delysis.mine.development \
    --options runtime --timestamp=none "$ARTIFACT"
fi
codesign --verify --deep --strict "$ARTIFACT"
codesign --display --requirements - "$ARTIFACT"
