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

// Product-state readiness can precede the Svelte document-open transition,
// especially while a large default writer is being inspected or loaded. A
// successful setter is not evidence. Retry the exact PID-bound AX value and
// range mutation until the visible value and collapsed end caret remain
// jointly stable; this avoids contaminating the manuscript with a delayed
// synthetic keyboard queue while still exercising WebKit's native edit path.
var observedEditorValue = ""
var observedSelection: CFRange?
var stabilized = false
var dispatchCount = 0
let terminalSpace = sentinel.hasSuffix(" ")
let seededValue = terminalSpace ? String(sentinel.dropLast()) : sentinel
for _ in 0..<60 {
    guard AXUIElementSetAttributeValue(
        editor,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
    ) == .success else {
        fputs("could not focus Loom's accessible manuscript text area\n", stderr)
        exit(1)
    }
    guard AXUIElementSetAttributeValue(
        editor,
        kAXValueAttribute as CFString,
        seededValue as CFString
    ) == .success else {
        fputs("could not set Loom's exact accessible manuscript value\n", stderr)
        exit(1)
    }
    var endRange = CFRange(location: seededValue.utf16.count, length: 0)
    guard let endRangeValue = AXValueCreate(.cfRange, &endRange),
          AXUIElementSetAttributeValue(
            editor,
            kAXSelectedTextRangeAttribute as CFString,
            endRangeValue
          ) == .success else {
        fputs("could not set Loom's exact accessible manuscript caret\n", stderr)
        exit(1)
    }
    if terminalSpace {
        guard let spaceDown = CGEvent(
                keyboardEventSource: nil,
                virtualKey: 49,
                keyDown: true
              ),
              let spaceUp = CGEvent(
                keyboardEventSource: nil,
                virtualKey: 49,
                keyDown: false
              ) else {
            fputs("could not construct Loom's terminal Space key event\n", stderr)
            exit(1)
        }
        spaceDown.postToPid(pid)
        Thread.sleep(forTimeInterval: 0.03)
        spaceUp.postToPid(pid)
    }
    dispatchCount += 1

    let attemptDeadline = Date().addingTimeInterval(1.5)
    var exactSince: Date?
    repeat {
        observedEditorValue = stringAttribute(editor, kAXValueAttribute as CFString)
        observedSelection = rangeAttribute(editor, kAXSelectedTextRangeAttribute as CFString)
        if observedEditorValue.trimmingCharacters(in: .newlines) == sentinel,
           observedSelection?.location == sentinel.utf16.count,
           observedSelection?.length == 0 {
            exactSince = exactSince ?? Date()
            if let exactSince,
               Date().timeIntervalSince(exactSince) >= 0.4 {
                stabilized = true
                break
            }
        } else {
            exactSince = nil
        }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < attemptDeadline
    if stabilized { break }
    Thread.sleep(forTimeInterval: 0.25)
}

guard stabilized,
      observedEditorValue.trimmingCharacters(in: .newlines) == sentinel,
      let observedSelection,
      observedSelection.location == sentinel.utf16.count,
      observedSelection.length == 0 else {
    fputs("native Accessibility input did not stabilize at the exact value and collapsed end caret\n", stderr)
    exit(1)
}
let evidence: [String: Any] = [
    "dispatch": "PID-targeted AXValue and AXSelectedTextRange",
    "dispatch_count": dispatchCount,
    "terminal_space_key_event": terminalSpace,
    "stable_seconds": 0.4,
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
