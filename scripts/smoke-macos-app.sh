#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
COMPONENT=${1:-}
SUPPLIED_ARTIFACT=${2:-}
RECEIPT_DESTINATION=${3:-}

if [ "$(uname -s)" != "Darwin" ]; then
  echo "smoke-macos-app.sh requires macOS" >&2
  exit 1
fi

case "$COMPONENT" in
  mom)
    APP_NAME="Mom Llama"
    BINARY_NAME=mom-llama-app
    BUNDLE_ID=com.delysis.llama-native-kit.mom-llama
    ;;
  loom)
    APP_NAME=Loom
    BINARY_NAME=loom-app
    BUNDLE_ID=app.delysis.loom
    ;;
  fte)
    APP_NAME="Free Token Energy"
    BINARY_NAME=free-token-energy
    BUNDLE_ID=dev.delysis.free-token-energy
    ;;
  *)
    echo "usage: $0 {mom|loom|fte} [path/to/App.app|path/to/App.app.zip] [receipt.json]" >&2
    exit 2
    ;;
esac

require_equal() {
  field=$1
  expected=$2
  observed=$3
  if [ "$expected" != "$observed" ]; then
    echo "$field mismatch: expected '$expected', observed '$observed'" >&2
    exit 1
  fi
}

read_receipt_string() {
  receipt=$1
  field=$2
  node - "$receipt" "$field" <<'NODE'
const fs = require("fs");
const [receipt, field] = process.argv.slice(2);
let value = JSON.parse(fs.readFileSync(receipt, "utf8"));
for (const key of field.split(".")) value = value?.[key];
if (typeof value !== "string" || value.length === 0) {
  console.error(`release receipt field is missing or invalid: ${field}`);
  process.exit(1);
}
process.stdout.write(value);
NODE
}

SMOKE_ROOT=$(mktemp -d -t "delysis-$COMPONENT-smoke.XXXXXX")
INPUT_ARCHIVE=
INPUT_ARCHIVE_SHA256=
INPUT_RELEASE_RECEIPT=
INPUT_RELEASE_RECEIPT_SHA256=
RELEASE_RECEIPT_EXECUTABLE_SHA256=

if [ -n "$SUPPLIED_ARTIFACT" ]; then
  case "$SUPPLIED_ARTIFACT" in
    /*) ARTIFACT=$SUPPLIED_ARTIFACT ;;
    *) ARTIFACT=$(CDPATH= cd -- "$(dirname -- "$SUPPLIED_ARTIFACT")" && pwd)/$(basename -- "$SUPPLIED_ARTIFACT") ;;
  esac
  case "$ARTIFACT" in
    *.zip)
      if [ ! -f "$ARTIFACT" ]; then
        echo "packaged application archive is missing: $ARTIFACT" >&2
        exit 1
      fi
      INPUT_ARCHIVE=$ARTIFACT
      INPUT_ARCHIVE_SHA256=$(shasum -a 256 "$INPUT_ARCHIVE" | awk '{print $1}')
      INPUT_RELEASE_RECEIPT="$(dirname -- "$INPUT_ARCHIVE")/release-receipt.json"
      if [ ! -f "$INPUT_RELEASE_RECEIPT" ]; then
        echo "adjacent release receipt is missing: $INPUT_RELEASE_RECEIPT" >&2
        exit 1
      fi
      INPUT_RELEASE_RECEIPT_SHA256=$(shasum -a 256 "$INPUT_RELEASE_RECEIPT" | awk '{print $1}')
      RELEASE_RECEIPT_COMPONENT=$(read_receipt_string "$INPUT_RELEASE_RECEIPT" component)
      RELEASE_RECEIPT_BUNDLE_ID=$(read_receipt_string "$INPUT_RELEASE_RECEIPT" macos.bundle_id)
      RELEASE_RECEIPT_ARCHIVE_SHA256=$(read_receipt_string "$INPUT_RELEASE_RECEIPT" macos.archive_sha256)
      RELEASE_RECEIPT_EXECUTABLE_SHA256=$(read_receipt_string "$INPUT_RELEASE_RECEIPT" macos.executable_sha256)
      require_equal "release receipt component" "$COMPONENT" "$RELEASE_RECEIPT_COMPONENT"
      require_equal "release receipt bundle ID" "$BUNDLE_ID" "$RELEASE_RECEIPT_BUNDLE_ID"
      require_equal "release receipt archive SHA-256" "$INPUT_ARCHIVE_SHA256" "$RELEASE_RECEIPT_ARCHIVE_SHA256"
      INSTALL_ROOT="$SMOKE_ROOT/extracted-archive"
      mkdir "$INSTALL_ROOT"
      ditto -x -k "$INPUT_ARCHIVE" "$INSTALL_ROOT"
      BUNDLE="$INSTALL_ROOT/$APP_NAME.app"
      ;;
    *) BUNDLE=$ARTIFACT ;;
  esac
else
  TARGET_DIR=$(rustup run 1.92.0 cargo metadata --locked --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" |
    node -e 'let s=""; process.stdin.on("data", c => s += c).on("end", () => console.log(JSON.parse(s).target_directory))')
  BUNDLE="$TARGET_DIR/release/bundle/macos/$APP_NAME.app"
fi

EXECUTABLE="$BUNDLE/Contents/MacOS/$BINARY_NAME"
PLIST="$BUNDLE/Contents/Info.plist"
if [ ! -d "$BUNDLE" ] || [ ! -x "$EXECUTABLE" ] || [ ! -f "$PLIST" ]; then
  echo "expected packaged application is missing or incomplete: $BUNDLE" >&2
  exit 1
fi

EXECUTABLE_SHA256=$(shasum -a 256 "$EXECUTABLE" | awk '{print $1}')
if [ -n "$INPUT_ARCHIVE" ]; then
  require_equal "release receipt executable SHA-256" "$EXECUTABLE_SHA256" "$RELEASE_RECEIPT_EXECUTABLE_SHA256"
fi

EMBEDDED_MODEL=$(node "$ROOT/scripts/find-embedded-model.mjs" "$BUNDLE/Contents")
if [ -n "$EMBEDDED_MODEL" ]; then
  echo "model weights must remain runtime-discovered, but the packaged bundle contains: $EMBEDDED_MODEL" >&2
  exit 1
fi

OBSERVED_BUNDLE_ID=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$PLIST")
OBSERVED_EXECUTABLE=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$PLIST")
if [ "$OBSERVED_BUNDLE_ID" != "$BUNDLE_ID" ]; then
  echo "bundle identifier mismatch: expected $BUNDLE_ID, observed $OBSERVED_BUNDLE_ID" >&2
  exit 1
fi
if [ "$OBSERVED_EXECUTABLE" != "$BINARY_NAME" ]; then
  echo "bundle executable mismatch: expected $BINARY_NAME, observed $OBSERVED_EXECUTABLE" >&2
  exit 1
fi
codesign --verify --deep --strict "$BUNDLE"

PRODUCT_STATE="$SMOKE_ROOT/product"
mkdir "$PRODUCT_STATE"
PRODUCT_STATE_CANONICAL=$(CDPATH= cd -- "$PRODUCT_STATE" && pwd -P)
ACTIVE_PID=

cleanup_failed_process() {
  if [ -n "$ACTIVE_PID" ] && kill -0 "$ACTIVE_PID" 2>/dev/null; then
    kill "$ACTIVE_PID" 2>/dev/null || true
    wait "$ACTIVE_PID" 2>/dev/null || true
  fi
}
trap cleanup_failed_process EXIT HUP INT TERM

wait_for_window() {
  target_pid=$1
  xcrun swift - "$target_pid" <<'SWIFT'
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let deadline = Date().addingTimeInterval(20)
repeat {
    let rows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID)! as! [[String: Any]]
    let ready = rows.contains { row in
        guard (row[kCGWindowOwnerPID as String] as? Int32) == pid,
              (row[kCGWindowLayer as String] as? Int) == 0,
              (row[kCGWindowIsOnscreen as String] as? Int) == 1,
              let bounds = row[kCGWindowBounds as String] as? [String: Any],
              let width = bounds["Width"] as? Double,
              let height = bounds["Height"] as? Double else {
            return false
        }
        return width > 0 && height > 0
    }
    if ready { exit(0) }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < deadline
fputs("packaged application did not expose an on-screen window\n", stderr)
exit(1)
SWIFT
}

wait_for_readiness() {
  run_number=$1
  target_pid=$2
  stderr_log=$3
  attempt=0
  while [ "$attempt" -lt 300 ]; do
    if ! kill -0 "$target_pid" 2>/dev/null; then
      echo "application exited before product state became ready" >&2
      return 1
    fi
    case "$COMPONENT" in
      mom)
        [ -f "$PRODUCT_STATE/runtime.sqlite3" ] &&
          grep -Fq "mom-llama runtime ready: $PRODUCT_STATE" "$stderr_log" && return 0
        ;;
      loom)
        loom_root="$PRODUCT_STATE/writing"
        if [ -f "$loom_root/.loom/project.json" ] &&
          [ -f "$loom_root/.loom/loom.sqlite3" ] &&
          [ -f "$loom_root/manuscript/Untitled.md" ]; then
          open_count=$(sqlite3 "$loom_root/.loom/loom.sqlite3" \
            "SELECT count(*) FROM command_receipts WHERE command_kind = 'open_project';" 2>/dev/null || echo 0)
          [ "$open_count" -ge "$run_number" ] && return 0
        fi
        ;;
      fte)
        [ -f "$PRODUCT_STATE/gateway.db" ] &&
          [ -f "$PRODUCT_STATE/gateway-v2.db" ] &&
          grep -Fq "free-token-energy runtime ready: $PRODUCT_STATE_CANONICAL" "$stderr_log" && return 0
        ;;
    esac
    attempt=$((attempt + 1))
    sleep 0.1
  done
  echo "product state did not become ready before the 30-second deadline" >&2
  return 1
}

state_identity() {
  case "$COMPONENT" in
    mom)
      stat -f '%d:%i' "$PRODUCT_STATE/runtime.sqlite3"
      ;;
    loom)
      printf 'open-project-receipt-%s\n' "$1"
      ;;
    fte)
      printf '%s|%s\n' \
        "$(stat -f '%d:%i' "$PRODUCT_STATE/gateway.db")" \
        "$(stat -f '%d:%i' "$PRODUCT_STATE/gateway-v2.db")"
      ;;
  esac
}

quit_through_application_menu() {
  target_pid=$1
  osascript - "$target_pid" "$APP_NAME" <<'APPLESCRIPT'
on run argv
  set targetPid to item 1 of argv as integer
  set appName to item 2 of argv
  tell application "System Events"
    set matches to every application process whose unix id is targetPid
    if (count of matches) is not 1 then error "target application process did not appear"
    set targetProcess to item 1 of matches
    set quitItem to menu item ("Quit " & appName) of menu 1 of menu bar item appName of menu bar 1 of targetProcess
    perform action "AXPress" of quitItem
  end tell
end run
APPLESCRIPT
}

wait_for_clean_exit() {
  target_pid=$1
  attempt=0
  while :; do
    process_state=$(ps -p "$target_pid" -o state= | tr -d ' ')
    case "$process_state" in
      ""|Z*) break ;;
    esac
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 300 ]; then
      echo "application did not exit within 30 seconds after its Cmd-Q menu command (pid $target_pid)" >&2
      return 1
    fi
    sleep 0.1
  done
  wait "$target_pid"
}

run_once() {
  run_number=$1
  stdout_log="$SMOKE_ROOT/launch-$run_number.stdout.log"
  stderr_log="$SMOKE_ROOT/launch-$run_number.stderr.log"
  echo "+ launch $run_number: $EXECUTABLE"
  case "$COMPONENT" in
    mom)
      env LLAMA_NATIVE_KIT_DATA_DIR="$PRODUCT_STATE" \
        LLAMA_NATIVE_KIT_STORE_KEY_HEX=000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f \
        "$EXECUTABLE" >"$stdout_log" 2>"$stderr_log" &
      ;;
    loom)
      env DELYSIS_LOOM_ACCEPTANCE_DIR="$PRODUCT_STATE" \
        "$EXECUTABLE" >"$stdout_log" 2>"$stderr_log" &
      ;;
    fte)
      env DELYSIS_FTE_ACCEPTANCE_DIR="$PRODUCT_STATE" \
        "$EXECUTABLE" >"$stdout_log" 2>"$stderr_log" &
      ;;
  esac
  ACTIVE_PID=$!
  eval "RUN_${run_number}_PID=$ACTIVE_PID"

  if ! wait_for_window "$ACTIVE_PID"; then
    echo "packaged app did not expose a window" >&2
    echo "application logs: $stdout_log and $stderr_log" >&2
    return 1
  fi
  if ! wait_for_readiness "$run_number" "$ACTIVE_PID" "$stderr_log"; then
    echo "application logs: $stdout_log and $stderr_log" >&2
    return 1
  fi
  observed_state_identity=$(state_identity "$run_number")
  case "$run_number" in
    1) RUN_1_STATE_IDENTITY=$observed_state_identity ;;
    2) RUN_2_STATE_IDENTITY=$observed_state_identity ;;
  esac
  if ! quit_through_application_menu "$ACTIVE_PID"; then
    echo "could not activate the app's ordinary Cmd-Q menu item; macOS Accessibility permission may be required" >&2
    echo "application logs: $stdout_log and $stderr_log" >&2
    return 1
  fi
  if ! wait_for_clean_exit "$ACTIVE_PID"; then
    echo "application logs: $stdout_log and $stderr_log" >&2
    return 1
  fi
  case "$COMPONENT" in
    mom)
      if ! grep -F 'mom-llama shutdown: {"Ok":' "$stderr_log" |
        grep -Fq '"native_host_joined":true'; then
        echo "Mom exited without positive native-host join evidence" >&2
        echo "application log: $stderr_log" >&2
        return 1
      fi
      if ! grep -F 'mom-llama shutdown: {"Ok":' "$stderr_log" |
        grep -Fq '"application_work_drained":true'; then
        echo "Mom exited without positive application-drain evidence" >&2
        echo "application log: $stderr_log" >&2
        return 1
      fi
      ;;
    fte)
      if grep -Fq 'Free Token Energy cleanup failed' "$stderr_log"; then
        echo "FTE reported a gateway cleanup failure" >&2
        echo "application log: $stderr_log" >&2
        return 1
      fi
      ;;
  esac
  ACTIVE_PID=
}

run_once 1
run_once 2

case "$COMPONENT" in
  mom|fte)
    if [ "$RUN_1_STATE_IDENTITY" != "$RUN_2_STATE_IDENTITY" ]; then
      echo "product database identity changed between packaged launches" >&2
      exit 1
    fi
    REOPEN_EVIDENCE="same database file identity observed after application readiness on both launches"
    ;;
  loom)
    REOPEN_EVIDENCE="open_project receipt count advanced across launches"
    ;;
esac

EXECUTABLE_SHA256_AFTER=$(shasum -a 256 "$EXECUTABLE" | awk '{print $1}')
if [ "$EXECUTABLE_SHA256_AFTER" != "$EXECUTABLE_SHA256" ]; then
  echo "packaged executable changed while the smoke test was running" >&2
  exit 1
fi
STATE_INVENTORY="$SMOKE_ROOT/state-inventory.txt"
find "$PRODUCT_STATE" -mindepth 1 -print | LC_ALL=C sort > "$STATE_INVENTORY"
RECEIPT="$SMOKE_ROOT/smoke-receipt.json"

DELYSIS_SMOKE_COMPONENT="$COMPONENT" \
DELYSIS_SMOKE_BUNDLE="$BUNDLE" \
DELYSIS_SMOKE_BUNDLE_ID="$BUNDLE_ID" \
DELYSIS_SMOKE_EXECUTABLE_SHA="$EXECUTABLE_SHA256" \
DELYSIS_SMOKE_INPUT_ARCHIVE="$INPUT_ARCHIVE" \
DELYSIS_SMOKE_INPUT_ARCHIVE_SHA="$INPUT_ARCHIVE_SHA256" \
DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT="$INPUT_RELEASE_RECEIPT" \
DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT_SHA="$INPUT_RELEASE_RECEIPT_SHA256" \
DELYSIS_SMOKE_STATE_ROOT="$PRODUCT_STATE" \
DELYSIS_SMOKE_RUN_1_PID="$RUN_1_PID" \
DELYSIS_SMOKE_RUN_2_PID="$RUN_2_PID" \
DELYSIS_SMOKE_REOPEN_EVIDENCE="$REOPEN_EVIDENCE" \
node <<'NODE' > "$RECEIPT"
const e = process.env;
const receipt = {
  schema: "delysis.macos-packaged-app-smoke.v1",
  created_at: new Date().toISOString(),
  component: e.DELYSIS_SMOKE_COMPONENT,
  bundle: e.DELYSIS_SMOKE_BUNDLE,
  bundle_id: e.DELYSIS_SMOKE_BUNDLE_ID,
  executable_sha256: e.DELYSIS_SMOKE_EXECUTABLE_SHA,
  input_archive: e.DELYSIS_SMOKE_INPUT_ARCHIVE || null,
  input_archive_sha256: e.DELYSIS_SMOKE_INPUT_ARCHIVE_SHA || null,
  input_release_receipt: e.DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT || null,
  input_release_receipt_sha256: e.DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT_SHA || null,
  app_owned_state_root: e.DELYSIS_SMOKE_STATE_ROOT,
  launches: [
    { pid: Number(e.DELYSIS_SMOKE_RUN_1_PID), window_observed: true, product_ready_at_state_root: true, quit: "AXPress on Quit menu item with Cmd-Q binding", exit_status: 0 },
    { pid: Number(e.DELYSIS_SMOKE_RUN_2_PID), window_observed: true, product_ready_at_state_root: true, quit: "AXPress on Quit menu item with Cmd-Q binding", exit_status: 0 },
  ],
  app_owned_state_reopened: true,
  state_reopen_evidence: e.DELYSIS_SMOKE_REOPEN_EVIDENCE,
  scope_note: "Product-owned state was isolated. macOS and WKWebView may write framework-managed caches outside this root.",
};
process.stdout.write(`${JSON.stringify(receipt, null, 2)}\n`);
NODE

if [ -n "$RECEIPT_DESTINATION" ]; then
  receipt_parent=$(dirname -- "$RECEIPT_DESTINATION")
  if [ ! -d "$receipt_parent" ]; then
    echo "receipt destination directory does not exist: $receipt_parent" >&2
    exit 1
  fi
  cp "$RECEIPT" "$RECEIPT_DESTINATION"
  RECEIPT_DESTINATION=$(CDPATH= cd -- "$receipt_parent" && pwd)/$(basename -- "$RECEIPT_DESTINATION")
fi

trap - EXIT HUP INT TERM
echo "packaged-app smoke passed twice: $COMPONENT"
echo "smoke evidence: $SMOKE_ROOT"
echo "receipt: $RECEIPT"
if [ -n "$RECEIPT_DESTINATION" ]; then
  echo "copied receipt: $RECEIPT_DESTINATION"
fi
