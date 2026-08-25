#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
COMPONENT=${1:-}
SUPPLIED_ARTIFACT=${2:-}
RECEIPT_DESTINATION=${3:-}
LOOM_SMOKE_GGUF_MODEL_PATH=${LOOM_SMOKE_GGUF_MODEL_PATH:-}
LOOM_SMOKE_REAL_COMPLETIONS=${LOOM_SMOKE_REAL_COMPLETIONS:-}
MOM_ACCEPTANCE_PRODUCT_NAME=${MOM_ACCEPTANCE_PRODUCT_NAME:-}
MOM_ACCEPTANCE_BUNDLE_ID=${MOM_ACCEPTANCE_BUNDLE_ID:-}
MOM_ACCEPTANCE_SOURCE_SHA=${MOM_ACCEPTANCE_SOURCE_SHA:-}

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

if [ -n "$MOM_ACCEPTANCE_PRODUCT_NAME" ] || [ -n "$MOM_ACCEPTANCE_BUNDLE_ID" ]; then
  if [ "$COMPONENT" != mom ]; then
    echo "Mom acceptance identity overrides are only valid for the Mom component" >&2
    exit 2
  fi
  if [ -z "$MOM_ACCEPTANCE_PRODUCT_NAME" ] || [ -z "$MOM_ACCEPTANCE_BUNDLE_ID" ]; then
    echo "Mom acceptance product name and bundle ID must be supplied together" >&2
    exit 2
  fi
  if [ "${#MOM_ACCEPTANCE_SOURCE_SHA}" -ne 40 ]; then
    echo "Mom acceptance source SHA must be a full 40-character Git object ID" >&2
    exit 2
  fi
  case "$MOM_ACCEPTANCE_SOURCE_SHA" in
    *[!0-9a-f]*)
      echo "Mom acceptance source SHA must contain only lowercase hexadecimal digits" >&2
      exit 2
      ;;
  esac
  case "$MOM_ACCEPTANCE_PRODUCT_NAME" in
    "Mom Llama Acceptance "*) ;;
    *)
      echo "Mom acceptance product name must begin with 'Mom Llama Acceptance '" >&2
      exit 2
      ;;
  esac
  case "$MOM_ACCEPTANCE_PRODUCT_NAME" in
    *[!A-Za-z0-9._\ -]*)
      echo "Mom acceptance product name contains an unsafe character" >&2
      exit 2
      ;;
  esac
  case "$MOM_ACCEPTANCE_BUNDLE_ID" in
    com.delysis.mom-llama.acceptance.*) ;;
    *)
      echo "Mom acceptance bundle ID must begin with 'com.delysis.mom-llama.acceptance.'" >&2
      exit 2
      ;;
  esac
  case "$MOM_ACCEPTANCE_BUNDLE_ID" in
    *[!A-Za-z0-9.-]*)
      echo "Mom acceptance bundle ID contains an unsafe character" >&2
      exit 2
      ;;
  esac
  APP_NAME=$MOM_ACCEPTANCE_PRODUCT_NAME
  BUNDLE_ID=$MOM_ACCEPTANCE_BUNDLE_ID
elif [ -n "$MOM_ACCEPTANCE_SOURCE_SHA" ]; then
  echo "Mom acceptance source SHA requires the unique product name and bundle ID" >&2
  exit 2
fi

if [ -n "$LOOM_SMOKE_GGUF_MODEL_PATH" ] && [ ! -f "$LOOM_SMOKE_GGUF_MODEL_PATH" ]; then
  echo "LOOM_SMOKE_GGUF_MODEL_PATH is not a model file: $LOOM_SMOKE_GGUF_MODEL_PATH" >&2
  exit 1
fi
if [ -n "$LOOM_SMOKE_GGUF_MODEL_PATH" ]; then
  LOOM_SMOKE_REAL_COMPLETIONS=1
fi

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

if [ ! -d "$BUNDLE" ]; then
  echo "expected packaged application is missing or incomplete: $BUNDLE" >&2
  exit 1
fi
# LaunchServices reports executable commands through the physical `/private`
# path while mktemp commonly returns its `/var` alias. Canonicalize the bundle
# before launching so exact-PID binding compares one filesystem identity.
BUNDLE=$(CDPATH= cd -- "$BUNDLE" && pwd -P)
EXECUTABLE="$BUNDLE/Contents/MacOS/$BINARY_NAME"
PLIST="$BUNDLE/Contents/Info.plist"
if [ ! -x "$EXECUTABLE" ] || [ ! -f "$PLIST" ]; then
  echo "expected packaged application is missing or incomplete: $BUNDLE" >&2
  exit 1
fi

EXECUTABLE_SHA256=$(shasum -a 256 "$EXECUTABLE" | awk '{print $1}')
EXECUTABLE_FILE_ID=$(stat -Lf '%d:%i' "$EXECUTABLE")
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

reject_legacy_mom_bundle_ui() {
  [ "$COMPONENT" = mom ] || return 0
  for legacy_text in \
    "Gentle explainer" \
    "Warm, plain language" \
    "Explain simply and warmly." \
    "Create Skill" \
    "No Skills yet." \
    "Policy: disabled until verified" \
    "KV-cache persistence is surfaced"; do
    if LC_ALL=C grep -R -a -F -q -- "$legacy_text" "$BUNDLE/Contents"; then
      echo "refusing legacy Mom bundle containing obsolete UI text: $legacy_text" >&2
      exit 1
    fi
  done
}

reject_legacy_mom_bundle_ui

PRODUCT_STATE="$SMOKE_ROOT/product"
mkdir "$PRODUCT_STATE"
PRODUCT_STATE_CANONICAL=$(CDPATH= cd -- "$PRODUCT_STATE" && pwd -P)
ACTIVE_PID=
ACTIVE_LAUNCHER_PID=
LOOM_SMOKE_MODEL_LINK=
LOOM_PROJECT_BUSY_MONITOR_PID=
LOOM_PROJECT_BUSY_MONITOR_STOP=
LOOM_GENERATION_GUARD_PID=
LOOM_GENERATION_GUARD_STOP=

settle_background_monitor_for_cleanup() {
  monitor_pid=$1
  stop_path=$2
  [ -n "$monitor_pid" ] || return 0
  kill -0 "$monitor_pid" 2>/dev/null || return 0
  if [ -n "$stop_path" ]; then touch "$stop_path"; fi
  settle_attempt=0
  while kill -0 "$monitor_pid" 2>/dev/null && [ "$settle_attempt" -lt 100 ]; do
    settle_attempt=$((settle_attempt + 1))
    sleep 0.05
  done
  if kill -0 "$monitor_pid" 2>/dev/null; then
    kill "$monitor_pid" 2>/dev/null || true
  fi
  wait "$monitor_pid" 2>/dev/null || true
}

cleanup_failed_process() {
  settle_background_monitor_for_cleanup \
    "$LOOM_PROJECT_BUSY_MONITOR_PID" "$LOOM_PROJECT_BUSY_MONITOR_STOP"
  settle_background_monitor_for_cleanup \
    "$LOOM_GENERATION_GUARD_PID" "$LOOM_GENERATION_GUARD_STOP"
  if [ -n "$ACTIVE_PID" ] && kill -0 "$ACTIVE_PID" 2>/dev/null; then
    kill "$ACTIVE_PID" 2>/dev/null || true
  fi
  if [ -n "$ACTIVE_LAUNCHER_PID" ] && kill -0 "$ACTIVE_LAUNCHER_PID" 2>/dev/null; then
    kill "$ACTIVE_LAUNCHER_PID" 2>/dev/null || true
    wait "$ACTIVE_LAUNCHER_PID" 2>/dev/null || true
  fi
  if [ -n "$LOOM_SMOKE_MODEL_LINK" ] && [ -f "$LOOM_SMOKE_MODEL_LINK" ]; then
    unlink "$LOOM_SMOKE_MODEL_LINK"
  fi
}
trap cleanup_failed_process EXIT HUP INT TERM

if [ "$COMPONENT" = loom ] && [ -n "$LOOM_SMOKE_GGUF_MODEL_PATH" ]; then
  model_library="$PRODUCT_STATE/models"
  mkdir -p "$model_library"
  LOOM_SMOKE_MODEL_LINK="$model_library/gemma-4-12B-it-qat-q4_0.gguf"
  if [ -e "$LOOM_SMOKE_MODEL_LINK" ]; then
    echo "isolated acceptance model target already exists: $LOOM_SMOKE_MODEL_LINK" >&2
    exit 1
  fi
  ln "$LOOM_SMOKE_GGUF_MODEL_PATH" "$LOOM_SMOKE_MODEL_LINK"
  require_equal "acceptance model hard-link identity" \
    "$(stat -Lf '%d:%i' "$LOOM_SMOKE_GGUF_MODEL_PATH")" \
    "$(stat -Lf '%d:%i' "$LOOM_SMOKE_MODEL_LINK")"
fi

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

require_mom_ui_identity() {
  target_pid=$1
  xcrun swift - "$target_pid" <<'SWIFT'
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let application = AXUIElementCreateApplication(pid)
let currentMarkers = [
    "Mention a persona or chat",
    "Personas, chats, and consult groups"
]
let legacyMarkers = [
    "Gentle explainer",
    "Warm, plain language",
    "Explain simply and warmly.",
    "Create Skill",
    "No Skills yet.",
    "Policy: disabled until verified",
    "KV-cache persistence is surfaced"
]
let stringAttributes = [
    kAXTitleAttribute as String,
    kAXDescriptionAttribute as String,
    kAXHelpAttribute as String,
    kAXValueAttribute as String,
    "AXPlaceholderValue"
]

func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else {
        return nil
    }
    return value
}

func accessibilityStrings() -> [String] {
    var queue = [application]
    var cursor = 0
    var observed: [String] = []
    while cursor < queue.count && cursor < 8192 {
        let element = queue[cursor]
        cursor += 1
        for name in stringAttributes {
            if let value = attribute(element, name) as? String, !value.isEmpty {
                observed.append(value)
            }
        }
        if let children = attribute(element, kAXChildrenAttribute as String) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return observed
}

let deadline = Date().addingTimeInterval(20)
repeat {
    let strings = accessibilityStrings()
    if let legacy = legacyMarkers.first(where: { needle in
        strings.contains(where: { $0.localizedCaseInsensitiveContains(needle) })
    }) {
        fputs("legacy Mom UI text is visible in the exact PID: \(legacy)\n", stderr)
        exit(1)
    }
    if let current = currentMarkers.first(where: { needle in
        strings.contains(where: { $0.localizedCaseInsensitiveContains(needle) })
    }) {
        let evidence: [String: Any] = [
            "pid": pid,
            "current_marker": current,
            "legacy_markers_observed": 0,
            "accessibility_strings_scanned": strings.count
        ]
        let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
        print(String(data: data, encoding: .utf8)!)
        exit(0)
    }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < deadline

fputs("the exact Mom PID did not expose the current Persona-menu accessibility marker\n", stderr)
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

func pointAttribute(_ element: AXUIElement, _ name: CFString) -> CGPoint? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else {
        return nil
    }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cgPoint else { return nil }
    var point = CGPoint.zero
    return AXValueGetValue(value, .cgPoint, &point) ? point : nil
}

func sizeAttribute(_ element: AXUIElement, _ name: CFString) -> CGSize? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else {
        return nil
    }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cgSize else { return nil }
    var size = CGSize.zero
    return AXValueGetValue(value, .cgSize, &size) ? size : nil
}

func rangeAttribute(_ element: AXUIElement, _ name: CFString) -> CFRange? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else {
        return nil
    }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cfRange else { return nil }
    var range = CFRange()
    return AXValueGetValue(value, .cfRange, &range) ? range : nil
}

func frame(_ element: AXUIElement) -> CGRect? {
    guard let origin = pointAttribute(element, kAXPositionAttribute as CFString),
          let size = sizeAttribute(element, kAXSizeAttribute as CFString) else {
        return nil
    }
    return CGRect(origin: origin, size: size)
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
var editor: AXUIElement?
for _ in 0..<600 {
    editor = findEditor(application)
    if editor != nil { break }
    Thread.sleep(forTimeInterval: 0.1)
}

guard let editor else {
    fputs("could not find Loom's accessible manuscript text area\n", stderr)
    exit(1)
}

guard let window = (attribute(application, kAXWindowsAttribute as CFString) as? [AXUIElement])?.first,
      let windowFrame = frame(window),
      let editorFrame = frame(editor) else {
    fputs("could not read Loom's window and manuscript-editor frames\n", stderr)
    exit(1)
}
let visibleEditorFrame = windowFrame.intersection(editorFrame)
guard !visibleEditorFrame.isNull,
      visibleEditorFrame.width >= 100,
      visibleEditorFrame.height >= 40 else {
    fputs("Loom exposed an accessible editor that was not visibly laid out in its window\n", stderr)
    exit(1)
}

guard let selectAllDown = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
      let selectAllUp = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
    fputs("could not construct manuscript keyboard events\n", stderr)
    exit(1)
}
selectAllDown.flags = [.maskCommand]
selectAllUp.flags = [.maskCommand]

func typeString(_ value: String) -> Bool {
    for character in value {
        var utf16 = Array(String(character).utf16)
        guard let characterDown = CGEvent(
                keyboardEventSource: nil,
                virtualKey: 0,
                keyDown: true
              ),
              let characterUp = CGEvent(
                keyboardEventSource: nil,
                virtualKey: 0,
                keyDown: false
              ) else {
            return false
        }
        characterDown.keyboardSetUnicodeString(
            stringLength: utf16.count,
            unicodeString: &utf16
        )
        characterUp.keyboardSetUnicodeString(
            stringLength: utf16.count,
            unicodeString: &utf16
        )
        characterDown.postToPid(pid)
        characterUp.postToPid(pid)
    }
    return true
}

// Product-state readiness can precede the Svelte document-open transition,
// especially while a large default writer is being inspected or loaded. A
// dispatched key is not evidence. Retry an idempotent Select-All + type until
// the exact AX value changes and remains observable in the bound editor.
var observedEditorValue = ""
for _ in 0..<60 {
    guard AXUIElementSetAttributeValue(
        editor,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
    ) == .success else {
        fputs("could not focus Loom's accessible manuscript text area\n", stderr)
        exit(1)
    }
    selectAllDown.postToPid(pid)
    selectAllUp.postToPid(pid)
    Thread.sleep(forTimeInterval: 0.05)
    guard typeString(sentinel) else {
        fputs("could not construct the complete manuscript keyboard sequence\n", stderr)
        exit(1)
    }

    for _ in 0..<20 {
        observedEditorValue = stringAttribute(editor, kAXValueAttribute as CFString)
        if observedEditorValue.trimmingCharacters(in: .newlines) == sentinel { break }
        Thread.sleep(forTimeInterval: 0.05)
    }
    if observedEditorValue.trimmingCharacters(in: .newlines) == sentinel { break }
    Thread.sleep(forTimeInterval: 0.25)
}

guard observedEditorValue.trimmingCharacters(in: .newlines) == sentinel else {
    fputs("native keyboard input never produced the exact observable editor value\n", stderr)
    exit(1)
}
var observedSelection: CFRange?
for _ in 0..<20 {
    observedSelection = rangeAttribute(editor, kAXSelectedTextRangeAttribute as CFString)
    if observedSelection?.location == sentinel.utf16.count && observedSelection?.length == 0 { break }
    Thread.sleep(forTimeInterval: 0.05)
}
guard let observedSelection,
      observedSelection.location == sentinel.utf16.count,
      observedSelection.length == 0 else {
    fputs("native keyboard input did not leave one collapsed caret at the manuscript end\n", stderr)
    exit(1)
}
let evidence: [String: Any] = [
    "dispatch": "PID-targeted Select-All and native keyboard input",
    "observed_editor_value": true,
    "observed_editor_utf8_bytes": observedEditorValue.lengthOfBytes(using: .utf8),
    "observed_caret_utf16": observedSelection.location,
    "editor_frame": [
        "x": editorFrame.minX,
        "y": editorFrame.minY,
        "width": editorFrame.width,
        "height": editorFrame.height
    ],
    "visible_editor_frame": [
        "x": visibleEditorFrame.minX,
        "y": visibleEditorFrame.minY,
        "width": visibleEditorFrame.width,
        "height": visibleEditorFrame.height
    ]
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
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

func supportsPress(_ element: AXUIElement) -> Bool {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success,
          let actions = names as? [String] else { return false }
    return actions.contains(kAXPressAction as String)
}

func button(named needle: String) -> AXUIElement? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let role = attribute(element, kAXRoleAttribute as CFString) as? String
        if (role == kAXButtonRole as String || role == kAXCheckBoxRole as String || supportsPress(element)),
           strings(element).contains(needle),
           (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false {
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

let settledDeadline = Date().addingTimeInterval(90)
var autocompleteSettledOff = false
repeat {
    if button(named: "Turn autocomplete on") != nil {
        autocompleteSettledOff = true
        break
    }
    if let autocompleteOn = button(named: "Turn autocomplete off"),
       AXUIElementPerformAction(autocompleteOn, kAXPressAction as CFString) == .success {
        autocompleteSettledOff = waitForButton("Turn autocomplete on", timeout: 15) != nil
        break
    }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < settledDeadline
guard autocompleteSettledOff else {
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

exercise_loom_formatting_palette() {
  target_pid=$1
  action_name=$2
  link_destination=${3:-}
  xcrun swift - "$target_pid" "$action_name" "$link_destination" <<'SWIFT'
import AppKit
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let actionName = CommandLine.arguments[2]
let linkDestination = CommandLine.arguments[3]
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func strings(_ element: AXUIElement) -> [String] {
    [kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute]
        .compactMap { attribute(element, $0 as CFString) as? String }
        .filter { !$0.isEmpty }
}

func supportsPress(_ element: AXUIElement) -> Bool {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success,
          let actions = names as? [String] else { return false }
    return actions.contains(kAXPressAction as String)
}

func rangeAttribute(_ element: AXUIElement, _ name: CFString) -> CFRange? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else {
        return nil
    }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cfRange else { return nil }
    var range = CFRange()
    return AXValueGetValue(value, .cfRange, &range) ? range : nil
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

func press(_ element: AXUIElement) -> Bool {
    supportsPress(element) &&
        AXUIElementPerformAction(element, kAXPressAction as CFString) == .success
}

func button(named needle: String) -> AXUIElement? {
    descendants().first { element in
        strings(element).contains(needle) &&
            supportsPress(element) &&
            (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false
    }
}

func waitForButton(_ name: String, timeout: TimeInterval = 5) -> AXUIElement? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if let match = button(named: name) { return match }
        Thread.sleep(forTimeInterval: 0.1)
    } while Date() < deadline
    return nil
}

func textField(named needle: String) -> AXUIElement? {
    descendants().first { element in
        strings(element).contains(needle) &&
            (attribute(element, kAXRoleAttribute as CFString) as? String) == kAXTextFieldRole as String &&
            (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false
    }
}

guard let editor = descendants().first(where: {
    (attribute($0, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String
}),
      let beforeSelection = rangeAttribute(editor, kAXSelectedTextRangeAttribute as CFString) else {
    fputs("could not bind the formatting action to Loom's accessible manuscript selection\n", stderr)
    exit(1)
}

NSRunningApplication(processIdentifier: pid)?.activate(options: [])
// Link is intentionally disabled until its destination is valid, so it
// cannot witness whether the palette is open. Its stable text field can.
if textField(named: "Link destination") == nil {
    guard let format = waitForButton("Format text"), press(format) else {
        fputs("could not open Loom's formatting palette through its exact titlebar control\n", stderr)
        exit(1)
    }
}

if actionName == "Link" {
    let destinationDeadline = Date().addingTimeInterval(5)
    var destination: AXUIElement?
    repeat {
        destination = textField(named: "Link destination")
        if destination != nil { break }
        Thread.sleep(forTimeInterval: 0.1)
    } while Date() < destinationDeadline
    guard let destination,
          AXUIElementSetAttributeValue(
            destination,
            kAXFocusedAttribute as CFString,
            kCFBooleanTrue
          ) == .success,
          let selectAllDown = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
          let selectAllUp = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
        fputs("could not focus Loom's exact Link destination field through Accessibility\n", stderr)
        exit(1)
    }
    selectAllDown.flags = [.maskCommand]
    selectAllUp.flags = [.maskCommand]
    selectAllDown.postToPid(pid)
    selectAllUp.postToPid(pid)
    for character in linkDestination {
        var utf16 = Array(String(character).utf16)
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
            fputs("could not construct Loom's PID-targeted Link destination input\n", stderr)
            exit(1)
        }
        down.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
        up.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
        down.postToPid(pid)
        up.postToPid(pid)
    }
    let valueDeadline = Date().addingTimeInterval(5)
    while Date() < valueDeadline {
        if (attribute(destination, kAXValueAttribute as CFString) as? String) == linkDestination,
           button(named: "Link") != nil { break }
        Thread.sleep(forTimeInterval: 0.05)
    }
    guard (attribute(destination, kAXValueAttribute as CFString) as? String) == linkDestination,
          button(named: "Link") != nil else {
        fputs("Loom did not bind the PID-targeted Link destination or enable its action\n", stderr)
        exit(1)
    }
}

guard let action = waitForButton(actionName), press(action) else {
    fputs("could not invoke \(actionName) from Loom's open formatting palette\n", stderr)
    exit(1)
}

let focusDeadline = Date().addingTimeInterval(5)
var afterSelection: CFRange?
var editorFocused = false
repeat {
    editorFocused = (attribute(editor, kAXFocusedAttribute as CFString) as? Bool) == true
    afterSelection = rangeAttribute(editor, kAXSelectedTextRangeAttribute as CFString)
    if editorFocused && afterSelection != nil { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < focusDeadline
guard editorFocused, let afterSelection else {
    fputs("Loom's formatting palette did not restore focus and selection to the manuscript editor\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "control_path": "Format text -> \(actionName)",
    "dispatch": "AXPress on exact accessible controls bound to the target PID",
    "link_destination": linkDestination.isEmpty ? NSNull() : linkDestination,
    "selection_before": ["location": beforeSelection.location, "length": beforeSelection.length],
    "selection_after": ["location": afterSelection.location, "length": afterSelection.length],
    "editor_refocused": true
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
SWIFT
}

select_all_in_loom_editor() {
  target_pid=$1
  expected=$2
  xcrun swift - "$target_pid" "$expected" <<'SWIFT'
import AppKit
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let expected = CommandLine.arguments[2]
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func rangeAttribute(_ element: AXUIElement, _ name: CFString) -> CFRange? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cfRange else { return nil }
    var range = CFRange()
    return AXValueGetValue(value, .cfRange, &range) ? range : nil
}

func editor() -> AXUIElement? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        if (attribute(element, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String {
            return element
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

guard let writingSurface = editor(),
      AXUIElementSetAttributeValue(
        writingSurface,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
      ) == .success,
      let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
      let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
    fputs("could not focus Loom's exact editor for Select-All\n", stderr)
    exit(1)
}
down.flags = [.maskCommand]
up.flags = [.maskCommand]
down.postToPid(pid)
up.postToPid(pid)

let deadline = Date().addingTimeInterval(5)
var observed: CFRange?
repeat {
    observed = rangeAttribute(writingSurface, kAXSelectedTextRangeAttribute as CFString)
    if observed?.location == 0 && observed?.length == expected.utf16.count { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < deadline
guard let observed, observed.location == 0, observed.length == expected.utf16.count else {
    fputs("Loom's exact editor did not retain the full manuscript selection\n", stderr)
    exit(1)
}
let evidence: [String: Any] = [
    "dispatch": "PID-targeted Command-A",
    "selection": ["location": observed.location, "length": observed.length]
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
SWIFT
}

require_loom_editor_state() {
  target_pid=$1
  expected=$2
  selection_mode=$3
  xcrun swift - "$target_pid" "$expected" "$selection_mode" <<'SWIFT'
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let expected = CommandLine.arguments[2]
let selectionMode = CommandLine.arguments[3]
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func rangeAttribute(_ element: AXUIElement, _ name: CFString) -> CFRange? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cfRange else { return nil }
    var range = CFRange()
    return AXValueGetValue(value, .cfRange, &range) ? range : nil
}

func editor() -> AXUIElement? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        if (attribute(element, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String {
            return element
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

func withoutTerminalLineBreaks(_ value: String) -> String {
    var normalized = value
    while normalized.last == "\n" || normalized.last == "\r" { normalized.removeLast() }
    return normalized
}

let deadline = Date().addingTimeInterval(12)
var writingSurface: AXUIElement?
var observedValue = ""
var observedSelection: CFRange?
var focused = false
repeat {
    writingSurface = editor()
    if let current = writingSurface {
        observedValue = withoutTerminalLineBreaks(
            (attribute(current, kAXValueAttribute as CFString) as? String) ?? ""
        )
        observedSelection = rangeAttribute(current, kAXSelectedTextRangeAttribute as CFString)
        focused = (attribute(current, kAXFocusedAttribute as CFString) as? Bool) == true
    } else {
        observedValue = ""
        observedSelection = nil
        focused = false
    }
    let selectionMatches: Bool
    switch selectionMode {
    case "caret-end":
        selectionMatches = observedSelection?.location == expected.utf16.count && observedSelection?.length == 0
    case "select-all":
        selectionMatches = observedSelection?.location == 0 && observedSelection?.length == expected.utf16.count
    default:
        selectionMatches = observedSelection != nil
    }
    if observedValue == expected && focused && selectionMatches { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < deadline

let selectionMatches: Bool
switch selectionMode {
case "caret-end":
    selectionMatches = observedSelection?.location == expected.utf16.count && observedSelection?.length == 0
case "select-all":
    selectionMatches = observedSelection?.location == 0 && observedSelection?.length == expected.utf16.count
default:
    selectionMatches = observedSelection != nil
}
guard writingSurface != nil,
      observedValue == expected,
      focused,
      selectionMatches,
      let observedSelection else {
    let location = observedSelection?.location ?? -1
    let length = observedSelection?.length ?? -1
    fputs(
        "Loom's live AX editor diverged from the exact canonical manuscript or lost focus/selection " +
        "(value=\(String(reflecting: observedValue)), expected=\(String(reflecting: expected)), " +
        "focused=\(focused), selection=\(location):\(length), mode=\(selectionMode))\n",
        stderr
    )
    exit(1)
}
let evidence: [String: Any] = [
    "canonical_editor_value": observedValue,
    "focused": true,
    "selection_mode": selectionMode,
    "selection": ["location": observedSelection.location, "length": observedSelection.length]
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
SWIFT
}

set_loom_completion_toggle() {
  target_pid=$1
  control_name=$2
  already_name=$3
  press_requirement=${4:-allow-already}
  xcrun swift - "$target_pid" "$control_name" "$already_name" "$press_requirement" <<'SWIFT'
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let controlName = CommandLine.arguments[2]
let alreadyName = CommandLine.arguments[3]
let requirePress = CommandLine.arguments[4] == "require-press"
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

var pressed = false
for _ in 0..<1800 {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let description = strings(element)
        let enabled = (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false
        if enabled {
            if description.contains(alreadyName) {
                guard pressed || !requirePress else {
                    fputs("Loom completion control reached the requested state without the required single press: \(controlName)\n", stderr)
                    exit(1)
                }
                let evidence: [String: Any] = [
                    "requested_control": controlName,
                    "resulting_control": alreadyName,
                    "pressed_exactly_once": pressed
                ]
                let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
                print(String(data: data, encoding: .utf8)!)
                exit(0)
            }
            if !pressed,
               description.contains(controlName),
               AXUIElementPerformAction(element, kAXPressAction as CFString) == .success {
                pressed = true
            }
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    Thread.sleep(forTimeInterval: 0.1)
}
fputs("could not press Loom completion control: \(controlName)\n", stderr)
exit(1)
SWIFT
}

wait_for_loom_accessibility_text() {
  target_pid=$1
  expected=$2
  generation_failure=${3:-}
  project_busy_failure=${4:-}
  xcrun swift - \
    "$target_pid" "$expected" "$generation_failure" "$project_busy_failure" <<'SWIFT'
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let expected = CommandLine.arguments[2]
let asynchronousFailurePaths = [CommandLine.arguments[3], CommandLine.arguments[4]]
    .filter { !$0.isEmpty }
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

for _ in 0..<1800 {
    if asynchronousFailurePaths.contains(where: { FileManager.default.fileExists(atPath: $0) }) {
        fputs("Loom failed an asynchronous generation or project_busy guard before the required accessible state\n", stderr)
        exit(1)
    }
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let strings = [kAXDescriptionAttribute, kAXTitleAttribute, kAXValueAttribute]
            .compactMap { attribute(element, $0 as CFString) as? String }
        if strings.contains(where: { $0.contains(expected) }) {
            print(expected)
            exit(0)
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    Thread.sleep(forTimeInterval: 0.2)
}
fputs("Loom never exposed the required accessible runtime state: \(expected)\n", stderr)
exit(1)
SWIFT
}

loom_completion_control_state() {
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

var queue = [application]
var cursor = 0
while cursor < queue.count && cursor < 4096 {
    let element = queue[cursor]
    cursor += 1
    let values = [kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute, kAXValueAttribute]
        .compactMap { attribute(element, $0 as CFString) as? String }
        .filter { !$0.isEmpty }
    if values.contains(where: { $0.contains("Turn autocomplete") }) {
        let evidence: [String: Any] = [
            "description": values,
            "enabled": (attribute(element, kAXEnabledAttribute as CFString) as? Bool) ?? false
        ]
        let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
        print(String(data: data, encoding: .utf8)!)
        exit(0)
    }
    if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
        queue.append(contentsOf: children)
    }
}
fputs("could not find Loom's autocomplete control\n", stderr)
exit(1)
SWIFT
}

start_loom_project_busy_monitor() {
  target_pid=$1
  monitor_name=$2
  LOOM_PROJECT_BUSY_MONITOR_STOP="$SMOKE_ROOT/$monitor_name.stop"
  LOOM_PROJECT_BUSY_MONITOR_READY="$SMOKE_ROOT/$monitor_name.ready"
  LOOM_PROJECT_BUSY_MONITOR_FAILURE="$SMOKE_ROOT/$monitor_name.failure.json"
  LOOM_PROJECT_BUSY_MONITOR_OUTPUT="$SMOKE_ROOT/$monitor_name.evidence.json"
  LOOM_PROJECT_BUSY_MONITOR_ERROR="$SMOKE_ROOT/$monitor_name.stderr.log"
  rm -f \
    "$LOOM_PROJECT_BUSY_MONITOR_STOP" \
    "$LOOM_PROJECT_BUSY_MONITOR_READY" \
    "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" \
    "$LOOM_PROJECT_BUSY_MONITOR_OUTPUT" \
    "$LOOM_PROJECT_BUSY_MONITOR_ERROR"
  xcrun swift - \
    "$target_pid" \
    "$LOOM_PROJECT_BUSY_MONITOR_STOP" \
    "$LOOM_PROJECT_BUSY_MONITOR_READY" \
    "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" \
    >"$LOOM_PROJECT_BUSY_MONITOR_OUTPUT" \
    2>"$LOOM_PROJECT_BUSY_MONITOR_ERROR" <<'SWIFT' &
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let stopPath = CommandLine.arguments[2]
let readyPath = CommandLine.arguments[3]
let failurePath = CommandLine.arguments[4]
let application = AXUIElementCreateApplication(pid)
let manager = FileManager.default
let startedAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
var polls = 0

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func projectBusyMatch() -> [String: Any]? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let values = [kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute, kAXValueAttribute]
            .compactMap { attribute(element, $0 as CFString) as? String }
            .filter { !$0.isEmpty }
        if values.contains(where: {
            $0.localizedCaseInsensitiveContains("project_busy") ||
                $0.localizedCaseInsensitiveContains("another bounded project operation is still running")
        }) {
            return [
                "role": (attribute(element, kAXRoleAttribute as CFString) as? String) ?? "",
                "strings": values
            ]
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

_ = manager.createFile(atPath: readyPath, contents: Data())
while !manager.fileExists(atPath: stopPath) {
    polls += 1
    if let match = projectBusyMatch() {
        let evidence: [String: Any] = [
            "pid": pid,
            "started_at_ms": startedAtMs,
            "detected_at_ms": Int64(Date().timeIntervalSince1970 * 1_000),
            "polls": polls,
            "project_busy_alert_observed": true,
            "match": match
        ]
        let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
        try? data.write(to: URL(fileURLWithPath: failurePath), options: [.atomic])
        print(String(data: data, encoding: .utf8)!)
        fputs("Loom exposed a project_busy alert during native smoke\n", stderr)
        exit(42)
    }
    Thread.sleep(forTimeInterval: 0.05)
}

let evidence: [String: Any] = [
    "pid": pid,
    "started_at_ms": startedAtMs,
    "finished_at_ms": Int64(Date().timeIntervalSince1970 * 1_000),
    "polls": polls,
    "project_busy_alert_observed": false
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
SWIFT
  LOOM_PROJECT_BUSY_MONITOR_PID=$!

  monitor_attempt=0
  while [ "$monitor_attempt" -lt 200 ]; do
    if [ -f "$LOOM_PROJECT_BUSY_MONITOR_READY" ]; then
      return 0
    fi
    if [ -f "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" ] ||
      ! kill -0 "$LOOM_PROJECT_BUSY_MONITOR_PID" 2>/dev/null; then
      cat "$LOOM_PROJECT_BUSY_MONITOR_ERROR" >&2 2>/dev/null || true
      return 1
    fi
    monitor_attempt=$((monitor_attempt + 1))
    sleep 0.05
  done
  echo "Loom project_busy alert monitor did not become ready" >&2
  return 1
}

require_loom_project_busy_monitor() {
  if [ -f "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" ]; then
    cat "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" >&2
    cat "$LOOM_PROJECT_BUSY_MONITOR_ERROR" >&2 2>/dev/null || true
    return 1
  fi
  if [ -z "$LOOM_PROJECT_BUSY_MONITOR_PID" ] ||
    ! kill -0 "$LOOM_PROJECT_BUSY_MONITOR_PID" 2>/dev/null; then
    cat "$LOOM_PROJECT_BUSY_MONITOR_ERROR" >&2 2>/dev/null || true
    echo "Loom project_busy alert monitor ended before the observed interaction interval" >&2
    return 1
  fi
}

stop_loom_project_busy_monitor() {
  touch "$LOOM_PROJECT_BUSY_MONITOR_STOP"
  if wait "$LOOM_PROJECT_BUSY_MONITOR_PID"; then
    monitor_status=0
  else
    monitor_status=$?
  fi
  LOOM_PROJECT_BUSY_MONITOR_PID=
  if [ "$monitor_status" -ne 0 ] || [ -f "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" ]; then
    cat "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" >&2 2>/dev/null || true
    cat "$LOOM_PROJECT_BUSY_MONITOR_ERROR" >&2 2>/dev/null || true
    return 1
  fi
  if [ ! -s "$LOOM_PROJECT_BUSY_MONITOR_OUTPUT" ]; then
    echo "Loom project_busy alert monitor produced no success evidence" >&2
    return 1
  fi
}

start_loom_generation_guard() {
  database=$1
  baseline=$2
  monitor_name=$3
  LOOM_GENERATION_GUARD_STOP="$SMOKE_ROOT/$monitor_name.stop"
  LOOM_GENERATION_GUARD_READY="$SMOKE_ROOT/$monitor_name.ready"
  LOOM_GENERATION_GUARD_FAILURE="$SMOKE_ROOT/$monitor_name.failure.json"
  LOOM_GENERATION_GUARD_OUTPUT="$SMOKE_ROOT/$monitor_name.evidence.json"
  LOOM_GENERATION_GUARD_ERROR="$SMOKE_ROOT/$monitor_name.stderr.log"
  rm -f \
    "$LOOM_GENERATION_GUARD_STOP" \
    "$LOOM_GENERATION_GUARD_READY" \
    "$LOOM_GENERATION_GUARD_FAILURE" \
    "$LOOM_GENERATION_GUARD_OUTPUT" \
    "$LOOM_GENERATION_GUARD_ERROR"
  (
    expected=$((baseline + 4))
    polls=0
    unreadable_polls=0
    maximum=$baseline
    touch "$LOOM_GENERATION_GUARD_READY"
    while [ ! -f "$LOOM_GENERATION_GUARD_STOP" ]; do
      count=$(sqlite3 "$database" 'SELECT count(*) FROM generation_runs;' 2>/dev/null || true)
      case "$count" in
        ''|*[!0-9]*)
          unreadable_polls=$((unreadable_polls + 1))
          ;;
        *)
          if [ "$count" -gt "$maximum" ]; then maximum=$count; fi
          if [ "$count" -gt "$expected" ]; then
            printf '{"baseline":%s,"expected_maximum":%s,"observed":%s,"polls":%s,"fifth_run_observed":true}\n' \
              "$baseline" "$expected" "$count" "$polls" >"$LOOM_GENERATION_GUARD_FAILURE"
            echo "Loom admitted a fifth generation run while the first ghost/cache family was in use" >&2
            exit 1
          fi
          ;;
      esac
      polls=$((polls + 1))
      sleep 0.05
    done
    final=$(sqlite3 "$database" 'SELECT count(*) FROM generation_runs;' 2>/dev/null || true)
    case "$final" in
      ''|*[!0-9]*)
        echo "could not read the final Loom generation-run count" >&2
        exit 1
        ;;
    esac
    if [ "$final" -ne "$expected" ]; then
      printf '{"baseline":%s,"expected":%s,"observed":%s,"polls":%s,"fifth_run_observed":false}\n' \
        "$baseline" "$expected" "$final" "$polls" >"$LOOM_GENERATION_GUARD_FAILURE"
      echo "Loom generation family did not remain exactly four runs through ghost/cache use" >&2
      exit 1
    fi
    printf '{"baseline":%s,"expected":%s,"final":%s,"maximum_observed":%s,"polls":%s,"unreadable_polls":%s,"fifth_run_observed":false}\n' \
      "$baseline" "$expected" "$final" "$maximum" "$polls" "$unreadable_polls"
  ) >"$LOOM_GENERATION_GUARD_OUTPUT" 2>"$LOOM_GENERATION_GUARD_ERROR" &
  LOOM_GENERATION_GUARD_PID=$!

  guard_attempt=0
  while [ "$guard_attempt" -lt 200 ]; do
    if [ -f "$LOOM_GENERATION_GUARD_READY" ]; then
      return 0
    fi
    if [ -f "$LOOM_GENERATION_GUARD_FAILURE" ] ||
      ! kill -0 "$LOOM_GENERATION_GUARD_PID" 2>/dev/null; then
      cat "$LOOM_GENERATION_GUARD_ERROR" >&2 2>/dev/null || true
      return 1
    fi
    guard_attempt=$((guard_attempt + 1))
    sleep 0.05
  done
  echo "Loom generation-family guard did not become ready" >&2
  return 1
}

require_loom_generation_guard() {
  if [ -f "$LOOM_GENERATION_GUARD_FAILURE" ]; then
    cat "$LOOM_GENERATION_GUARD_FAILURE" >&2
    cat "$LOOM_GENERATION_GUARD_ERROR" >&2 2>/dev/null || true
    return 1
  fi
  if [ -z "$LOOM_GENERATION_GUARD_PID" ] ||
    ! kill -0 "$LOOM_GENERATION_GUARD_PID" 2>/dev/null; then
    cat "$LOOM_GENERATION_GUARD_ERROR" >&2 2>/dev/null || true
    echo "Loom generation-family guard ended before cached completion use finished" >&2
    return 1
  fi
}

stop_loom_generation_guard() {
  touch "$LOOM_GENERATION_GUARD_STOP"
  if wait "$LOOM_GENERATION_GUARD_PID"; then
    guard_status=0
  else
    guard_status=$?
  fi
  LOOM_GENERATION_GUARD_PID=
  if [ "$guard_status" -ne 0 ] || [ -f "$LOOM_GENERATION_GUARD_FAILURE" ]; then
    cat "$LOOM_GENERATION_GUARD_FAILURE" >&2 2>/dev/null || true
    cat "$LOOM_GENERATION_GUARD_ERROR" >&2 2>/dev/null || true
    return 1
  fi
  if [ ! -s "$LOOM_GENERATION_GUARD_OUTPUT" ]; then
    echo "Loom generation-family guard produced no success evidence" >&2
    return 1
  fi
}

capture_loom_completion_diagnostics() {
  target_pid=$1
  database=$2
  manuscript=$3
  baseline=$4
  destination=$5
  diagnostic_control=$(loom_completion_control_state "$target_pid" 2>&1 || true)
  diagnostic_count=$(sqlite3 "$database" 'SELECT count(*) FROM generation_runs;' 2>/dev/null || true)
  diagnostic_runs=$(sqlite3 -json "$database" \
    'SELECT run_id, branch_id, source_revision_id, source_blob_id, target_start_byte, target_end_byte, created_at_ms FROM generation_runs ORDER BY created_at_ms DESC LIMIT 12;' \
    2>/dev/null || printf '[]')
  diagnostic_manuscript_sha=$(shasum -a 256 "$manuscript" 2>/dev/null | awk '{print $1}' || true)
  DELYSIS_DIAGNOSTIC_CONTROL="$diagnostic_control" \
  DELYSIS_DIAGNOSTIC_COUNT="$diagnostic_count" \
  DELYSIS_DIAGNOSTIC_RUNS="$diagnostic_runs" \
  DELYSIS_DIAGNOSTIC_BASELINE="$baseline" \
  DELYSIS_DIAGNOSTIC_MANUSCRIPT_SHA="$diagnostic_manuscript_sha" \
  DELYSIS_DIAGNOSTIC_PROJECT_BUSY_FAILURE="${LOOM_PROJECT_BUSY_MONITOR_FAILURE:-}" \
  DELYSIS_DIAGNOSTIC_GENERATION_FAILURE="${LOOM_GENERATION_GUARD_FAILURE:-}" \
  node <<'NODE' >"$destination"
const fs = require('fs');
const e = process.env;
const generationRunCount = /^\d+$/.test(e.DELYSIS_DIAGNOSTIC_COUNT || '')
  ? Number(e.DELYSIS_DIAGNOSTIC_COUNT)
  : null;
function parsed(value) {
  try { return JSON.parse(value); } catch { return value || null; }
}
function optionalFile(path) {
  if (!path || !fs.existsSync(path)) return null;
  return parsed(fs.readFileSync(path, 'utf8'));
}
process.stdout.write(`${JSON.stringify({
  captured_at: new Date().toISOString(),
  completion_control: parsed(e.DELYSIS_DIAGNOSTIC_CONTROL),
  generation_run_baseline: Number(e.DELYSIS_DIAGNOSTIC_BASELINE),
  generation_run_count: generationRunCount,
  latest_generation_runs: parsed(e.DELYSIS_DIAGNOSTIC_RUNS),
  manuscript_sha256: e.DELYSIS_DIAGNOSTIC_MANUSCRIPT_SHA || null,
  project_busy_monitor_failure: optionalFile(e.DELYSIS_DIAGNOSTIC_PROJECT_BUSY_FAILURE),
  generation_guard_failure: optionalFile(e.DELYSIS_DIAGNOSTIC_GENERATION_FAILURE),
}, null, 2)}\n`);
NODE
}

wait_for_loom_generation_family() {
  database=$1
  baseline=$2
  attempt=0
  family_evidence=
  while [ "$attempt" -lt 1800 ]; do
    if { [ -n "${LOOM_GENERATION_GUARD_FAILURE:-}" ] && [ -f "$LOOM_GENERATION_GUARD_FAILURE" ]; } ||
      { [ -n "${LOOM_PROJECT_BUSY_MONITOR_FAILURE:-}" ] && [ -f "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" ]; }; then
      echo "Loom failed an asynchronous generation or project_busy guard before family admission" >&2
      return 1
    fi
    count=$(sqlite3 "$database" 'SELECT count(*) FROM generation_runs;' 2>/dev/null || true)
    case "$count" in
      ''|*[!0-9]*) ;;
      *)
        delta=$((count - baseline))
        if [ "$delta" -gt 0 ]; then
          if [ "$delta" -ne 4 ]; then
            echo "Loom admitted $delta generation runs instead of one four-choice batch" >&2
            return 1
          fi
          if [ -z "$family_evidence" ]; then
            family_rows=$(sqlite3 -json "$database" \
              "SELECT run_id, branch_id, document_id, source_revision_id, source_blob_id, target_start_byte, target_end_byte, model_environment_artifact_id, prompt_recipe_artifact_id, context_recipe_artifact_id, authority_policy_artifact_id, created_at_ms FROM generation_runs ORDER BY created_at_ms, run_id LIMIT 4 OFFSET $baseline;" \
              2>/dev/null || true)
            [ -n "$family_rows" ] || family_rows='[]'
            if ! family_evidence=$(
              LOOM_FAMILY_ROWS="$family_rows" \
              LOOM_FAMILY_BASELINE="$baseline" \
              LOOM_FAMILY_ADMITTED="$count" \
              node <<'NODE'
const rows = JSON.parse(process.env.LOOM_FAMILY_ROWS || '[]');
const unique = (field) => new Set(rows.map((row) => JSON.stringify(row[field])));
const same = (field) => unique(field).size === 1;
const identityFields = [
  'document_id',
  'source_revision_id',
  'source_blob_id',
  'target_start_byte',
  'target_end_byte',
  'model_environment_artifact_id',
  'prompt_recipe_artifact_id',
  'context_recipe_artifact_id',
  'authority_policy_artifact_id',
  'created_at_ms',
];
if (
  rows.length !== 4 ||
  unique('run_id').size !== 4 ||
  unique('branch_id').size !== 4 ||
  !identityFields.every(same)
) {
  console.error('four admitted runs did not form one exact source/anchor/model family');
  process.exit(1);
}
process.stdout.write(JSON.stringify({
  baseline: Number(process.env.LOOM_FAMILY_BASELINE),
  admitted: Number(process.env.LOOM_FAMILY_ADMITTED),
  family_size: rows.length,
  run_ids: rows.map((row) => row.run_id),
  branch_ids: rows.map((row) => row.branch_id),
  document_id: rows[0].document_id,
  source_revision_id: rows[0].source_revision_id,
  source_blob_id: rows[0].source_blob_id,
  target: {
    start_byte: rows[0].target_start_byte,
    end_byte: rows[0].target_end_byte,
  },
  model_environment_artifact_id: rows[0].model_environment_artifact_id,
  prompt_recipe_artifact_id: rows[0].prompt_recipe_artifact_id,
  context_recipe_artifact_id: rows[0].context_recipe_artifact_id,
  authority_policy_artifact_id: rows[0].authority_policy_artifact_id,
  created_at_ms: rows[0].created_at_ms,
}));
NODE
            ); then
              echo "Loom's four runs were not one exact source/anchor/model family" >&2
              return 1
            fi
          fi

          terminal_failures=$(sqlite3 -json "$database" \
            "WITH family AS (SELECT run_id FROM generation_runs ORDER BY created_at_ms, run_id LIMIT 4 OFFSET $baseline) SELECT t.run_id, t.status, t.error, t.created_at_ms FROM generation_terminals t JOIN family f ON f.run_id = t.run_id WHERE t.status <> 'completed' ORDER BY t.created_at_ms, t.run_id;" \
            2>/dev/null || true)
          if [ -n "$terminal_failures" ]; then
            echo "Loom's admitted completion family reached a non-completed terminal: $terminal_failures" >&2
            return 1
          fi

          completion_rows=$(sqlite3 -json "$database" \
            "WITH family AS (SELECT run_id, created_at_ms FROM generation_runs ORDER BY created_at_ms, run_id LIMIT 4 OFFSET $baseline) SELECT f.run_id, t.status, t.event_id AS terminal_event_id, t.candidate_id AS terminal_candidate_id, t.created_at_ms AS terminal_created_at_ms, c.candidate_id, c.generated_span_artifact_id, c.token_trace_artifact_id AS candidate_token_trace_artifact_id, c.output_blob_id AS candidate_output_blob_id, e.operation_id, e.output_artifact_id, e.output_blob_id AS evidence_output_blob_id, e.token_trace_artifact_id AS evidence_token_trace_artifact_id, e.candidate_id AS evidence_candidate_id, e.created_at_ms AS evidence_created_at_ms FROM family f JOIN generation_terminals t ON t.run_id = f.run_id AND t.status = 'completed' JOIN generation_candidates c ON c.run_id = f.run_id JOIN generation_terminal_evidence e ON e.run_id = f.run_id ORDER BY f.created_at_ms, f.run_id;" \
            2>/dev/null || true)
          [ -n "$completion_rows" ] || completion_rows='[]'
          if completed_family_evidence=$(
            LOOM_FAMILY_ADMISSION="$family_evidence" \
            LOOM_FAMILY_COMPLETIONS="$completion_rows" \
            node <<'NODE'
const admission = JSON.parse(process.env.LOOM_FAMILY_ADMISSION || '{}');
const rows = JSON.parse(process.env.LOOM_FAMILY_COMPLETIONS || '[]');
const required = (row, field) => typeof row[field] === 'string' && row[field].length > 0;
const runIds = rows.map((row) => row.run_id);
const exact = rows.length === 4 &&
  new Set(runIds).size === 4 &&
  JSON.stringify([...runIds].sort()) === JSON.stringify([...admission.run_ids].sort()) &&
  rows.every((row) =>
    row.status === 'completed' &&
    required(row, 'terminal_event_id') &&
    required(row, 'terminal_candidate_id') &&
    required(row, 'candidate_id') &&
    required(row, 'evidence_candidate_id') &&
    row.terminal_candidate_id === row.candidate_id &&
    row.evidence_candidate_id === row.candidate_id &&
    required(row, 'generated_span_artifact_id') &&
    required(row, 'candidate_token_trace_artifact_id') &&
    required(row, 'evidence_token_trace_artifact_id') &&
    row.candidate_token_trace_artifact_id === row.evidence_token_trace_artifact_id &&
    required(row, 'candidate_output_blob_id') &&
    required(row, 'evidence_output_blob_id') &&
    row.candidate_output_blob_id === row.evidence_output_blob_id &&
    required(row, 'operation_id') &&
    required(row, 'output_artifact_id') &&
    row.generated_span_artifact_id === row.output_artifact_id
  );
if (!exact) process.exit(1);
process.stdout.write(JSON.stringify({
  ...admission,
  family_terminal_status: 'completed',
  completed_family_size: rows.length,
  terminals: rows,
}));
NODE
          ); then
            printf '%s\n' "$completed_family_evidence"
            return 0
          fi
        fi
        ;;
    esac
    attempt=$((attempt + 1))
    sleep 0.2
  done
  echo "Loom never durably completed one exact four-choice generation family" >&2
  return 1
}

wait_for_loom_manuscript_extension() {
  manuscript=$1
  prefix=$2
  attempt=0
  while [ "$attempt" -lt 1800 ]; do
    if [ -n "${LOOM_PROJECT_BUSY_MONITOR_FAILURE:-}" ] && [ -f "$LOOM_PROJECT_BUSY_MONITOR_FAILURE" ]; then
      echo "Loom exposed project_busy before Shuttle persisted its cached word" >&2
      return 1
    fi
    if node - "$manuscript" "$prefix" <<'NODE'
const fs = require('fs');
const [path, prefix] = process.argv.slice(2);
try {
  const observed = fs.readFileSync(path, 'utf8');
  process.exit(observed.startsWith(prefix) && observed.length > prefix.length && /\S/u.test(observed.slice(prefix.length)) ? 0 : 1);
} catch (error) {
  if (error?.code === 'ENOENT') process.exit(1);
  throw error;
}
NODE
    then
      node - "$manuscript" <<'NODE'
const fs = require('fs');
process.stdout.write(JSON.stringify(fs.readFileSync(process.argv[2], 'utf8')));
NODE
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 0.1
  done
  echo "Shuttle never persisted a completion word after a real four-way batch" >&2
  return 1
}

exercise_loom_completion_word_reversal() {
  target_pid=$1
  manuscript=$2
  prefix=$3
  generation_failure=${4:-}
  project_busy_failure=${5:-}
  xcrun swift - \
    "$target_pid" "$manuscript" "$prefix" "$generation_failure" "$project_busy_failure" <<'SWIFT'
import AppKit
import ApplicationServices
import CryptoKit
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let manuscript = CommandLine.arguments[2]
let prefix = CommandLine.arguments[3]
let asynchronousFailurePaths = [CommandLine.arguments[4], CommandLine.arguments[5]]
    .filter { !$0.isEmpty }
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func stringAttribute(_ element: AXUIElement, _ name: CFString) -> String {
    attribute(element, name) as? String ?? ""
}

let stringAttributes = [
    kAXValueAttribute,
    kAXTitleAttribute,
    kAXDescriptionAttribute,
    kAXHelpAttribute
].map { $0 as CFString }

func strings(_ element: AXUIElement) -> [String] {
    stringAttributes.compactMap { attribute(element, $0) as? String }
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

func editor() -> AXUIElement? {
    descendants().first { element in
        stringAttribute(element, kAXRoleAttribute as CFString) == kAXTextAreaRole as String
    }
}

func jsonObject(in text: String, schema: String) -> [String: Any]? {
    guard let schemaRange = text.range(of: "\"schema\":\"\(schema)\"") else { return nil }
    let prefix = text[..<schemaRange.lowerBound]
    guard let open = prefix.lastIndex(of: "{"),
          let close = text.lastIndex(of: "}"),
          open <= close,
          let data = String(text[open...close]).data(using: .utf8),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          object["schema"] as? String == schema else { return nil }
    return object
}

func completionWitness() -> [String: Any]? {
    for element in descendants() {
        for value in strings(element) {
            if let object = jsonObject(
                in: value,
                schema: "delysis.loom-completion-witness.v1"
            ) { return object }
        }
    }
    return nil
}

func supportsPress(_ element: AXUIElement) -> Bool {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success,
          let actions = names as? [String] else { return false }
    return actions.contains(kAXPressAction as String)
}

func button(named name: String) -> AXUIElement? {
    descendants().first { element in
        strings(element).contains(where: { $0.contains(name) }) &&
            supportsPress(element) &&
            (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false
    }
}

func pressButton(named name: String, timeout: TimeInterval = 10) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if let control = button(named: name),
           AXUIElementPerformAction(control, kAXPressAction as CFString) == .success {
            return true
        }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return false
}

func visual(_ witness: [String: Any]) -> [String: Any] {
    witness["visual"] as? [String: Any] ?? [:]
}

func bool(_ object: [String: Any], _ key: String) -> Bool {
    object[key] as? Bool ?? false
}

func integer(_ object: [String: Any], _ key: String) -> Int {
    (object[key] as? NSNumber)?.intValue ?? -1
}

func string(_ object: [String: Any], _ key: String) -> String {
    object[key] as? String ?? ""
}

func stringArray(_ object: [String: Any], _ key: String) -> [String] {
    object[key] as? [String] ?? []
}

func familyRunIds(_ witness: [String: Any]) -> [String] {
    (witness["candidates"] as? [[String: Any]] ?? []).map { string($0, "run_id") }
}

func lastAction(_ witness: [String: Any]) -> [String: Any] {
    witness["last_action"] as? [String: Any] ?? [:]
}

func sameFamily(
    _ witness: [String: Any],
    context: String,
    runIds: [String]
) -> Bool {
    string(witness, "context_key") == context &&
        familyRunIds(witness) == runIds &&
        integer(witness, "family_count") == 4
}

func actionNames(_ element: AXUIElement) -> [String] {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success else { return [] }
    return names as? [String] ?? []
}

func selectedState(_ element: AXUIElement) -> Bool {
    let value = attribute(element, kAXSelectedAttribute as CFString)
    if let selected = value as? Bool { return selected }
    return (value as? NSNumber)?.boolValue ?? false
}

let suggestionLabelPattern = try! NSRegularExpression(
    pattern: #"^Suggestion ([1-9][0-9]*) of ([1-9][0-9]*): (.+)$"#
)

func suggestionOrdinal(_ label: String) -> (index: Int, count: Int)? {
    let range = NSRange(label.startIndex..<label.endIndex, in: label)
    guard let match = suggestionLabelPattern.firstMatch(in: label, range: range),
          let indexRange = Range(match.range(at: 1), in: label),
          let countRange = Range(match.range(at: 2), in: label),
          let index = Int(label[indexRange]),
          let count = Int(label[countRange]) else { return nil }
    return (index, count)
}

func subtree(_ root: AXUIElement) -> [AXUIElement] {
    var queue = [root]
    var cursor = 0
    while cursor < queue.count && cursor < 256 {
        let element = queue[cursor]
        cursor += 1
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return queue
}

func accessibilityObservation(_ element: AXUIElement) -> [String: Any] {
    [
        "role": stringAttribute(element, kAXRoleAttribute as CFString),
        "subrole": stringAttribute(element, kAXSubroleAttribute as CFString),
        "strings": strings(element),
        "selected": selectedState(element),
        "actions": actionNames(element)
    ]
}

func fanAccessibility() -> (
    listbox: Bool,
    options: [[String: Any]],
    observations: [[String: Any]]
) {
    let listboxes = descendants().filter { element in
        stringAttribute(element, kAXRoleAttribute as CFString) == kAXListRole as String &&
            strings(element).contains("Completion suggestions")
    }
    var byIndex: [Int: [String: Any]] = [:]
    var observations: [[String: Any]] = []
    for listbox in listboxes {
        observations.append(accessibilityObservation(listbox))
        for element in subtree(listbox) {
            for label in strings(element) {
                guard let ordinal = suggestionOrdinal(label), ordinal.count == 4 else { continue }
                let option: [String: Any] = [
                    "index": ordinal.index,
                    "count": ordinal.count,
                    "label": label,
                    "ax_selected": selectedState(element)
                ]
                if byIndex[ordinal.index] == nil || selectedState(element) {
                    byIndex[ordinal.index] = option
                }
                observations.append(accessibilityObservation(element))
            }
        }
    }
    return (!listboxes.isEmpty, Array(byIndex.values), observations)
}

func exactAccessibleFan(_ witness: [String: Any]) -> [[String: Any]]? {
    let fan = fanAccessibility()
    let candidates = witness["candidates"] as? [[String: Any]] ?? []
    guard fan.listbox,
          fan.options.count == 4,
          candidates.count == 4 else {
        return nil
    }
    var enriched: [[String: Any]] = []
    for option in fan.options.sorted(by: { integer($0, "index") < integer($1, "index") }) {
        let index = integer(option, "index")
        guard index == enriched.count + 1 else { return nil }
        let candidate = candidates[index - 1]
        var joined = option
        joined["run_id"] = string(candidate, "run_id")
        joined["candidate_id"] = string(candidate, "candidate_id")
        joined["presentation_key"] = string(candidate, "presentation_key")
        enriched.append(joined)
    }
    guard enriched.filter({ bool($0, "ax_selected") }).count == 1,
          let selected = enriched.first(where: { bool($0, "ax_selected") }),
          string(selected, "run_id") == string(witness, "selected_run_id") else {
        return nil
    }
    return enriched
}

func waitForAccessibleFan(
    timeout: TimeInterval,
    _ predicate: ([String: Any]) -> Bool
) -> (witness: [String: Any], options: [[String: Any]])? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return nil }
        if let witness = completionWitness(), predicate(witness),
           let options = exactAccessibleFan(witness) {
            return (witness, options)
        }
        // WebKit publishes the application witness and the rebuilt ARIA
        // subtree on separate accessibility turns. Require both exact views
        // to converge instead of sampling the option rows once immediately
        // after the parent witness changes.
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return nil
}

@discardableResult
func postKey(_ key: CGKeyCode, down: Bool, flags: CGEventFlags) -> Bool {
    guard let event = CGEvent(keyboardEventSource: nil, virtualKey: key, keyDown: down) else {
        return false
    }
    event.flags = flags
    event.postToPid(pid)
    return true
}

func readManuscript() -> Data? {
    try? Data(contentsOf: URL(fileURLWithPath: manuscript), options: [.uncached])
}

func sha256(_ data: Data) -> String {
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}

func asynchronousGuardFailed() -> Bool {
    asynchronousFailurePaths.contains { FileManager.default.fileExists(atPath: $0) }
}

func waitForWitness(
    timeout: TimeInterval,
    _ predicate: ([String: Any]) -> Bool
) -> [String: Any]? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return nil }
        if let witness = completionWitness(), predicate(witness) { return witness }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return nil
}

func waitForChangedManuscript(from original: Data, timeout: TimeInterval) -> Data? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return nil }
        if let current = readManuscript(), current != original,
           let text = String(data: current, encoding: .utf8), text.hasPrefix(prefix) {
            let suffix = text.dropFirst(prefix.count)
            if suffix.rangeOfCharacter(from: .whitespacesAndNewlines.inverted) != nil {
                return current
            }
        }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return nil
}

func waitForExactManuscript(_ expected: Data, timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return false }
        if readManuscript() == expected { return true }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return false
}

NSRunningApplication(processIdentifier: pid)?.activate(options: [])
guard let writingSurface = editor(),
      AXUIElementSetAttributeValue(
        writingSurface,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
      ) == .success else {
    fputs("could not focus Loom's exact writing surface for completion reversal\n", stderr)
    exit(1)
}
guard let original = readManuscript() else {
    fputs("could not read Loom's isolated manuscript before completion reversal\n", stderr)
    exit(1)
}
Thread.sleep(forTimeInterval: 0.1)

guard let initial = waitForWitness(timeout: 30, { witness in
    let rendered = visual(witness)
    return bool(witness, "session_cached") &&
        integer(witness, "family_count") == 4 &&
        integer(witness, "accepted_chunk_count") == 0 &&
        bool(witness, "autocomplete_enabled") &&
        !bool(witness, "shuttle_enabled") &&
        !string(witness, "inline_visible_key").isEmpty &&
        bool(rendered, "available") &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("Loom did not expose one exact cached four-choice completion witness\n", stderr)
    exit(1)
}
let context = string(initial, "context_key")
let runIds = familyRunIds(initial)
let initialRunId = string(initial, "selected_run_id")
let initialActionSequence = integer(lastAction(initial), "sequence")
guard !context.isEmpty,
      Set(runIds).count == 4,
      !initialRunId.isEmpty else {
    fputs("Loom's initial completion witness lacked exact family identity\n", stderr)
    exit(1)
}

// Keep the physical Option state down across both arrows. The test observes
// persisted manuscript bytes after each event instead of trusting dispatch.
guard postKey(58, down: true, flags: [.maskAlternate]) else {
    fputs("could not construct Loom's Option modifier event\n", stderr)
    exit(1)
}
defer { postKey(58, down: false, flags: []) }

func releaseOptionAndFail(_ message: String) -> Never {
    let fan = fanAccessibility()
    let diagnostic: [String: Any] = [
        "completion_witness": completionWitness() ?? [:],
        "fan_listbox_observed": fan.listbox,
        "fan_options": fan.options,
        "fan_observations": fan.observations
    ]
    if JSONSerialization.isValidJSONObject(diagnostic),
       let data = try? JSONSerialization.data(withJSONObject: diagnostic, options: [.sortedKeys]),
       let json = String(data: data, encoding: .utf8) {
        fputs("completion fan diagnostics: \(json)\n", stderr)
    }
    postKey(58, down: false, flags: [])
    fputs("\(message)\n", stderr)
    exit(1)
}

guard let fanOpenedEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible") &&
        stringArray(rendered, "alternativeRunIds") == runIds
}) else {
    releaseOptionAndFail("physical Option-down did not expose one accessible four-choice fan")
}
let fanOpened = fanOpenedEvidence.witness
let fanOptions = fanOpenedEvidence.options

guard postKey(125, down: true, flags: [.maskAlternate]),
      postKey(125, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Down events")
}
guard let cycledDownEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") != initialRunId &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible")
}) else {
    releaseOptionAndFail("Option-Down did not select a different run while the four-choice fan stayed visible")
}
let cycledDown = cycledDownEvidence.witness
let cycledRunId = string(cycledDown, "selected_run_id")

guard postKey(126, down: true, flags: [.maskAlternate]),
      postKey(126, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Up events")
}
guard let cycledUpEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") == initialRunId &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible")
}) else {
    releaseOptionAndFail("Option-Up did not restore the original run while the four-choice fan stayed visible")
}
let cycledUp = cycledUpEvidence.witness

guard postKey(124, down: true, flags: [.maskAlternate]),
      postKey(124, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Right events")
}
guard let accepted = waitForChangedManuscript(from: original, timeout: 30) else {
    releaseOptionAndFail("Option-Right did not persist one cached completion word")
}
guard let wordAccepted = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    let action = lastAction(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") == initialRunId &&
        integer(witness, "accepted_chunk_count") == 1 &&
        bool(witness, "authority_frozen") &&
        bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible") &&
        string(action, "kind") == "option_word" &&
        string(action, "run_id") == initialRunId &&
        integer(action, "sequence") > initialActionSequence
}) else {
    releaseOptionAndFail("Option-Right did not retain physical Option and exact cached-session authority")
}

guard postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Left events")
}
guard waitForExactManuscript(original, timeout: 30) else {
    releaseOptionAndFail("Option-Left did not restore the exact pre-acceptance manuscript bytes")
}

guard let rolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") == initialRunId &&
        integer(witness, "accepted_chunk_count") == 0 &&
        bool(witness, "authority_frozen") &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible") &&
        stringArray(rendered, "alternativeRunIds") == runIds
}) else {
    releaseOptionAndFail("Option-Left did not restore the same cached four-choice fan while Option remained held")
}
let rolledBack = rolledBackEvidence.witness

postKey(58, down: false, flags: [])
guard let optionReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("physical Option-up did not close the completion fan\n", stderr)
    exit(1)
}

guard pressButton(named: "Turn Shuttle on") else {
    fputs("could not enable Shuttle on the cached completion session\n", stderr)
    exit(1)
}
guard let shuttleEnabled = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        bool(witness, "autocomplete_enabled") &&
        bool(witness, "shuttle_enabled") &&
        bool(witness, "inline_hidden_requested") &&
        string(witness, "inline_visible_key").isEmpty &&
        integer(witness, "accepted_chunk_count") == 0 &&
        bool(rendered, "inlineHidden") &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("Shuttle did not hide the inline presentation while retaining the exact cached family\n", stderr)
    exit(1)
}
guard let shuttleAcceptedBytes = waitForChangedManuscript(from: original, timeout: 30),
      let shuttleAccepted = waitForWitness(timeout: 10, { witness in
          let action = lastAction(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              bool(witness, "autocomplete_enabled") &&
              bool(witness, "shuttle_enabled") &&
              bool(witness, "inline_hidden_requested") &&
              string(witness, "inline_visible_key").isEmpty &&
              integer(witness, "accepted_chunk_count") == 1 &&
              integer(witness, "accepted_utf8_bytes") > 0 &&
              bool(witness, "authority_frozen") &&
              string(action, "kind") == "shuttle_word" &&
              string(action, "run_id") == initialRunId &&
              integer(action, "accepted_utf8_bytes") == integer(witness, "accepted_utf8_bytes") &&
              integer(action, "sequence") > integer(lastAction(wordAccepted), "sequence")
      }) else {
    fputs("Shuttle did not consume exactly one word from the same hidden cached family\n", stderr)
    exit(1)
}
let shuttleAction = lastAction(shuttleAccepted)
guard shuttleAcceptedBytes.count - original.count == integer(shuttleAction, "inserted_utf8_bytes") else {
    fputs("Shuttle's persisted byte delta did not equal its authorized cached word\n", stderr)
    exit(1)
}

guard pressButton(named: "Turn Shuttle off") else {
    fputs("could not stop Shuttle after its first cached word\n", stderr)
    exit(1)
}
guard let shuttleDisabled = waitForWitness(timeout: 10, { witness in
    return sameFamily(witness, context: context, runIds: runIds) &&
        bool(witness, "autocomplete_enabled") &&
        !bool(witness, "shuttle_enabled") &&
        !bool(witness, "inline_hidden_requested") &&
        integer(witness, "accepted_chunk_count") == 1 &&
        string(lastAction(witness), "kind") == "shuttle_word"
}) else {
    fputs("Shuttle-off did not preserve its exact one-word cached session\n", stderr)
    exit(1)
}

guard AXUIElementSetAttributeValue(
        writingSurface,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
      ) == .success,
      postKey(58, down: true, flags: [.maskAlternate]),
      postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not dispatch Shuttle's exact Option-Left rollback")
}
guard waitForExactManuscript(original, timeout: 30),
      let shuttleRolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              integer(witness, "accepted_chunk_count") == 0 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") &&
              bool(rendered, "fanVisible") &&
              stringArray(rendered, "alternativeRunIds") == runIds
      }) else {
    releaseOptionAndFail("Option-Left did not exactly reverse Shuttle's cached word and restore its fan")
}
let shuttleRolledBack = shuttleRolledBackEvidence.witness
postKey(58, down: false, flags: [])
guard let shuttleRollbackReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("Option-up did not settle after Shuttle rollback\n", stderr)
    exit(1)
}

// Prove the documented fan Return action against a deliberately non-default
// run, then use the exhausted session's rollback-only plan immediately.
guard postKey(58, down: true, flags: [.maskAlternate]),
      waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) != nil,
      postKey(125, down: true, flags: [.maskAlternate]),
      postKey(125, down: false, flags: [.maskAlternate]),
      let returnSelectedEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") != initialRunId &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("could not select a non-default cached run for fan Return")
}
let returnSelected = returnSelectedEvidence.witness
let returnRunId = string(returnSelected, "selected_run_id")
let returnPreviousSequence = integer(lastAction(returnSelected), "sequence")
guard postKey(36, down: true, flags: [.maskAlternate]),
      postKey(36, down: false, flags: [.maskAlternate]),
      let returnAcceptedBytes = waitForChangedManuscript(from: original, timeout: 30),
      let returnAccepted = waitForWitness(timeout: 10, { witness in
          let rendered = visual(witness)
          let action = lastAction(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == returnRunId &&
              integer(witness, "accepted_chunk_count") == 1 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") &&
              !bool(rendered, "fanVisible") &&
              string(action, "kind") == "fan_return" &&
              string(action, "run_id") == returnRunId &&
              integer(action, "sequence") > returnPreviousSequence
      }) else {
    releaseOptionAndFail("fan Return did not persist the selected cached remainder")
}
let returnAction = lastAction(returnAccepted)
guard returnAcceptedBytes.count - original.count == integer(returnAction, "inserted_utf8_bytes"),
      integer(returnAction, "accepted_utf8_bytes") == integer(returnAction, "inserted_utf8_bytes"),
      (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true,
      postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]),
      waitForExactManuscript(original, timeout: 30),
      let returnRolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == returnRunId &&
              integer(witness, "accepted_chunk_count") == 0 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("fan Return was not exact, focused, or immediately reversible")
}
let returnRolledBack = returnRolledBackEvidence.witness
postKey(58, down: false, flags: [])
guard let returnReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") && !bool(rendered, "fanVisible")
}) else {
    fputs("Option-up did not settle after fan Return rollback\n", stderr)
    exit(1)
}

// Repeat with fan Tab. A literal-tab fallback cannot satisfy the action kind,
// selected-run identity, or exact authorized byte delta below.
guard postKey(58, down: true, flags: [.maskAlternate]),
      waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) != nil,
      postKey(125, down: true, flags: [.maskAlternate]),
      postKey(125, down: false, flags: [.maskAlternate]),
      let tabSelectedEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") != returnRunId &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("could not select another cached run for fan Tab")
}
let tabSelected = tabSelectedEvidence.witness
let tabRunId = string(tabSelected, "selected_run_id")
let tabPreviousSequence = integer(lastAction(tabSelected), "sequence")
guard postKey(48, down: true, flags: [.maskAlternate]),
      postKey(48, down: false, flags: [.maskAlternate]),
      let tabAcceptedBytes = waitForChangedManuscript(from: original, timeout: 30),
      let tabAccepted = waitForWitness(timeout: 10, { witness in
          let rendered = visual(witness)
          let action = lastAction(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == tabRunId &&
              integer(witness, "accepted_chunk_count") == 1 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") &&
              !bool(rendered, "fanVisible") &&
              string(action, "kind") == "fan_tab" &&
              string(action, "run_id") == tabRunId &&
              integer(action, "sequence") > tabPreviousSequence
      }) else {
    releaseOptionAndFail("fan Tab did not persist the selected cached remainder")
}
let tabAction = lastAction(tabAccepted)
guard tabAcceptedBytes.count - original.count == integer(tabAction, "inserted_utf8_bytes"),
      integer(tabAction, "accepted_utf8_bytes") == integer(tabAction, "inserted_utf8_bytes"),
      (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true,
      postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]),
      waitForExactManuscript(original, timeout: 30),
      let tabRolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == tabRunId &&
              integer(witness, "accepted_chunk_count") == 0 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("fan Tab was not exact, focused, or immediately reversible")
}
let tabRolledBack = tabRolledBackEvidence.witness
postKey(58, down: false, flags: [])
guard let tabReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") && !bool(rendered, "fanVisible")
}) else {
    fputs("Option-up did not settle after fan Tab rollback\n", stderr)
    exit(1)
}

guard pressButton(named: "Turn autocomplete off") else {
    fputs("could not turn the shared completion engine off after cached checks\n", stderr)
    exit(1)
}
guard let engineDisabled = waitForWitness(timeout: 10, { witness in
    return !bool(witness, "autocomplete_enabled") &&
        !bool(witness, "shuttle_enabled") &&
        !bool(witness, "session_cached") &&
        integer(witness, "family_count") == 0 &&
        string(lastAction(witness), "kind") == "fan_tab"
}) else {
    fputs("shared engine on-to-off did not clear the cached completion session\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "dispatch": "Option held across native Right and Left arrow events",
    "original_bytes": original.count,
    "accepted_bytes": accepted.count,
    "original_sha256": sha256(original),
    "accepted_sha256": sha256(accepted),
    "rollback_sha256": sha256(readManuscript()!),
    "accepted_then_exactly_reversed": true,
    "context_key": context,
    "family_run_ids": runIds,
    "initial_selected_run_id": initialRunId,
    "cycled_down_run_id": cycledRunId,
    "accessible_fan_options": fanOptions,
    "initial_witness": initial,
    "fan_opened_witness": fanOpened,
    "cycled_down_witness": cycledDown,
    "cycled_up_witness": cycledUp,
    "word_accepted_witness": wordAccepted,
    "rolled_back_witness": rolledBack,
    "option_released_witness": optionReleased,
    "shuttle_enabled_witness": shuttleEnabled,
    "shuttle_accepted_witness": shuttleAccepted,
    "shuttle_accepted_sha256": sha256(shuttleAcceptedBytes),
    "shuttle_disabled_witness": shuttleDisabled,
    "shuttle_rolled_back_witness": shuttleRolledBack,
    "shuttle_rollback_released_witness": shuttleRollbackReleased,
    "fan_return_selected_witness": returnSelected,
    "fan_return_accepted_witness": returnAccepted,
    "fan_return_accepted_sha256": sha256(returnAcceptedBytes),
    "fan_return_rolled_back_witness": returnRolledBack,
    "fan_return_released_witness": returnReleased,
    "fan_tab_selected_witness": tabSelected,
    "fan_tab_accepted_witness": tabAccepted,
    "fan_tab_accepted_sha256": sha256(tabAcceptedBytes),
    "fan_tab_rolled_back_witness": tabRolledBack,
    "fan_tab_released_witness": tabReleased,
    "engine_disabled_witness": engineDisabled
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
try {
  const observed = fs.readFileSync(path, 'utf8');
  process.exit(observed === expected ? 0 : 1);
} catch (error) {
  if (error?.code === 'ENOENT') process.exit(1);
  throw error;
}
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
try {
  const observed = fs.readFileSync(path, 'utf8');
  process.exit(observed === expected ? 0 : 1);
} catch (error) {
  if (error?.code === 'ENOENT') process.exit(1);
  throw error;
}
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
  running_exact_pids=$(exact_bundle_pid)
  if [ -n "$running_exact_pids" ]; then
    echo "refusing to run macOS UI smoke while the exact application bundle is already running (pid(s): $(printf '%s' "$running_exact_pids" | tr '\n' ' '))" >&2
    echo "quit the app and run this smoke in a dedicated session so automation cannot steal or mutate an active editor" >&2
    return 1
  fi
  echo "+ launch $run_number: $EXECUTABLE"
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
    ACTIVE_PID=$(exact_bundle_pid | head -n 1)
    [ -n "$ACTIVE_PID" ] && break
    if ! kill -0 "$ACTIVE_LAUNCHER_PID" 2>/dev/null; then break; fi
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if [ -z "$ACTIVE_PID" ]; then
    echo "LaunchServices did not expose a new exact-bundle process" >&2
    return 1
  fi
  bound_command=$(ps -p "$ACTIVE_PID" -o command= | sed 's/^ *//')
  require_equal "bound process executable path" "$EXECUTABLE" "$bound_command"
  bound_executable_file_id=$(stat -Lf '%d:%i' "$EXECUTABLE")
  bound_executable_sha256=$(shasum -a 256 "$EXECUTABLE" | awk '{print $1}')
  require_equal "bound executable device/inode" "$EXECUTABLE_FILE_ID" "$bound_executable_file_id"
  require_equal "bound executable SHA-256" "$EXECUTABLE_SHA256" "$bound_executable_sha256"
  echo "+ bound pid: $ACTIVE_PID"
  case "$run_number" in
    1)
      RUN_1_PID=$ACTIVE_PID
      RUN_1_EXECUTABLE_FILE_ID=$bound_executable_file_id
      RUN_1_EXECUTABLE_SHA256=$bound_executable_sha256
      ;;
    2)
      RUN_2_PID=$ACTIVE_PID
      RUN_2_EXECUTABLE_FILE_ID=$bound_executable_file_id
      RUN_2_EXECUTABLE_SHA256=$bound_executable_sha256
      ;;
  esac

  if ! wait_for_window "$ACTIVE_PID"; then
    echo "packaged app did not expose a window" >&2
    echo "application logs: $stdout_log and $stderr_log" >&2
    return 1
  fi
  if ! wait_for_readiness "$run_number" "$ACTIVE_PID" "$stderr_log"; then
    echo "application logs: $stdout_log and $stderr_log" >&2
    return 1
  fi
  if [ "$COMPONENT" = mom ]; then
    if ! mom_ui_identity=$(require_mom_ui_identity "$ACTIVE_PID"); then
      echo "the exact Mom process failed UI identity verification" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    case "$run_number" in
      1) RUN_1_MOM_UI_IDENTITY=$mom_ui_identity ;;
      2) RUN_2_MOM_UI_IDENTITY=$mom_ui_identity ;;
    esac
  fi
  if [ "$COMPONENT" = loom ] && [ "$run_number" -eq 1 ]; then
    loom_manuscript="$PRODUCT_STATE/writing/manuscript/Untitled.md"
    loom_database="$PRODUCT_STATE/writing/.loom/loom.sqlite3"
    if ! start_loom_project_busy_monitor "$ACTIVE_PID" "launch-1-project-busy-monitor"; then
      echo "could not start the exact-PID project_busy alert monitor" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    if [ -n "$LOOM_SMOKE_REAL_COMPLETIONS" ]; then
      if ! RUN_1_AUTOCOMPLETE_OFF_EVIDENCE=$(set_loom_completion_toggle \
        "$ACTIVE_PID" "Turn autocomplete off" "Turn autocomplete on"); then
        echo "could not establish autocomplete off before real-completion typing" >&2
        return 1
      fi
      loom_generation_count_before_batch=$(sqlite3 \
        "$loom_database" \
        'SELECT count(*) FROM generation_runs;')
    else
      if ! RUN_1_COMPLETION_CONTROLS_EVIDENCE=$(exercise_loom_completion_controls "$ACTIVE_PID"); then
        echo "autocomplete and Shuttle did not behave as independent native controls" >&2
        echo "application logs: $stdout_log and $stderr_log" >&2
        return 1
      fi
    fi
    RUN_1_EDITOR_CORE_SENTINEL='Loom native smoke: editor persistence.'
    RUN_1_EDITOR_INPUT_SENTINEL="$RUN_1_EDITOR_CORE_SENTINEL "
    RUN_1_EDITOR_SENTINEL=$RUN_1_EDITOR_INPUT_SENTINEL
    if ! RUN_1_EDITOR_EVIDENCE=$(type_into_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_INPUT_SENTINEL"); then
      echo "could not drive the exact app's accessible manuscript editor" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    if ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL"; then
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    if ! RUN_1_WYSIWYG_EVIDENCE=$(require_loom_editor_state \
      "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "caret-end"); then
      echo "terminal-space input did not reconcile to one exact live/persisted WYSIWYG value" >&2
      echo "application logs: $stdout_log and $stderr_log" >&2
      return 1
    fi
    RUN_1_MANUSCRIPT_SHA256_AFTER_EDITOR_INPUT=$(shasum -a 256 "$loom_manuscript" | awk '{print $1}')
    if ! require_loom_project_busy_monitor; then
      echo "ordinary editor typing exposed a project_busy alert" >&2
      return 1
    fi
    if [ -n "$LOOM_SMOKE_REAL_COMPLETIONS" ]; then
      loom_generation_count_after_off_typing=$(sqlite3 \
        "$loom_database" \
        'SELECT count(*) FROM generation_runs;')
      require_equal "generation-run count while autocomplete was off during typing" \
        "$loom_generation_count_before_batch" "$loom_generation_count_after_off_typing"
      if ! start_loom_generation_guard \
        "$loom_database" \
        "$loom_generation_count_before_batch" \
        "launch-1-generation-family-guard"; then
        echo "could not start the one-family generation guard" >&2
        return 1
      fi
      if ! RUN_1_AUTOCOMPLETE_ENABLE_EVIDENCE=$(set_loom_completion_toggle \
        "$ACTIVE_PID" "Turn autocomplete on" "Turn autocomplete off" "require-press"); then
        echo "could not enable autocomplete exactly once for the real-model presentation check" >&2
        return 1
      fi
      if ! RUN_1_REAL_GENERATION_EVIDENCE=$(wait_for_loom_generation_family \
        "$loom_database" \
        "$loom_generation_count_before_batch"); then
        RUN_1_COMPLETION_DIAGNOSTICS="$SMOKE_ROOT/launch-1-completion-diagnostics.json"
        capture_loom_completion_diagnostics \
          "$ACTIVE_PID" "$loom_database" "$loom_manuscript" \
          "$loom_generation_count_before_batch" "$RUN_1_COMPLETION_DIAGNOSTICS"
        echo "completion control state: $(loom_completion_control_state "$ACTIVE_PID" 2>&1 || true)" >&2
        echo "completion diagnostics: $RUN_1_COMPLETION_DIAGNOSTICS" >&2
        cat "$RUN_1_COMPLETION_DIAGNOSTICS" >&2
        echo "application logs: $stdout_log and $stderr_log" >&2
        return 1
      fi
      if ! require_loom_generation_guard || ! require_loom_project_busy_monitor; then
        echo "the first real completion family violated its generation/alert guard" >&2
        return 1
      fi
      if ! RUN_1_REAL_GHOST_EVIDENCE=$(wait_for_loom_accessibility_text \
        "$ACTIVE_PID" "Suggestion available." \
        "$LOOM_GENERATION_GUARD_FAILURE" "$LOOM_PROJECT_BUSY_MONITOR_FAILURE"); then
        RUN_1_COMPLETION_DIAGNOSTICS="$SMOKE_ROOT/launch-1-ghost-timeout-diagnostics.json"
        capture_loom_completion_diagnostics \
          "$ACTIVE_PID" "$loom_database" "$loom_manuscript" \
          "$loom_generation_count_before_batch" "$RUN_1_COMPLETION_DIAGNOSTICS"
        echo "a real four-way batch never produced an observed visible ghost presentation" >&2
        echo "completion control state: $(loom_completion_control_state "$ACTIVE_PID" 2>&1 || true)" >&2
        echo "completion diagnostics: $RUN_1_COMPLETION_DIAGNOSTICS" >&2
        cat "$RUN_1_COMPLETION_DIAGNOSTICS" >&2
        echo "application logs: $stdout_log and $stderr_log" >&2
        return 1
      fi
      if ! require_loom_generation_guard || ! require_loom_project_busy_monitor; then
        echo "visible ghost presentation admitted an extra run or exposed project_busy" >&2
        return 1
      fi
      loom_generation_count_before_reversal=$(sqlite3 \
        "$loom_database" \
        'SELECT count(*) FROM generation_runs;')
      loom_expected_generation_count=$((loom_generation_count_before_batch + 4))
      require_equal "generation-run count before Option reversal" \
        "$loom_expected_generation_count" "$loom_generation_count_before_reversal"
      if ! RUN_1_REAL_WORD_REVERSAL_EVIDENCE=$(exercise_loom_completion_word_reversal \
        "$ACTIVE_PID" "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" \
        "$LOOM_GENERATION_GUARD_FAILURE" "$LOOM_PROJECT_BUSY_MONITOR_FAILURE"); then
        RUN_1_COMPLETION_DIAGNOSTICS="$SMOKE_ROOT/launch-1-interaction-failure-diagnostics.json"
        capture_loom_completion_diagnostics \
          "$ACTIVE_PID" "$loom_database" "$loom_manuscript" \
          "$loom_generation_count_before_batch" "$RUN_1_COMPLETION_DIAGNOSTICS"
        echo "the exact four-choice cache did not survive fan, Shuttle, Return, Tab, and rollback checks" >&2
        echo "completion diagnostics: $RUN_1_COMPLETION_DIAGNOSTICS" >&2
        cat "$RUN_1_COMPLETION_DIAGNOSTICS" >&2
        echo "application logs: $stdout_log and $stderr_log" >&2
        return 1
      fi
      if ! require_loom_generation_guard || ! require_loom_project_busy_monitor; then
        echo "cached completion interactions admitted a fifth run or exposed project_busy" >&2
        return 1
      fi
      loom_generation_count_after_reversal=$(sqlite3 \
        "$loom_database" \
        'SELECT count(*) FROM generation_runs;')
      require_equal "generation-run count across all cached completion interactions" \
        "$loom_generation_count_before_reversal" "$loom_generation_count_after_reversal"
      if ! DELYSIS_DATABASE_FAMILY="$RUN_1_REAL_GENERATION_EVIDENCE" \
        DELYSIS_ACCESSIBILITY_FAMILY="$RUN_1_REAL_WORD_REVERSAL_EVIDENCE" \
        node <<'NODE'
const db = JSON.parse(process.env.DELYSIS_DATABASE_FAMILY);
const ax = JSON.parse(process.env.DELYSIS_ACCESSIBILITY_FAMILY);
const normalizedRunIds = (runIds) => [...new Set(runIds)].sort();
if (
  JSON.stringify(normalizedRunIds(db.run_ids)) !==
    JSON.stringify(normalizedRunIds(ax.family_run_ids)) ||
  normalizedRunIds(db.run_ids).length !== 4 ||
  normalizedRunIds(ax.family_run_ids).length !== 4 ||
  db.family_size !== ax.family_run_ids.length
) {
  console.error('database family run IDs did not equal the exact AX cached family');
  process.exit(1);
}
NODE
      then
        echo "the native fan witness was not bound to the admitted database family" >&2
        return 1
      fi
      if ! stop_loom_generation_guard; then
        echo "the four-run family did not remain singular through every cached interaction" >&2
        return 1
      fi
      RUN_1_REAL_GENERATION_GUARD_EVIDENCE=$(cat "$LOOM_GENERATION_GUARD_OUTPUT")
    fi
    RUN_1_TITLE_SENTINEL="# $RUN_1_EDITOR_SENTINEL"
    if ! RUN_1_FORMAT_TITLE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Title") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_TITLE_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "caret-end" >/dev/null; then
      echo "Format text -> Title did not preserve exact manuscript/AX/focus state" >&2
      return 1
    fi
    if ! RUN_1_FORMAT_BODY_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Body") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "caret-end" >/dev/null; then
      echo "Format text -> Body did not exactly reverse the paragraph style" >&2
      return 1
    fi
    RUN_1_HEADING_SENTINEL="## $RUN_1_EDITOR_SENTINEL"
    if ! RUN_1_FORMAT_HEADING_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Heading") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_HEADING_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "caret-end" >/dev/null; then
      echo "Format text -> Heading did not preserve exact manuscript/AX/focus state" >&2
      return 1
    fi
    if ! RUN_1_FORMAT_HEADING_BODY_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Body") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "caret-end" >/dev/null; then
      echo "Format text -> Body did not exactly reverse Heading" >&2
      return 1
    fi
    RUN_1_SUBHEADING_SENTINEL="### $RUN_1_EDITOR_SENTINEL"
    if ! RUN_1_FORMAT_SUBHEADING_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Subheading") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_SUBHEADING_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "caret-end" >/dev/null; then
      echo "Format text -> Subheading did not preserve exact manuscript/AX/focus state" >&2
      return 1
    fi
    if ! RUN_1_FORMAT_SUBHEADING_BODY_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Body") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "caret-end" >/dev/null; then
      echo "Format text -> Body did not exactly reverse Subheading" >&2
      return 1
    fi

    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_BOLD_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Bold") ||
      ! require_loom_manuscript_text "$loom_manuscript" "**$RUN_1_EDITOR_CORE_SENTINEL** " ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Bold did not preserve the exact selected WYSIWYG text" >&2
      return 1
    fi
    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_BOLD_REVERSE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Bold") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Bold did not reverse to the exact manuscript" >&2
      return 1
    fi

    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_ITALIC_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Italic") ||
      ! require_loom_manuscript_text "$loom_manuscript" "*$RUN_1_EDITOR_CORE_SENTINEL* " ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Italic did not preserve the exact selected WYSIWYG text" >&2
      return 1
    fi
    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_ITALIC_REVERSE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Italic") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Italic did not reverse to the exact manuscript" >&2
      return 1
    fi

    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_QUOTE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Block quote") ||
      ! require_loom_manuscript_text "$loom_manuscript" "> $RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Block quote did not preserve the exact selected WYSIWYG text" >&2
      return 1
    fi
    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_QUOTE_REVERSE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Block quote") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Block quote did not reverse to the exact manuscript" >&2
      return 1
    fi

    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_LIST_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Bulleted list") ||
      ! require_loom_manuscript_text "$loom_manuscript" "* $RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Bulleted list did not preserve the exact selected WYSIWYG text" >&2
      return 1
    fi
    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_LIST_REVERSE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Bulleted list") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Bulleted list did not reverse to the exact manuscript" >&2
      return 1
    fi

    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_NUMBERED_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Numbered list") ||
      ! require_loom_manuscript_text "$loom_manuscript" "1. $RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Numbered list did not preserve the exact selected WYSIWYG text" >&2
      return 1
    fi
    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_NUMBERED_REVERSE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Numbered list") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Numbered list did not reverse to the exact manuscript" >&2
      return 1
    fi

    RUN_1_LINK_DESTINATION='https://example.com'
    RUN_1_LINK_SENTINEL="[$RUN_1_EDITOR_CORE_SENTINEL]($RUN_1_LINK_DESTINATION) "
    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_LINK_EVIDENCE=$(exercise_loom_formatting_palette \
        "$ACTIVE_PID" "Link" "$RUN_1_LINK_DESTINATION") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_LINK_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Link did not preserve the exact selected WYSIWYG text" >&2
      return 1
    fi
    if ! select_all_in_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" >/dev/null ||
      ! RUN_1_FORMAT_REMOVE_EVIDENCE=$(exercise_loom_formatting_palette "$ACTIVE_PID" "Remove") ||
      ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_EDITOR_SENTINEL" ||
      ! require_loom_editor_state "$ACTIVE_PID" "$RUN_1_EDITOR_SENTINEL" "select-all" >/dev/null; then
      echo "Format text -> Remove did not restore the exact unlinked manuscript" >&2
      return 1
    fi
    RUN_1_FORMATTED_SENTINEL=$RUN_1_EDITOR_SENTINEL
    RUN_1_FORMATTING_EVIDENCE=$(
      DELYSIS_FORMAT_TITLE="$RUN_1_FORMAT_TITLE_EVIDENCE" \
      DELYSIS_FORMAT_BODY="$RUN_1_FORMAT_BODY_EVIDENCE" \
      DELYSIS_FORMAT_HEADING="$RUN_1_FORMAT_HEADING_EVIDENCE" \
      DELYSIS_FORMAT_HEADING_BODY="$RUN_1_FORMAT_HEADING_BODY_EVIDENCE" \
      DELYSIS_FORMAT_SUBHEADING="$RUN_1_FORMAT_SUBHEADING_EVIDENCE" \
      DELYSIS_FORMAT_SUBHEADING_BODY="$RUN_1_FORMAT_SUBHEADING_BODY_EVIDENCE" \
      DELYSIS_FORMAT_BOLD="$RUN_1_FORMAT_BOLD_EVIDENCE" \
      DELYSIS_FORMAT_BOLD_REVERSE="$RUN_1_FORMAT_BOLD_REVERSE_EVIDENCE" \
      DELYSIS_FORMAT_ITALIC="$RUN_1_FORMAT_ITALIC_EVIDENCE" \
      DELYSIS_FORMAT_ITALIC_REVERSE="$RUN_1_FORMAT_ITALIC_REVERSE_EVIDENCE" \
      DELYSIS_FORMAT_QUOTE="$RUN_1_FORMAT_QUOTE_EVIDENCE" \
      DELYSIS_FORMAT_QUOTE_REVERSE="$RUN_1_FORMAT_QUOTE_REVERSE_EVIDENCE" \
      DELYSIS_FORMAT_LIST="$RUN_1_FORMAT_LIST_EVIDENCE" \
      DELYSIS_FORMAT_LIST_REVERSE="$RUN_1_FORMAT_LIST_REVERSE_EVIDENCE" \
      DELYSIS_FORMAT_NUMBERED="$RUN_1_FORMAT_NUMBERED_EVIDENCE" \
      DELYSIS_FORMAT_NUMBERED_REVERSE="$RUN_1_FORMAT_NUMBERED_REVERSE_EVIDENCE" \
      DELYSIS_FORMAT_LINK="$RUN_1_FORMAT_LINK_EVIDENCE" \
      DELYSIS_FORMAT_REMOVE="$RUN_1_FORMAT_REMOVE_EVIDENCE" \
      DELYSIS_FORMAT_CANONICAL="$RUN_1_EDITOR_SENTINEL" \
      DELYSIS_FORMAT_CORE="$RUN_1_EDITOR_CORE_SENTINEL" \
      DELYSIS_FORMAT_LINK_DESTINATION="$RUN_1_LINK_DESTINATION" \
      node <<'NODE'
const e = process.env;
const stage = (name, evidence, markdown) => ({ name, ...JSON.parse(evidence), observed_persisted_markdown: markdown });
process.stdout.write(JSON.stringify({
  canonical_plain_text: e.DELYSIS_FORMAT_CANONICAL,
  final_persisted_markdown: e.DELYSIS_FORMAT_CANONICAL,
  stages: [
    stage('title', e.DELYSIS_FORMAT_TITLE, `# ${e.DELYSIS_FORMAT_CANONICAL}`),
    stage('body', e.DELYSIS_FORMAT_BODY, e.DELYSIS_FORMAT_CANONICAL),
    stage('heading', e.DELYSIS_FORMAT_HEADING, `## ${e.DELYSIS_FORMAT_CANONICAL}`),
    stage('heading_body', e.DELYSIS_FORMAT_HEADING_BODY, e.DELYSIS_FORMAT_CANONICAL),
    stage('subheading', e.DELYSIS_FORMAT_SUBHEADING, `### ${e.DELYSIS_FORMAT_CANONICAL}`),
    stage('subheading_body', e.DELYSIS_FORMAT_SUBHEADING_BODY, e.DELYSIS_FORMAT_CANONICAL),
    stage('bold', e.DELYSIS_FORMAT_BOLD, `**${e.DELYSIS_FORMAT_CORE}** `),
    stage('bold_reverse', e.DELYSIS_FORMAT_BOLD_REVERSE, e.DELYSIS_FORMAT_CANONICAL),
    stage('italic', e.DELYSIS_FORMAT_ITALIC, `*${e.DELYSIS_FORMAT_CORE}* `),
    stage('italic_reverse', e.DELYSIS_FORMAT_ITALIC_REVERSE, e.DELYSIS_FORMAT_CANONICAL),
    stage('block_quote', e.DELYSIS_FORMAT_QUOTE, `> ${e.DELYSIS_FORMAT_CANONICAL}`),
    stage('block_quote_reverse', e.DELYSIS_FORMAT_QUOTE_REVERSE, e.DELYSIS_FORMAT_CANONICAL),
    stage('bullet_list', e.DELYSIS_FORMAT_LIST, `* ${e.DELYSIS_FORMAT_CANONICAL}`),
    stage('bullet_list_reverse', e.DELYSIS_FORMAT_LIST_REVERSE, e.DELYSIS_FORMAT_CANONICAL),
    stage('numbered_list', e.DELYSIS_FORMAT_NUMBERED, `1. ${e.DELYSIS_FORMAT_CANONICAL}`),
    stage('numbered_list_reverse', e.DELYSIS_FORMAT_NUMBERED_REVERSE, e.DELYSIS_FORMAT_CANONICAL),
    stage('link', e.DELYSIS_FORMAT_LINK, `[${e.DELYSIS_FORMAT_CORE}](${e.DELYSIS_FORMAT_LINK_DESTINATION}) `),
    stage('remove_link', e.DELYSIS_FORMAT_REMOVE, e.DELYSIS_FORMAT_CANONICAL),
  ],
}));
NODE
    )
    if ! require_loom_project_busy_monitor; then
      echo "formatting overlap exposed a project_busy alert" >&2
      return 1
    fi
    RUN_1_MANUSCRIPT_SHA256_AFTER_FORMATTING=$(shasum -a 256 "$loom_manuscript" | awk '{print $1}')
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
    if ! require_loom_project_busy_monitor || ! stop_loom_project_busy_monitor; then
      echo "Loom exposed project_busy during the monitored editor/autosave/completion interval" >&2
      return 1
    fi
    RUN_1_PROJECT_BUSY_MONITOR_EVIDENCE=$(cat "$LOOM_PROJECT_BUSY_MONITOR_OUTPUT")
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
    if ! require_loom_manuscript_text "$loom_manuscript" "$RUN_1_FORMATTED_SENTINEL"; then
      echo "persisted editor input did not reopen on the second exact-bundle launch" >&2
      return 1
    fi
    RUN_2_MANUSCRIPT_SHA256=$(shasum -a 256 "$loom_manuscript" | awk '{print $1}')
    require_equal "reopened manuscript SHA-256" \
      "$RUN_1_MANUSCRIPT_SHA256_AFTER_FORMATTING" "$RUN_2_MANUSCRIPT_SHA256"
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
    loom)
      if grep -Eiq 'project_busy|another bounded project operation is still running' \
        "$stdout_log" "$stderr_log"; then
        echo "Loom launch logs contain project_busy during native smoke" >&2
        grep -Ein 'project_busy|another bounded project operation is still running' \
          "$stdout_log" "$stderr_log" >&2 || true
        return 1
      fi
      if [ "$run_number" -eq 1 ]; then
        project_busy_receipt_count=$(sqlite3 \
          "$PRODUCT_STATE/writing/.loom/loom.sqlite3" \
          "SELECT count(*) FROM command_receipts WHERE instr(lower(receipt_json), 'project_busy') > 0 OR instr(lower(receipt_json), 'another bounded project operation is still running') > 0;")
        require_equal "durable project_busy command-receipt count" "0" "$project_busy_receipt_count"
        RUN_1_PROJECT_BUSY_LOG_EVIDENCE=$(printf \
          '{"stdout_and_stderr_scanned_after_exit":true,"durable_command_receipts_scanned":true,"project_busy_receipt_count":%s,"project_busy_observed":false}' \
          "$project_busy_receipt_count")
      fi
      ;;
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
EXECUTABLE_FILE_ID_AFTER=$(stat -Lf '%d:%i' "$EXECUTABLE")
if [ "$EXECUTABLE_FILE_ID_AFTER" != "$EXECUTABLE_FILE_ID" ]; then
  echo "packaged executable inode changed while the smoke test was running" >&2
  exit 1
fi
STATE_INVENTORY="$SMOKE_ROOT/state-inventory.txt"
find "$PRODUCT_STATE" -mindepth 1 -print | LC_ALL=C sort > "$STATE_INVENTORY"
RECEIPT="$SMOKE_ROOT/smoke-receipt.json"

DELYSIS_SMOKE_COMPONENT="$COMPONENT" \
DELYSIS_SMOKE_BUNDLE="$BUNDLE" \
DELYSIS_SMOKE_BUNDLE_ID="$BUNDLE_ID" \
DELYSIS_SMOKE_SOURCE_SHA="$MOM_ACCEPTANCE_SOURCE_SHA" \
DELYSIS_SMOKE_EXECUTABLE_PATH="$EXECUTABLE" \
DELYSIS_SMOKE_EXECUTABLE_FILE_ID="$EXECUTABLE_FILE_ID" \
DELYSIS_SMOKE_EXECUTABLE_SHA="$EXECUTABLE_SHA256" \
DELYSIS_SMOKE_INPUT_ARCHIVE="$INPUT_ARCHIVE" \
DELYSIS_SMOKE_INPUT_ARCHIVE_SHA="$INPUT_ARCHIVE_SHA256" \
DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT="$INPUT_RELEASE_RECEIPT" \
DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT_SHA="$INPUT_RELEASE_RECEIPT_SHA256" \
DELYSIS_SMOKE_STATE_ROOT="$PRODUCT_STATE" \
DELYSIS_SMOKE_RUN_1_PID="$RUN_1_PID" \
DELYSIS_SMOKE_RUN_2_PID="$RUN_2_PID" \
DELYSIS_SMOKE_RUN_1_EXECUTABLE_FILE_ID="$RUN_1_EXECUTABLE_FILE_ID" \
DELYSIS_SMOKE_RUN_2_EXECUTABLE_FILE_ID="$RUN_2_EXECUTABLE_FILE_ID" \
DELYSIS_SMOKE_RUN_1_EXECUTABLE_SHA="$RUN_1_EXECUTABLE_SHA256" \
DELYSIS_SMOKE_RUN_2_EXECUTABLE_SHA="$RUN_2_EXECUTABLE_SHA256" \
DELYSIS_SMOKE_RUN_1_MOM_UI_IDENTITY="${RUN_1_MOM_UI_IDENTITY:-}" \
DELYSIS_SMOKE_RUN_2_MOM_UI_IDENTITY="${RUN_2_MOM_UI_IDENTITY:-}" \
DELYSIS_SMOKE_RUN_1_DRAG_EVIDENCE="${RUN_1_DRAG_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_BEFORE="${RUN_1_MANUSCRIPT_SHA256_BEFORE:-}" \
DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER="${RUN_1_MANUSCRIPT_SHA256_AFTER:-}" \
DELYSIS_SMOKE_RUN_1_COMPLETION_CONTROLS_EVIDENCE="${RUN_1_COMPLETION_CONTROLS_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_EDITOR_EVIDENCE="${RUN_1_EDITOR_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_EDITOR_INPUT_SENTINEL="${RUN_1_EDITOR_INPUT_SENTINEL:-}" \
DELYSIS_SMOKE_RUN_1_EDITOR_SENTINEL="${RUN_1_EDITOR_SENTINEL:-}" \
DELYSIS_SMOKE_RUN_1_WYSIWYG_EVIDENCE="${RUN_1_WYSIWYG_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_FORMATTED_SENTINEL="${RUN_1_FORMATTED_SENTINEL:-}" \
DELYSIS_SMOKE_RUN_1_FORMATTING_EVIDENCE="${RUN_1_FORMATTING_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_AUTOCOMPLETE_OFF_EVIDENCE="${RUN_1_AUTOCOMPLETE_OFF_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_AUTOCOMPLETE_ENABLE_EVIDENCE="${RUN_1_AUTOCOMPLETE_ENABLE_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_REAL_GHOST_EVIDENCE="${RUN_1_REAL_GHOST_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_REAL_GENERATION_EVIDENCE="${RUN_1_REAL_GENERATION_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_REAL_GENERATION_GUARD_EVIDENCE="${RUN_1_REAL_GENERATION_GUARD_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_REAL_WORD_REVERSAL_EVIDENCE="${RUN_1_REAL_WORD_REVERSAL_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_GENERATION_COUNT_BEFORE_REVERSAL="${loom_generation_count_before_reversal:-}" \
DELYSIS_SMOKE_RUN_1_GENERATION_COUNT_AFTER_REVERSAL="${loom_generation_count_after_reversal:-}" \
DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER_EDITOR_INPUT="${RUN_1_MANUSCRIPT_SHA256_AFTER_EDITOR_INPUT:-}" \
DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER_FORMATTING="${RUN_1_MANUSCRIPT_SHA256_AFTER_FORMATTING:-}" \
DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_EVIDENCE="${RUN_1_NEW_DOCUMENT_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_PATH="${RUN_1_NEW_DOCUMENT_PATH:-}" \
DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_SENTINEL="${RUN_1_NEW_DOCUMENT_SENTINEL:-}" \
DELYSIS_SMOKE_RUN_1_PROJECT_BUSY_MONITOR_EVIDENCE="${RUN_1_PROJECT_BUSY_MONITOR_EVIDENCE:-}" \
DELYSIS_SMOKE_RUN_1_PROJECT_BUSY_LOG_EVIDENCE="${RUN_1_PROJECT_BUSY_LOG_EVIDENCE:-}" \
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
const editorInput = e.DELYSIS_SMOKE_RUN_1_EDITOR_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_EDITOR_EVIDENCE)
  : null;
const wysiwyg = e.DELYSIS_SMOKE_RUN_1_WYSIWYG_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_WYSIWYG_EVIDENCE)
  : null;
const newDocument = e.DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_NEW_DOCUMENT_EVIDENCE)
  : null;
const formatting = e.DELYSIS_SMOKE_RUN_1_FORMATTING_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_FORMATTING_EVIDENCE)
  : null;
const cachedCompletionInteractions = e.DELYSIS_SMOKE_RUN_1_REAL_WORD_REVERSAL_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_REAL_WORD_REVERSAL_EVIDENCE)
  : null;
const generationFamily = e.DELYSIS_SMOKE_RUN_1_REAL_GENERATION_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_REAL_GENERATION_EVIDENCE)
  : null;
const generationGuard = e.DELYSIS_SMOKE_RUN_1_REAL_GENERATION_GUARD_EVIDENCE
  ? JSON.parse(e.DELYSIS_SMOKE_RUN_1_REAL_GENERATION_GUARD_EVIDENCE)
  : null;
const autocompleteActivation = e.DELYSIS_SMOKE_RUN_1_AUTOCOMPLETE_OFF_EVIDENCE ? {
  off_before_typing: JSON.parse(e.DELYSIS_SMOKE_RUN_1_AUTOCOMPLETE_OFF_EVIDENCE),
  enabled_after_canonical_persistence: JSON.parse(e.DELYSIS_SMOKE_RUN_1_AUTOCOMPLETE_ENABLE_EVIDENCE),
} : null;
const projectBusyRegression = e.DELYSIS_SMOKE_RUN_1_PROJECT_BUSY_MONITOR_EVIDENCE ? {
  accessibility_monitor: JSON.parse(e.DELYSIS_SMOKE_RUN_1_PROJECT_BUSY_MONITOR_EVIDENCE),
  launch_log_scan: JSON.parse(e.DELYSIS_SMOKE_RUN_1_PROJECT_BUSY_LOG_EVIDENCE),
} : null;
const momUiIdentity = (value) => value ? JSON.parse(value) : null;
const receipt = {
  schema: "delysis.macos-packaged-app-smoke.v1",
  created_at: new Date().toISOString(),
  component: e.DELYSIS_SMOKE_COMPONENT,
  bundle: e.DELYSIS_SMOKE_BUNDLE,
  bundle_id: e.DELYSIS_SMOKE_BUNDLE_ID,
  source_git_sha: e.DELYSIS_SMOKE_SOURCE_SHA || null,
  executable_path: e.DELYSIS_SMOKE_EXECUTABLE_PATH,
  executable_device_inode: e.DELYSIS_SMOKE_EXECUTABLE_FILE_ID,
  executable_sha256: e.DELYSIS_SMOKE_EXECUTABLE_SHA,
  input_archive: e.DELYSIS_SMOKE_INPUT_ARCHIVE || null,
  input_archive_sha256: e.DELYSIS_SMOKE_INPUT_ARCHIVE_SHA || null,
  input_release_receipt: e.DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT || null,
  input_release_receipt_sha256: e.DELYSIS_SMOKE_INPUT_RELEASE_RECEIPT_SHA || null,
  app_owned_state_root: e.DELYSIS_SMOKE_STATE_ROOT,
  launches: [
    {
      pid: Number(e.DELYSIS_SMOKE_RUN_1_PID),
      executable_path: e.DELYSIS_SMOKE_EXECUTABLE_PATH,
      executable_device_inode: e.DELYSIS_SMOKE_RUN_1_EXECUTABLE_FILE_ID,
      executable_sha256: e.DELYSIS_SMOKE_RUN_1_EXECUTABLE_SHA,
      ui_identity: momUiIdentity(e.DELYSIS_SMOKE_RUN_1_MOM_UI_IDENTITY),
      window_observed: true,
      product_ready_at_state_root: true,
      titlebar_drag: titlebarDrag,
      manuscript_sha256_before_drag: e.DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_BEFORE || null,
      manuscript_sha256_after_drag: e.DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER || null,
      completion_controls: completionControls,
      project_busy_regression: projectBusyRegression,
      editor_input: editorInput ? {
        ...editorInput,
        typed_terminal_space_sentinel: e.DELYSIS_SMOKE_RUN_1_EDITOR_INPUT_SENTINEL,
        canonical_manuscript: e.DELYSIS_SMOKE_RUN_1_EDITOR_SENTINEL,
        live_wysiwyg_after_persistence: wysiwyg,
        manuscript_sha256_after_input: e.DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER_EDITOR_INPUT,
      } : null,
      visual_formatting: formatting ? {
        ...formatting,
        observed_persisted_markdown: e.DELYSIS_SMOKE_RUN_1_FORMATTED_SENTINEL,
        manuscript_sha256_after_formatting: e.DELYSIS_SMOKE_RUN_1_MANUSCRIPT_SHA_AFTER_FORMATTING,
      } : null,
      real_model_completion: e.DELYSIS_SMOKE_RUN_1_REAL_GHOST_EVIDENCE ? {
        autocomplete_activation: autocompleteActivation,
        generation_family: generationFamily,
        generation_family_guard: generationGuard,
        ghost_presentation: e.DELYSIS_SMOKE_RUN_1_REAL_GHOST_EVIDENCE,
        cached_completion_interactions: cachedCompletionInteractions ? {
          ...cachedCompletionInteractions,
          generation_runs_before: Number(e.DELYSIS_SMOKE_RUN_1_GENERATION_COUNT_BEFORE_REVERSAL),
          generation_runs_after: Number(e.DELYSIS_SMOKE_RUN_1_GENERATION_COUNT_AFTER_REVERSAL),
        } : null,
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
      executable_path: e.DELYSIS_SMOKE_EXECUTABLE_PATH,
      executable_device_inode: e.DELYSIS_SMOKE_RUN_2_EXECUTABLE_FILE_ID,
      executable_sha256: e.DELYSIS_SMOKE_RUN_2_EXECUTABLE_SHA,
      ui_identity: momUiIdentity(e.DELYSIS_SMOKE_RUN_2_MOM_UI_IDENTITY),
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

if [ -n "$LOOM_SMOKE_MODEL_LINK" ] && [ -f "$LOOM_SMOKE_MODEL_LINK" ]; then
  unlink "$LOOM_SMOKE_MODEL_LINK"
  LOOM_SMOKE_MODEL_LINK=
fi

trap - EXIT HUP INT TERM
echo "packaged-app smoke passed twice: $COMPONENT"
echo "smoke evidence: $SMOKE_ROOT"
echo "receipt: $RECEIPT"
if [ -n "$RECEIPT_DESTINATION" ]; then
  echo "copied receipt: $RECEIPT_DESTINATION"
fi
