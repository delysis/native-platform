#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
COMPONENT=${1:-}

if [ "$(uname -s)" != "Darwin" ]; then
  echo "release-macos.sh only builds the currently supported macOS packages" >&2
  exit 1
fi

case "$COMPONENT" in
  mom)
    PRODUCT_DIR="$ROOT/products/mom/apps/mom-llama"
    CONFIG="$PRODUCT_DIR/src-tauri/tauri.conf.json"
    PACKAGE=mom-llama-app
    APP_NAME="Mom Llama"
    BINARY_NAME=mom-llama-app
    ;;
  loom)
    PRODUCT_DIR="$ROOT/products/loom/apps/loom"
    CONFIG="$PRODUCT_DIR/src-tauri/tauri.conf.json"
    PACKAGE=loom-app
    APP_NAME=Loom
    BINARY_NAME=loom-app
    ;;
  fte)
    PRODUCT_DIR="$ROOT/products/fte"
    CONFIG="$PRODUCT_DIR/src-tauri/tauri.conf.json"
    PACKAGE=free-token-energy
    APP_NAME="Free Token Energy"
    BINARY_NAME=free-token-energy
    ;;
  *)
    echo "usage: $0 {mom|loom|fte}" >&2
    exit 2
    ;;
esac

run() {
  echo "+ $*"
  "$@"
}

CHECKS=
record_check() {
  CHECKS="${CHECKS}${CHECKS:+|}$1"
}

run_exact_test() {
  package=$1
  target_kind=$2
  target_name=$3
  test_name=$4
  test_list=$(mktemp -t delysis-release-tests.XXXXXX)
  if [ "$target_kind" = lib ]; then
    rustup run 1.92.0 cargo test --locked -p "$package" --lib -- --list > "$test_list"
  else
    rustup run 1.92.0 cargo test --locked -p "$package" --bin "$target_name" -- --list > "$test_list"
  fi
  if ! grep -Fqx "$test_name: test" "$test_list"; then
    rm -f "$test_list"
    echo "release check no longer exists: $package $test_name" >&2
    exit 1
  fi
  rm -f "$test_list"
  if [ "$target_kind" = lib ]; then
    run rustup run 1.92.0 cargo test --locked -p "$package" --lib "$test_name" -- --exact
  else
    run rustup run 1.92.0 cargo test --locked -p "$package" --bin "$target_name" "$test_name" -- --exact
  fi
  record_check "$package::$test_name"
}

require_equal() {
  field=$1
  expected=$2
  observed=$3
  if [ "$expected" != "$observed" ]; then
    echo "$field mismatch: expected '$expected', observed '$observed'" >&2
    exit 1
  fi
}

cd "$ROOT"

VERSION=$(node -e 'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).version)' "$CONFIG")
BUNDLE_ID=$(node -e 'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).identifier)' "$CONFIG")
MINIMUM_MACOS=$(node -e 'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).bundle.macOS.minimumSystemVersion)' "$CONFIG")
SOURCE_REVISION=$(git rev-parse HEAD)
SOURCE_TREE=$(git rev-parse 'HEAD^{tree}')
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
  echo "release candidates require a clean source tree; commit or stash the current changes" >&2
  exit 1
fi

run pnpm install --frozen-lockfile --offline

case "$COMPONENT" in
  mom)
    run_exact_test mom-llama-runtime lib unused store::tests::prior_logical_store_import_cleans_plaintext_and_reopens_with_fixture_only_key
    run_exact_test mom-llama-runtime lib unused kv_cache::tests::persistent_cache_corruption_invalidates_and_falls_back_after_reopen
    run_exact_test mom-llama-app bin mom-llama-app app_runtime::tests::direct_native_operation_drains_before_final_join
    ;;
  loom)
    run_exact_test loom-store lib unused store::tests::prior_v10_project_store_migrates_and_reopens_without_identity_drift
    run_exact_test loom-store lib unused generation::tests::exact_boundary_suggestion_promotion_survives_store_reopen
    run_exact_test tauri-plugin-loom lib unused tests::close_cancels_active_family_waits_for_terminal_release_and_replays
    run pnpm --dir "$PRODUCT_DIR" test
    record_check "@delysis/loom::frontend-tests"
    ;;
  fte)
    run_exact_test free-token-energy lib unused db::tests::local_model_configuration_survives_database_reopen
    run_exact_test free-token-energy lib unused db::tests::fresh_database_is_versioned_and_reopens_only_as_the_current_schema
    run_exact_test free-token-energy lib unused gateway_runtime::tests::runtime_shutdown_reports_every_owned_worker_and_native_join
    run_exact_test fte-router lib unused tests::shutdown_cancels_active_work_and_waits_for_authoritative_completion
    run pnpm --dir "$PRODUCT_DIR" run test:frontend
    record_check "free-token-energy::frontend-tests"
    ;;
esac

TARGET_DIR=$(rustup run 1.92.0 cargo metadata --locked --no-deps --format-version 1 | node -e 'let s=""; process.stdin.on("data", c => s += c).on("end", () => console.log(JSON.parse(s).target_directory))')
BUNDLE="$TARGET_DIR/release/bundle/macos/$APP_NAME.app"
EXECUTABLE="$BUNDLE/Contents/MacOS/$BINARY_NAME"
if [ -d "$BUNDLE" ]; then
  rm -rf "$BUNDLE"
fi

LOOM_POLICY_NAME=
LOOM_POLICY_FILE_SHA256=
if [ "$COMPONENT" = loom ]; then
  LOOM_POLICY_NAME=writer-gemma4-base-v2
  LOOM_POLICY_FILE="$ROOT/products/loom/model-policies/$LOOM_POLICY_NAME.json"
  LOOM_POLICY_FILE_SHA256=$(shasum -a 256 "$LOOM_POLICY_FILE" | awk '{print $1}')
fi

run env MACOSX_DEPLOYMENT_TARGET="$MINIMUM_MACOS" CMAKE_OSX_DEPLOYMENT_TARGET="$MINIMUM_MACOS" LOOM_BUILD_MODEL_POLICY="$LOOM_POLICY_NAME" pnpm --dir "$PRODUCT_DIR" exec tauri build --bundles app -- --locked
record_check "$PACKAGE::tauri-build"

if [ ! -d "$BUNDLE" ] || [ ! -x "$EXECUTABLE" ]; then
  echo "expected bundle or executable is missing: $BUNDLE" >&2
  exit 1
fi

PLIST="$BUNDLE/Contents/Info.plist"
OBSERVED_BUNDLE_ID=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$PLIST")
OBSERVED_VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$PLIST")
OBSERVED_EXECUTABLE=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$PLIST")
OBSERVED_MINIMUM_MACOS=$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$PLIST")
require_equal CFBundleIdentifier "$BUNDLE_ID" "$OBSERVED_BUNDLE_ID"
require_equal CFBundleShortVersionString "$VERSION" "$OBSERVED_VERSION"
require_equal CFBundleExecutable "$BINARY_NAME" "$OBSERVED_EXECUTABLE"
require_equal LSMinimumSystemVersion "$MINIMUM_MACOS" "$OBSERVED_MINIMUM_MACOS"

EMBEDDED_MODEL=$(find "$BUNDLE/Contents" -type f \( -iname '*.gguf' -o -iname '*.safetensors' -o -iname '*.onnx' \) -print -quit)
if [ -n "$EMBEDDED_MODEL" ]; then
  echo "model file must remain runtime-discovered, but the bundle contains: $EMBEDDED_MODEL" >&2
  exit 1
fi

SIGNING_IDENTITY=${DELYSIS_SIGNING_IDENTITY:--}
if [ "$SIGNING_IDENTITY" = "-" ]; then
  run codesign --force --deep --sign - "$BUNDLE"
  SIGNING_MODE=ad-hoc
else
  run codesign --force --deep --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$BUNDLE"
  SIGNING_MODE=custom-identity
fi
run codesign --verify --deep --strict "$BUNDLE"
record_check "$PACKAGE::codesign-verify"

if [ "$(git rev-parse HEAD)" != "$SOURCE_REVISION" ] ||
  [ "$(git rev-parse 'HEAD^{tree}')" != "$SOURCE_TREE" ] ||
  [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
  echo "source changed while the release candidate was building; refusing to write a receipt" >&2
  exit 1
fi

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
SHORT_REVISION=$(git rev-parse --short=12 HEAD)
OUTPUT_DIR="$ROOT/dist/macos/$COMPONENT-v$VERSION-$SHORT_REVISION-$STAMP"
mkdir -p "$OUTPUT_DIR"
ARCHIVE="$OUTPUT_DIR/$APP_NAME.app.zip"
run ditto -c -k --sequesterRsrc --keepParent "$BUNDLE" "$ARCHIVE"

CARGO_LOCK_SHA256=$(shasum -a 256 "$ROOT/Cargo.lock" | awk '{print $1}')
PNPM_LOCK_SHA256=$(shasum -a 256 "$ROOT/pnpm-lock.yaml" | awk '{print $1}')
EXECUTABLE_SHA256=$(shasum -a 256 "$EXECUTABLE" | awk '{print $1}')
ARCHIVE_SHA256=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
ARCHIVE_BYTES=$(stat -f '%z' "$ARCHIVE")
RECEIPT="$OUTPUT_DIR/release-receipt.json"

DELYSIS_RECEIPT_COMPONENT="$COMPONENT" \
DELYSIS_RECEIPT_VERSION="$VERSION" \
DELYSIS_RECEIPT_REVISION="$SOURCE_REVISION" \
DELYSIS_RECEIPT_TREE="$SOURCE_TREE" \
DELYSIS_RECEIPT_CARGO_LOCK="$CARGO_LOCK_SHA256" \
DELYSIS_RECEIPT_PNPM_LOCK="$PNPM_LOCK_SHA256" \
DELYSIS_RECEIPT_BUNDLE_ID="$BUNDLE_ID" \
DELYSIS_RECEIPT_MINIMUM_MACOS="$MINIMUM_MACOS" \
DELYSIS_RECEIPT_PACKAGE="$PACKAGE" \
DELYSIS_RECEIPT_APP_NAME="$APP_NAME" \
DELYSIS_RECEIPT_EXECUTABLE_SHA="$EXECUTABLE_SHA256" \
DELYSIS_RECEIPT_ARCHIVE_SHA="$ARCHIVE_SHA256" \
DELYSIS_RECEIPT_ARCHIVE_BYTES="$ARCHIVE_BYTES" \
DELYSIS_RECEIPT_SIGNING_MODE="$SIGNING_MODE" \
DELYSIS_RECEIPT_CHECKS="$CHECKS" \
DELYSIS_RECEIPT_LOOM_POLICY_NAME="$LOOM_POLICY_NAME" \
DELYSIS_RECEIPT_LOOM_POLICY_FILE_SHA="$LOOM_POLICY_FILE_SHA256" \
DELYSIS_RECEIPT_ARCHIVE_NAME="$(basename "$ARCHIVE")" \
node <<'NODE' > "$RECEIPT"
const e = process.env;
const receipt = {
  schema: "delysis.macos-release-receipt.v1",
  created_at: new Date().toISOString(),
  component: e.DELYSIS_RECEIPT_COMPONENT,
  version: e.DELYSIS_RECEIPT_VERSION,
  source: {
    revision: e.DELYSIS_RECEIPT_REVISION,
    tree: e.DELYSIS_RECEIPT_TREE,
    dirty: false,
  },
  inputs: {
    cargo_lock_sha256: e.DELYSIS_RECEIPT_CARGO_LOCK,
    pnpm_lock_sha256: e.DELYSIS_RECEIPT_PNPM_LOCK,
  },
  macos: {
    package: e.DELYSIS_RECEIPT_PACKAGE,
    app_name: e.DELYSIS_RECEIPT_APP_NAME,
    bundle_id: e.DELYSIS_RECEIPT_BUNDLE_ID,
    minimum_version: e.DELYSIS_RECEIPT_MINIMUM_MACOS,
    signing: e.DELYSIS_RECEIPT_SIGNING_MODE,
    executable_sha256: e.DELYSIS_RECEIPT_EXECUTABLE_SHA,
    archive: e.DELYSIS_RECEIPT_ARCHIVE_NAME,
    archive_bytes: Number(e.DELYSIS_RECEIPT_ARCHIVE_BYTES),
    archive_sha256: e.DELYSIS_RECEIPT_ARCHIVE_SHA,
  },
  checks_passed: e.DELYSIS_RECEIPT_CHECKS.split("|").filter(Boolean),
  loom_build_model_policy: e.DELYSIS_RECEIPT_LOOM_POLICY_NAME
    ? { name: e.DELYSIS_RECEIPT_LOOM_POLICY_NAME, file_sha256: e.DELYSIS_RECEIPT_LOOM_POLICY_FILE_SHA }
    : null,
  live_model_and_ui_smoke: "not run by this command",
  notarization: "not requested",
};
process.stdout.write(`${JSON.stringify(receipt, null, 2)}\n`);
NODE

echo "macOS release candidate: $OUTPUT_DIR"
echo "receipt: $RECEIPT"
