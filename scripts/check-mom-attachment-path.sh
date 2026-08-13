#!/usr/bin/env bash
set -euo pipefail

readonly MOM_REVISION="3cf57941af6d523378e7fa8b24f5c24c8e50363f"
readonly ROOT="$(git rev-parse --show-toplevel)"
readonly PROBE="$(mktemp -d "${TMPDIR:-/tmp}/native-platform-mom-attachment.XXXXXX")"

cleanup() {
    rm -rf -- "$PROBE"
}
trap cleanup EXIT

git -C "$PROBE" init --quiet
git -C "$PROBE" remote add origin https://github.com/delysis/mom-llama.git
git -C "$PROBE" fetch --quiet --depth=1 origin "$MOM_REVISION"
git -C "$PROBE" checkout --quiet --detach FETCH_HEAD
test "$(git -C "$PROBE" rev-parse HEAD)" = "$MOM_REVISION"

printf '\n[patch."https://github.com/delysis/attachment-native-kit"]\n' >> "$PROBE/Cargo.toml"
printf 'attachment-native-host = { path = "%s" }\n' \
    "$ROOT/crates/services/attachment/crates/attachment-native-host" >> "$PROBE/Cargo.toml"
printf 'attachment-native-types = { path = "%s" }\n' \
    "$ROOT/crates/services/attachment/crates/attachment-native-types" >> "$PROBE/Cargo.toml"

rustup run 1.92.0 cargo test \
    --manifest-path "$PROBE/Cargo.toml" \
    -p mom-llama-runtime \
    --features unstable-w1-vertical-fixtures \
    attachments::tests::w1_ordinary_markdown_round_trips_through_attachment_native_projection_and_reopen \
    -- --exact

echo "Mom Attachment vertical: exact Mom $MOM_REVISION passed against imported Attachment paths"
