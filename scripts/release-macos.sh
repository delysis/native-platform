#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
COMPONENT=${1:-}
RELEASE_KIND=${2:-candidate}

case "$RELEASE_KIND" in
  candidate|stable) ;;
  *)
    echo "usage: $0 {mom|loom|fte} [candidate|stable]" >&2
    exit 2
    ;;
esac

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
    TAG_PREFIX=mom-llama
    ;;
  loom)
    PRODUCT_DIR="$ROOT/products/loom/apps/loom"
    CONFIG="$PRODUCT_DIR/src-tauri/tauri.conf.json"
    PACKAGE=loom-app
    APP_NAME=Loom
    BINARY_NAME=loom-app
    TAG_PREFIX=loom
    ;;
  fte)
    PRODUCT_DIR="$ROOT/products/fte"
    CONFIG="$PRODUCT_DIR/src-tauri/tauri.conf.json"
    PACKAGE=free-token-energy
    APP_NAME="Free Token Energy"
    BINARY_NAME=free-token-energy
    TAG_PREFIX=fte-desktop
    ;;
  *)
    echo "usage: $0 {mom|loom|fte} [candidate|stable]" >&2
    exit 2
    ;;
esac

run() {
  echo "+ $*"
  "$@"
}

NOTARY_TEMP_ROOT=
cleanup() {
  if [ -n "$NOTARY_TEMP_ROOT" ] && [ -d "$NOTARY_TEMP_ROOT" ]; then
    find "$NOTARY_TEMP_ROOT" -depth -delete
  fi
}
trap cleanup EXIT HUP INT TERM

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

RELEASE_TAG=
RELEASE_TAG_OBJECT=
if [ "$RELEASE_KIND" = stable ]; then
  RELEASE_TAG="$TAG_PREFIX-v$VERSION"
  if ! git show-ref --verify --quiet "refs/tags/$RELEASE_TAG"; then
    echo "stable releases require the exact annotated tag $RELEASE_TAG" >&2
    exit 1
  fi
  if [ "$(git cat-file -t "refs/tags/$RELEASE_TAG")" != tag ]; then
    echo "stable release tag must be annotated: $RELEASE_TAG" >&2
    exit 1
  fi
  TAGGED_REVISION=$(git rev-parse "refs/tags/$RELEASE_TAG^{commit}")
  require_equal "stable release tag target" "$SOURCE_REVISION" "$TAGGED_REVISION"
  RELEASE_TAG_OBJECT=$(git rev-parse "refs/tags/$RELEASE_TAG")
fi

SIGNING_IDENTITY=${DELYSIS_SIGNING_IDENTITY:--}
NOTARY_PROFILE=${DELYSIS_NOTARY_PROFILE:-}
if [ "$RELEASE_KIND" = stable ]; then
  if [ -z "$SIGNING_IDENTITY" ] || [ "$SIGNING_IDENTITY" = "-" ]; then
    echo "stable releases require DELYSIS_SIGNING_IDENTITY for a Developer ID Application identity" >&2
    exit 1
  fi
  MATCHING_IDENTITY=$(security find-identity -v -p codesigning |
    grep -F "$SIGNING_IDENTITY" |
    grep -F "Developer ID Application:" |
    head -n 1 || true)
  if [ -z "$MATCHING_IDENTITY" ]; then
    echo "DELYSIS_SIGNING_IDENTITY is not an available Developer ID Application identity" >&2
    exit 1
  fi
  if [ -z "$NOTARY_PROFILE" ]; then
    echo "stable releases require DELYSIS_NOTARY_PROFILE for notarytool" >&2
    exit 1
  fi
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

EMBEDDED_MODEL=$(node "$ROOT/scripts/find-embedded-model.mjs" "$BUNDLE/Contents")
if [ -n "$EMBEDDED_MODEL" ]; then
  echo "model weights must remain runtime-discovered, but the bundle contains: $EMBEDDED_MODEL" >&2
  exit 1
fi

if [ "$SIGNING_IDENTITY" = "-" ]; then
  run codesign --force --deep --sign - "$BUNDLE"
  SIGNING_MODE=ad-hoc
else
  run codesign --force --deep --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$BUNDLE"
  if [ "$RELEASE_KIND" = stable ]; then
    SIGNING_MODE=developer-id
  else
    SIGNING_MODE=custom-identity
  fi
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

NOTARIZATION_STATUS=not-requested
NOTARIZATION_SUBMISSION_ID=
NOTARIZATION_RECEIPT_NAME=
NOTARIZATION_STAPLED=false
GATEKEEPER_ASSESSED=false
if [ "$RELEASE_KIND" = stable ]; then
  NOTARY_TEMP_ROOT=$(mktemp -d -t delysis-notary.XXXXXX)
  NOTARY_UPLOAD="$NOTARY_TEMP_ROOT/$APP_NAME.app.zip"
  NOTARIZATION_RECEIPT_NAME=notarization-receipt.json
  NOTARIZATION_RECEIPT="$OUTPUT_DIR/$NOTARIZATION_RECEIPT_NAME"
  run ditto -c -k --sequesterRsrc --keepParent "$BUNDLE" "$NOTARY_UPLOAD"
  echo "+ xcrun notarytool submit '$NOTARY_UPLOAD' --keychain-profile '$NOTARY_PROFILE' --wait --output-format json"
  xcrun notarytool submit "$NOTARY_UPLOAD" \
    --keychain-profile "$NOTARY_PROFILE" \
    --wait \
    --output-format json > "$NOTARIZATION_RECEIPT"
  NOTARIZATION_STATUS=$(node -e '
    const fs = require("fs");
    const receipt = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    if (typeof receipt.status !== "string") process.exit(1);
    process.stdout.write(receipt.status);
  ' "$NOTARIZATION_RECEIPT")
  NOTARIZATION_SUBMISSION_ID=$(node -e '
    const fs = require("fs");
    const receipt = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    if (typeof receipt.id !== "string" || receipt.id.length === 0) process.exit(1);
    process.stdout.write(receipt.id);
  ' "$NOTARIZATION_RECEIPT")
  require_equal "notarization status" Accepted "$NOTARIZATION_STATUS"
  run xcrun stapler staple "$BUNDLE"
  run xcrun stapler validate "$BUNDLE"
  NOTARIZATION_STAPLED=true
  run codesign --verify --deep --strict "$BUNDLE"
  run spctl --assess --type execute --verbose=4 "$BUNDLE"
  GATEKEEPER_ASSESSED=true
fi

ARCHIVE="$OUTPUT_DIR/$APP_NAME.app.zip"
run ditto -c -k --sequesterRsrc --keepParent "$BUNDLE" "$ARCHIVE"

CARGO_LOCK_SHA256=$(shasum -a 256 "$ROOT/Cargo.lock" | awk '{print $1}')
PNPM_LOCK_SHA256=$(shasum -a 256 "$ROOT/pnpm-lock.yaml" | awk '{print $1}')
EXECUTABLE_SHA256=$(shasum -a 256 "$EXECUTABLE" | awk '{print $1}')
ARCHIVE_SHA256=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
ARCHIVE_BYTES=$(stat -f '%z' "$ARCHIVE")
RECEIPT="$OUTPUT_DIR/release-receipt.json"

if [ "$(git rev-parse HEAD)" != "$SOURCE_REVISION" ] ||
  [ "$(git rev-parse 'HEAD^{tree}')" != "$SOURCE_TREE" ] ||
  [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
  echo "source changed while the release artifact was being finalized; refusing to write a receipt" >&2
  exit 1
fi

DELYSIS_RECEIPT_COMPONENT="$COMPONENT" \
DELYSIS_RECEIPT_VERSION="$VERSION" \
DELYSIS_RECEIPT_RELEASE_KIND="$RELEASE_KIND" \
DELYSIS_RECEIPT_REVISION="$SOURCE_REVISION" \
DELYSIS_RECEIPT_TREE="$SOURCE_TREE" \
DELYSIS_RECEIPT_TAG="$RELEASE_TAG" \
DELYSIS_RECEIPT_TAG_OBJECT="$RELEASE_TAG_OBJECT" \
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
DELYSIS_RECEIPT_SIGNING_IDENTITY="$SIGNING_IDENTITY" \
DELYSIS_RECEIPT_NOTARIZATION_STATUS="$NOTARIZATION_STATUS" \
DELYSIS_RECEIPT_NOTARIZATION_ID="$NOTARIZATION_SUBMISSION_ID" \
DELYSIS_RECEIPT_NOTARIZATION_RECEIPT="$NOTARIZATION_RECEIPT_NAME" \
DELYSIS_RECEIPT_NOTARIZATION_STAPLED="$NOTARIZATION_STAPLED" \
DELYSIS_RECEIPT_GATEKEEPER_ASSESSED="$GATEKEEPER_ASSESSED" \
DELYSIS_RECEIPT_CHECKS="$CHECKS" \
DELYSIS_RECEIPT_LOOM_POLICY_NAME="$LOOM_POLICY_NAME" \
DELYSIS_RECEIPT_LOOM_POLICY_FILE_SHA="$LOOM_POLICY_FILE_SHA256" \
DELYSIS_RECEIPT_ARCHIVE_NAME="$(basename "$ARCHIVE")" \
node <<'NODE' > "$RECEIPT"
const e = process.env;
const receipt = {
  schema: "delysis.macos-release-receipt.v2",
  created_at: new Date().toISOString(),
  component: e.DELYSIS_RECEIPT_COMPONENT,
  version: e.DELYSIS_RECEIPT_VERSION,
  release_kind: e.DELYSIS_RECEIPT_RELEASE_KIND,
  source: {
    revision: e.DELYSIS_RECEIPT_REVISION,
    tree: e.DELYSIS_RECEIPT_TREE,
    dirty: false,
    tag: e.DELYSIS_RECEIPT_TAG || null,
    tag_object: e.DELYSIS_RECEIPT_TAG_OBJECT || null,
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
    signing_identity: e.DELYSIS_RECEIPT_SIGNING_IDENTITY === "-"
      ? null
      : e.DELYSIS_RECEIPT_SIGNING_IDENTITY,
    executable_sha256: e.DELYSIS_RECEIPT_EXECUTABLE_SHA,
    archive: e.DELYSIS_RECEIPT_ARCHIVE_NAME,
    archive_bytes: Number(e.DELYSIS_RECEIPT_ARCHIVE_BYTES),
    archive_sha256: e.DELYSIS_RECEIPT_ARCHIVE_SHA,
  },
  checks_passed: e.DELYSIS_RECEIPT_CHECKS.split("|").filter(Boolean),
  loom_build_model_policy: e.DELYSIS_RECEIPT_LOOM_POLICY_NAME
    ? { name: e.DELYSIS_RECEIPT_LOOM_POLICY_NAME, file_sha256: e.DELYSIS_RECEIPT_LOOM_POLICY_FILE_SHA }
    : null,
  packaged_app_smoke: e.DELYSIS_RECEIPT_RELEASE_KIND === "stable"
    ? "required as adjacent smoke-receipt.json"
    : "not run by this command",
  notarization: {
    status: e.DELYSIS_RECEIPT_NOTARIZATION_STATUS,
    submission_id: e.DELYSIS_RECEIPT_NOTARIZATION_ID || null,
    stapled: e.DELYSIS_RECEIPT_NOTARIZATION_STAPLED === "true",
    gatekeeper_assessed: e.DELYSIS_RECEIPT_GATEKEEPER_ASSESSED === "true",
    receipt: e.DELYSIS_RECEIPT_NOTARIZATION_RECEIPT || null,
  },
};
process.stdout.write(`${JSON.stringify(receipt, null, 2)}\n`);
NODE

if [ "$RELEASE_KIND" = stable ]; then
  SMOKE_RECEIPT="$OUTPUT_DIR/smoke-receipt.json"
  run "$ROOT/scripts/smoke-macos-app.sh" "$COMPONENT" "$ARCHIVE" "$SMOKE_RECEIPT"
  echo "stable macOS release passed: $OUTPUT_DIR"
  echo "smoke receipt: $SMOKE_RECEIPT"
else
  echo "macOS release candidate: $OUTPUT_DIR"
  echo "exact-archive smoke: scripts/smoke-macos-app.sh $COMPONENT '$ARCHIVE' '$OUTPUT_DIR/smoke-receipt.json'"
fi
echo "receipt: $RECEIPT"
