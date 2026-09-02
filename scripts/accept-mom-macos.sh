#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
RECEIPT_DESTINATION=${1:-}

if [ "$(uname -s)" != Darwin ]; then
  echo "accept-mom-macos.sh requires macOS" >&2
  exit 1
fi

if [ -n "$(git -C "$ROOT" status --porcelain --untracked-files=all)" ]; then
  echo "refusing to build acceptance evidence from a dirty source tree" >&2
  git -C "$ROOT" status --short >&2
  exit 1
fi

SOURCE_SHA=$(git -C "$ROOT" rev-parse HEAD)
SHORT_SHA=$(printf '%s' "$SOURCE_SHA" | cut -c1-12)
ACCEPTANCE_TMP_ROOT=${TMPDIR:-/tmp}
case "$ACCEPTANCE_TMP_ROOT" in
  /) ;;
  */) ACCEPTANCE_TMP_ROOT=${ACCEPTANCE_TMP_ROOT%/} ;;
esac
if [ ! -d "$ACCEPTANCE_TMP_ROOT" ]; then
  echo "temporary directory root is missing: $ACCEPTANCE_TMP_ROOT" >&2
  exit 1
fi
BUILD_ROOT=$(mktemp -d "$ACCEPTANCE_TMP_ROOT/delysis-mom-acceptance.$SHORT_SHA.XXXXXX")
RUN_TOKEN=$(basename -- "$BUILD_ROOT" | sed 's/.*\.//' | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9')
if [ -z "$RUN_TOKEN" ]; then
  echo "could not derive a safe unique acceptance token from: $BUILD_ROOT" >&2
  exit 1
fi
PRODUCT_NAME="Mom Llama Acceptance $SHORT_SHA $RUN_TOKEN"
BUNDLE_ID="com.delysis.mom-llama.acceptance.r$SHORT_SHA.$RUN_TOKEN"
TARGET_DIR="$BUILD_ROOT/target"
CONFIG="$BUILD_ROOT/tauri.acceptance.conf.json"

if [ -z "$RECEIPT_DESTINATION" ]; then
  RECEIPT_DESTINATION="$BUILD_ROOT/smoke-receipt.json"
else
  case "$RECEIPT_DESTINATION" in
    /*) ;;
    *) RECEIPT_DESTINATION="$PWD/$RECEIPT_DESTINATION" ;;
  esac
fi

node - "$PRODUCT_NAME" "$BUNDLE_ID" <<'NODE' > "$CONFIG"
const [productName, identifier] = process.argv.slice(2);
process.stdout.write(`${JSON.stringify({ productName, identifier }, null, 2)}\n`);
NODE

MINIMUM_MACOS=$(node -e \
  'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).bundle.macOS.minimumSystemVersion)' \
  "$ROOT/products/mom/apps/mom-llama/src-tauri/tauri.conf.json")

echo "+ source: $ROOT@$SOURCE_SHA"
echo "+ isolated target: $TARGET_DIR"
echo "+ acceptance product: $PRODUCT_NAME"
echo "+ acceptance bundle ID: $BUNDLE_ID"

env \
  CARGO_TARGET_DIR="$TARGET_DIR" \
  MACOSX_DEPLOYMENT_TARGET="$MINIMUM_MACOS" \
  CMAKE_OSX_DEPLOYMENT_TARGET="$MINIMUM_MACOS" \
  pnpm --dir "$ROOT/products/mom/apps/mom-llama" exec tauri build \
    --ci --bundles app --config "$CONFIG" -- --locked

if [ "$(git -C "$ROOT" rev-parse HEAD)" != "$SOURCE_SHA" ] ||
  [ -n "$(git -C "$ROOT" status --porcelain --untracked-files=all)" ]; then
  echo "source changed while the isolated acceptance bundle was building" >&2
  git -C "$ROOT" status --short >&2
  exit 1
fi

BUNDLE="$TARGET_DIR/release/bundle/macos/$PRODUCT_NAME.app"
EXECUTABLE="$BUNDLE/Contents/MacOS/mom-llama-app"
if [ ! -x "$EXECUTABLE" ]; then
  echo "isolated acceptance executable is missing: $EXECUTABLE" >&2
  exit 1
fi

# Tauri's custom per-run identity changes the bundle resources after its build
# signature is created. Re-seal the isolated local artifact before the strict
# smoke verifier binds it to the exact executable inode and hash.
codesign --force --deep --sign - "$BUNDLE"
codesign --verify --deep --strict "$BUNDLE"

MOM_ACCEPTANCE_PRODUCT_NAME="$PRODUCT_NAME" \
MOM_ACCEPTANCE_BUNDLE_ID="$BUNDLE_ID" \
MOM_ACCEPTANCE_SOURCE_SHA="$SOURCE_SHA" \
  "$ROOT/scripts/smoke-macos-app.sh" mom "$BUNDLE" "$RECEIPT_DESTINATION"

echo "Mom acceptance bundle: $BUNDLE"
echo "Mom acceptance executable: $EXECUTABLE"
echo "Mom acceptance bundle ID: $BUNDLE_ID"
echo "Mom acceptance source: $SOURCE_SHA"
echo "Mom acceptance receipt: $RECEIPT_DESTINATION"
