#!/bin/sh
# Repair only the partition guard of one existing Mom credential. The item's
# trusted-application ACL and encrypted key bytes are never read or replaced.
set -eu
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 SIGNED_MOM_APP EXACT_DATA_DIRECTORY [--check]" >&2
  exit 2
fi
bundle=$1
data_dir=$2
mode=${3:-repair}
case "$bundle:$data_dir" in /*:/*) ;; *) echo 'Use absolute paths.' >&2; exit 2;; esac
case "$mode" in repair|--check) ;; *) echo 'Unknown mode.' >&2; exit 2;; esac
[ -f "$data_dir/runtime.sqlite3" ] || { echo 'The existing Mom database is missing.' >&2; exit 1; }
/usr/bin/codesign --verify --deep --strict "$bundle"
bundle_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$bundle/Contents/Info.plist")
[ "$bundle_id" = com.delysis.llama-native-kit.mom-llama ] || { echo 'Expected a Mom Llama app.' >&2; exit 1; }
team=$(/usr/bin/codesign -dv --verbose=4 "$bundle" 2>&1 | /usr/bin/sed -n 's/^TeamIdentifier=//p')
case "$team" in ''|'not set'|*[!A-Z0-9]*) echo 'Use an Apple-signed build with a stable TeamIdentifier.' >&2; exit 1;; esac
[ "${#team}" -eq 10 ] || { echo 'Invalid TeamIdentifier.' >&2; exit 1; }
account=$(printf '%s' "$data_dir" | /usr/bin/shasum -a 256 | /usr/bin/awk '{print $1}')
service=com.delysis.llama-native-kit.mom-llama.store.v1
keychain=$(/usr/bin/security default-keychain -d user | /usr/bin/sed 's/^[[:space:]]*"//;s/"[[:space:]]*$//')
# Metadata-only existence check: deliberately no -g or -w password output.
/usr/bin/security find-generic-password -a "$account" -s "$service" "$keychain" >/dev/null 2>&1
printf 'Mom credential: %s\nSigner partition: teamid:%s\n' "$account" "$team"
[ "$mode" != --check ] || exit 0
[ -t 0 ] || { echo 'Run this repair in a terminal so Apple can request authorization.' >&2; exit 1; }
# Apple requests the Keychain password interactively. Never put it in argv,
# environment variables, a file, or command output. Keep the item ACL intact.
exec /usr/bin/security set-generic-password-partition-list \
  -a "$account" -s "$service" -S "teamid:$team" "$keychain"
