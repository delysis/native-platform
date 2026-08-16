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
ACTIVE_LAUNCHER_PID=

cleanup_failed_process() {
  if [ -n "$ACTIVE_PID" ] && kill -0 "$ACTIVE_PID" 2>/dev/null; then
    kill "$ACTIVE_PID" 2>/dev/null || true
  fi
  if [ -n "$ACTIVE_LAUNCHER_PID" ] && kill -0 "$ACTIVE_LAUNCHER_PID" 2>/dev/null; then
    kill "$ACTIVE_LAUNCHER_PID" 2>/dev/null || true
    wait "$ACTIVE_LAUNCHER_PID" 2>/dev/null || true
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
    let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)! as! [[String: Any]]
    let ready = rows.contains { row in
        guard (row[kCGWindowOwnerPID as String] as? Int32) == pid,
              (row[kCGWindowLayer as String] as? Int) == 0,
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

drag_window_and_require_delta() {
  target_pid=$1
  xcrun swift - "$target_pid" <<'SWIFT'
import AppKit
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments[1])!

func frame() -> CGRect? {
    let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)! as! [[String: Any]]
    for row in rows {
        guard (row[kCGWindowOwnerPID as String] as? Int32) == pid,
              (row[kCGWindowLayer as String] as? Int) == 0,
              let bounds = row[kCGWindowBounds as String] as? [String: Any],
              let x = bounds["X"] as? Double,
              let y = bounds["Y"] as? Double,
              let width = bounds["Width"] as? Double,
              let height = bounds["Height"] as? Double,
              width > 0, height > 0 else { continue }
        return CGRect(x: x, y: y, width: width, height: height)
    }
    return nil
}

guard let before = frame() else {
    fputs("could not bind titlebar drag to the exact application window\n", stderr)
    exit(1)
}

NSRunningApplication(processIdentifier: pid)?.activate(options: [])
Thread.sleep(forTimeInterval: 0.75)

// The centered native title hit region belongs to AppKit even when its text is
// hidden. Exercise Loom's explicit noninteractive web drag strip to its left.
let start = CGPoint(x: before.minX + before.width * 0.30, y: before.minY + 15)
let horizontal = before.minX + before.width + 72 < 1500 ? 64.0 : -64.0
let vertical = 0.0
let finish = CGPoint(x: start.x + horizontal, y: start.y + vertical)
guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: start, mouseButton: .left),
      let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: finish, mouseButton: .left) else {
    fputs("could not construct titlebar drag events\n", stderr)
    exit(1)
}
down.post(tap: .cghidEventTap)
Thread.sleep(forTimeInterval: 0.30)
for step in 1...8 {
    let fraction = Double(step) / 8.0
    let point = CGPoint(
        x: start.x + horizontal * fraction,
        y: start.y + vertical * fraction
    )
    guard let drag = CGEvent(
        mouseEventSource: nil,
        mouseType: .leftMouseDragged,
        mouseCursorPosition: point,
        mouseButton: .left
    ) else {
        fputs("could not construct an intermediate titlebar drag event\n", stderr)
        exit(1)
    }
    drag.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.08)
}
up.post(tap: .cghidEventTap)

let deadline = Date().addingTimeInterval(4)
var after = before
repeat {
    Thread.sleep(forTimeInterval: 0.1)
    after = frame() ?? before
    if abs(after.minX - before.minX) >= 8 || abs(after.minY - before.minY) >= 8 { break }
} while Date() < deadline

guard abs(after.minX - before.minX) >= 8 || abs(after.minY - before.minY) >= 8 else {
    fputs("titlebar drag was dispatched but the bound window frame did not move\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "pid": pid,
    "before": ["x": before.minX, "y": before.minY, "width": before.width, "height": before.height],
    "after": ["x": after.minX, "y": after.minY, "width": after.width, "height": after.height],
    "delta": ["x": after.minX - before.minX, "y": after.minY - before.minY]
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
SWIFT
}

retry_titlebar_drag_and_require_delta() {
  target_pid=$1
  drag_attempt=1
  while [ "$drag_attempt" -le 3 ]; do
    if drag_evidence=$(drag_window_and_require_delta "$target_pid"); then
      printf '%s\n' "$drag_evidence"
      return 0
    fi
    drag_attempt=$((drag_attempt + 1))
    sleep 0.25
  done
  return 1
}

type_into_loom_editor() {
  target_pid=$1
  sentinel=$2
  xcrun swift - "$target_pid" "$sentinel" <<'SWIFT'
import AppKit
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let sentinel = CommandLine.arguments[2]
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func stringAttribute(_ element: AXUIElement, _ name: CFString) -> String {
    attribute(element, name) as? String ?? ""
}

func findEditor(_ root: AXUIElement) -> AXUIElement? {
    var queue = [root]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let role = stringAttribute(element, kAXRoleAttribute as CFString)
        let description = stringAttribute(element, kAXDescriptionAttribute as CFString)
        let title = stringAttribute(element, kAXTitleAttribute as CFString)
        if role == kAXTextAreaRole as String &&
            (description.contains("editor") || title.contains("editor") || description.isEmpty) {
            return element
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

NSRunningApplication(processIdentifier: pid)?.activate(options: [])
let deadline = Date().addingTimeInterval(10)
var editor: AXUIElement?
repeat {
    editor = findEditor(application)
    if editor != nil { break }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < deadline

guard let editor else {
    fputs("could not find Loom's accessible manuscript text area\n", stderr)
    exit(1)
}

guard AXUIElementSetAttributeValue(editor, kAXFocusedAttribute as CFString, kCFBooleanTrue) == .success else {
    fputs("could not focus Loom's accessible manuscript text area\n", stderr)
    exit(1)
}
Thread.sleep(forTimeInterval: 0.2)

var utf16 = Array(sentinel.utf16)
guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
      let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
    fputs("could not construct manuscript keyboard events\n", stderr)
    exit(1)
}
down.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
up.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
down.post(tap: .cghidEventTap)
up.post(tap: .cghidEventTap)
print("AX-focused text area received native keyboard events")
SWIFT
}

exercise_loom_completion_controls() {
  target_pid=$1
  xcrun swift - "$target_pid" <<'SWIFT'
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func strings(_ element: AXUIElement) -> String {
    [kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute]
        .compactMap { attribute(element, $0 as CFString) as? String }
        .joined(separator: " ")
}

func button(named needle: String) -> AXUIElement? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let role = attribute(element, kAXRoleAttribute as CFString) as? String
        if (role == kAXButtonRole as String || role == kAXCheckBoxRole as String),
           strings(element).contains(needle) {
            return element
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

func waitForButton(_ name: String, timeout: TimeInterval = 5) -> AXUIElement? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if let match = button(named: name) { return match }
        Thread.sleep(forTimeInterval: 0.1)
    } while Date() < deadline
    return nil
}

if let autocompleteOn = button(named: "Turn autocomplete off") {
    guard AXUIElementPerformAction(autocompleteOn, kAXPressAction as CFString) == .success else {
        fputs("could not turn autocomplete off through its exact titlebar control\n", stderr)
        exit(1)
    }
}
guard waitForButton("Turn autocomplete on") != nil else {
    fputs("autocomplete did not expose its independent off state\n", stderr)
    exit(1)
}
guard let shuttle = waitForButton("Turn Shuttle on") else {
    fputs("could not find Shuttle's titlebar control with autocomplete off\n", stderr)
    exit(1)
}
guard (attribute(shuttle, kAXEnabledAttribute as CFString) as? Bool) == true else {
    fputs("Shuttle remained disabled when autocomplete was off\n", stderr)
    exit(1)
}
guard AXUIElementPerformAction(shuttle, kAXPressAction as CFString) == .success else {
    fputs("could not turn Shuttle on independently\n", stderr)
    exit(1)
}
guard waitForButton("Turn Shuttle off") != nil else {
    fputs("Shuttle did not expose its independent on state\n", stderr)
    exit(1)
}
guard let shuttleOn = button(named: "Turn Shuttle off"),
      AXUIElementPerformAction(shuttleOn, kAXPressAction as CFString) == .success,
      waitForButton("Turn Shuttle on") != nil else {
    fputs("Shuttle did not return to its independent off state\n", stderr)
    exit(1)
}
let evidence: [String: Any] = [
    "autocomplete": "off",
    "shuttle_transition": "off-on-off",
    "shuttle_enabled_while_autocomplete_off": true
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
SWIFT
}

create_loom_document_and_require_editor() {
  target_pid=$1
  xcrun swift - "$target_pid" <<'SWIFT'
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func stringAttribute(_ element: AXUIElement, _ name: CFString) -> String {
    attribute(element, name) as? String ?? ""
}

func descendants() -> [AXUIElement] {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return queue
}

func button(named needle: String) -> AXUIElement? {
    descendants().first { element in
        guard stringAttribute(element, kAXRoleAttribute as CFString) == kAXButtonRole as String else {
            return false
        }
        return [
            stringAttribute(element, kAXDescriptionAttribute as CFString),
            stringAttribute(element, kAXTitleAttribute as CFString),
            stringAttribute(element, kAXHelpAttribute as CFString)
        ].joined(separator: " ").contains(needle)
    }
}

guard let window = (attribute(application, kAXWindowsAttribute as CFString) as? [AXUIElement])?.first,
      let create = button(named: "New document") else {
    fputs("could not bind the new-document check to Loom's exact accessible window\n", stderr)
    exit(1)
}
let beforeTitle = stringAttribute(window, kAXTitleAttribute as CFString)
guard AXUIElementPerformAction(create, kAXPressAction as CFString) == .success else {
    fputs("could not press Loom's new-document control\n", stderr)
    exit(1)
}

let deadline = Date().addingTimeInterval(10)
var afterTitle = beforeTitle
var focusedEditor = false
repeat {
    afterTitle = stringAttribute(window, kAXTitleAttribute as CFString)
    focusedEditor = descendants().contains { element in
        stringAttribute(element, kAXRoleAttribute as CFString) == kAXTextAreaRole as String &&
            (attribute(element, kAXFocusedAttribute as CFString) as? Bool) == true
    }
    if afterTitle != beforeTitle && focusedEditor { break }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < deadline

guard afterTitle != beforeTitle, focusedEditor else {
    fputs("new document did not expose and focus a fresh writing surface\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "title_before": beforeTitle,
    "title_after": afterTitle,
    "focused_editor_observed": focusedEditor
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
SWIFT
}

require_loom_manuscript_text() {
  manuscript=$1
  expected=$2
  attempt=0
  while [ "$attempt" -lt 120 ]; do
    if node - "$manuscript" "$expected" <<'NODE'
const fs = require('fs');
const [path, expected] = process.argv.slice(2);
const observed = fs.readFileSync(path, 'utf8');
process.exit(observed.trimEnd() === expected ? 0 : 1);
NODE
    then
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 0.1
  done
  echo "native editor input did not reach the persisted Loom manuscript" >&2
  return 1
}

require_loom_new_manuscript_text() {
  manuscript_root=$1
  original_manuscript=$2
  expected=$3
  attempt=0
  while [ "$attempt" -lt 120 ]; do
    matching_path=$(find "$manuscript_root" -type f -name '*.md' ! -path "$original_manuscript" -print | while IFS= read -r candidate; do
      if node - "$candidate" "$expected" <<'NODE'
const fs = require('fs');
const [path, expected] = process.argv.slice(2);
const observed = fs.readFileSync(path, 'utf8');
process.exit(observed.trimEnd() === expected ? 0 : 1);
NODE
      then
        printf '%s\n' "$candidate"
        break
      fi
    done)
    if [ -n "$matching_path" ]; then
      printf '%s\n' "$matching_path"
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 0.1
  done
  echo "native input did not reach the newly created Loom manuscript" >&2
  return 1
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
  launcher_pid=$2
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
  wait "$launcher_pid"
}

exact_bundle_pid() {
  for candidate_pid in $(pgrep -x "$BINARY_NAME" 2>/dev/null || true); do
    candidate_command=$(ps -p "$candidate_pid" -o command= | sed 's/^ *//')
    if [ "$candidate_command" = "$EXECUTABLE" ]; then
      printf '%s\n' "$candidate_pid"
    fi
  done
}

run_once() {
  run_number=$1
  stdout_log="$SMOKE_ROOT/launch-$run_number.stdout.log"
  stderr_log="$SMOKE_ROOT/launch-$run_number.stderr.log"
  echo "+ launch $run_number: $EXECUTABLE"
  if [ -n "$(exact_bundle_pid)" ]; then
    echo "refusing ambiguous smoke launch while the exact application bundle is already running" >&2
    return 1
  fi
  case "$COMPONENT" in
    mom)
      open -F -n -W -o "$stdout_log" --stderr "$stderr_log" \
        --env "LLAMA_NATIVE_KIT_DATA_DIR=$PRODUCT_STATE" \
        --env LLAMA_NATIVE_KIT_STORE_KEY_HEX=000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f \
        "$BUNDLE" &
      ;;
    loom)
      open -F -n -W -o "$stdout_log" --stderr "$stderr_log" \
        --env "DELYSIS_LOOM_ACCEPTANCE_DIR=$PRODUCT_STATE" "$BUNDLE" &
      ;;
    fte)
      open -F -n -W -o "$stdout_log" --stderr "$stderr_log" \
        --env "DELYSIS_FTE_ACCEPTANCE_DIR=$PRODUCT_STATE" "$BUNDLE" &
      ;;
  esac
  ACTIVE_LAUNCHER_PID=$!
  ACTIVE_PID=
  attempt=0
  while [ "$attempt" -lt 200 ]; do
    ACTIVE_PID=$(exact_bundle_pid | tail -n 1)
    [ -n "$ACTIVE_PID" ] && break
    if ! kill -0 "$ACTIVE_LAUNCHER_PID" 2>/dev/null; then break; fi
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if [ -z "$ACTIVE_PID" ]; then
    echo "LaunchServices did not expose the exact application process" >&2
    return 1
  fi
  echo "+ bound pid: $ACTIVE_PID"
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
  if [ "$COMPONENT" = loom ] && [ "$run_number" -eq 1 ]; then
    loom_manuscript="$PRODUCT_STATE/writing/manuscript/Untitled.md"
    if ! RUN_1_COMPLETION_CONTROLS_EVIDENCE=$(exercise_loom_completion_controls "$ACTIVE_PID"); then
      echo "autocomplete and Shuttle did not behave as independent native controls" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    RUN_1_EDITOR_SENTINEL='Loom native smoke: editor persistence.'
    if ! RUN_1_EDITOR_EVIDENCE=$(type_into_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL"); then
      echo "could not drive the exact app's accessible manuscript editor" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    if ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL"; then
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    RUN_1_MANUSCRIPT_SHA256_AFTER_EDITOR_INPUT=$(shasum -a 256 "$loom_manuscript" | awk '{print $1}')
    if ! RUN_1_NEW_DOCUMENT_EVIDENCE=$(create_loom_document_and_require_editor "$ACTIVE_PID"); then
      echo "new document did not expose a focused writing surface in the exact app" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    RUN_1_NEW_DOCUMENT_SENTINEL='Loom native smoke: new document editor.'
    if ! type_into_loom_editor "$ACTIVE_PID" "$RUN_1_NEW_DOCUMENT_SENTINEL" >/dev/null; then
      echo "could not type into the newly created document's writing surface" >&2
      return 1
    fi
    if ! RUN_1_NEW_DOCUMENT_PATH=$(require_loom_new_manuscript_text \
      "$PRODUCT_STATE/writing/manuscript" \
      "$loom_manuscript" \
      "$RUN_1_NEW_DOCUMENT_SENTINEL"); then
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    RUN_1_MANUSCRIPT_SHA256_BEFORE=$(shasum -a 256 "$loom_manuscript" | awk '{print $1}')
    if ! RUN_1_DRAG_EVIDENCE=$(retry_titlebar_drag_and_require_delta "$ACTIVE_PID"); then
      echo "titlebar drag did not produce an observed window-frame delta for pid $ACTIVE_PID" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    RUN_1_MANUSCRIPT_SHA256_AFTER=$(shasum -a 256 "$loom_manuscript" | awk '{print $1}')
    require_equal "manuscript SHA-256 after titlebar drag" \
      "$RUN_1_MANUSCRIPT_SHA256_BEFORE" "$RUN_1_MANUSCRIPT_SHA256_AFTER"
  elif [ "$COMPONENT" = loom ] && [ "$run_number" -eq 2 ]; then
    loom_manuscript="$PRODUCT_STATE/writing/manuscript/Untitled.md"
    if ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL"; then
      echo "persisted editor input did not reopen on the second exact-bundle launch" >&2
      return 1
    fi
    RUN_2_MANUSCRIPT_SHA256=$(shasum -a 256 "$loom_manuscript" | awk '{print $1}')
    require_equal "reopened manuscript SHA-256" \
      "$RUN_1_MANUSCRIPT_SHA256_AFTER_EDITOR_INPUT" "$RUN_2_MANUSCRIPT_SHA256"
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
  if ! wait_for_clean_exit "$ACTIVE_PID" "$ACTIVE_LAUNCHER_PID"; then
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
  ACTIVE_LAUNCHER_PID=
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
DELYSIS_SMOKE_RUN_1_DRAG_EVIDENCE="${RUN_1_DRAG_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_BEFORE="${RUN_1_MANUSCRIPT_SHA256_BEFORE:-}" \
DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER="${RUN_1_MANUSCRIPT_SHA256_AFTER:-}" \
DELYSIS_SMOKE_RUN_1_COMPLETION_CONTROLS_EVIDENCE="${RUN_1_COMPLETION_CONTROLS_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_EDITOR_EVIDENCE="${RUN_1_EDITOR_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_EDITOR_SENTINEL="${RUN_1_EDITOR_SENTINEL:-}" \
DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER_EDITOR_INPUT="${RUN_1_MANUSCRIPT_SHA256_AFTER_EDITOR_INPUT:-}" \
DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_EVIDENCE="${RUN_1_NEW_DOCUMENT_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_PATH="${RUN_1_NEW_DOCUMENT_PATH:-}" \
DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_SENTINEL="${RUN_1_NEW_DOCUMENT_SENTINEL:-}" \
DELYSIS_SMOKE_RUN_2_MANUSCRIPT_SHA="${RUN_2_MANUSCRIPT_SHA256:-}" \
DELYSIS_SMOKE_REOPEN_EVIDENCE="$REOPEN_EVIDENCE" \
node <<'NODE' > "$RECEIPT"
const e = process.env;
const titlebarDrag = e.DELYSIS_SMOKE_RUN_1_DRAG_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_DRAG_EVIDENCE)
  : null;
const completionControls = e.DELYSIS_SMOKE_RUN_1_COMPLETION_CONTROLS_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_COMPLETION_CONTROLS_EVIDENCE)
  : null;
const newDocument = e.DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_EVIDENCE)
  : null;
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
    {
      pid: Number(e.DELYSIS_SMOKE_RUN_1_PID),
      window_observed: true,
      product_ready_at_state_root: true,
      titlebar_drag: titlebarDrag,
      manuscript_sha256_before_drag: e.DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_BEFORE || null,
      manuscript_sha256_after_drag: e.DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER || null,
      completion_controls: completionControls,
      editor_input: e.DELYSIS_SMOKE_RUN_1_EDITOR_EVIDENCE ? {
        dispatch: e.DELYSIS_SMOKE_RUN_1_EDITOR_EVIDENCE,
        sentinel: e.DELYSIS_SMOKE_RUN_1_EDITOR_SENTINEL,
        manuscript_sha256_after_input: e.DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER_EDITOR_INPUT,
      } : null,
      new_document: newDocument ? {
        ...newDocument,
        path: e.DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_PATH,
        sentinel: e.DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_SENTINEL,
      } : null,
      quit: "AXPress on Quit menu item with Cmd-Q binding",
      exit_status: 0
    },
    {
      pid: Number(e.DELYSIS_SMOKE_RUN_2_PID),
      window_observed: true,
      product_ready_at_state_root: true,
      reopened_manuscript_sha256: e.DELYSIS_SMOKE_RUN_2_MANUSCRIPT_SHA || null,
      quit: "AXPress on Quit menu item with Cmd-Q binding",
      exit_status: 0
    },
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
