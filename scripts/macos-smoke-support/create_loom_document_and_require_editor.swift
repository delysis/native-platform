import ApplicationServices
import AppKit
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

func controls(named needle: String) -> [AXUIElement] {
    descendants().filter { element in
        let role = stringAttribute(element, kAXRoleAttribute as CFString)
        // WebKit exposes the Add button as a pop-up button when aria-haspopup
        // is a menu, even though the DOM element is still a button.
        guard role == kAXButtonRole as String ||
              role == kAXPopUpButtonRole as String ||
              role == kAXMenuItemRole as String else {
            return false
        }
        let labels = [
            stringAttribute(element, kAXDescriptionAttribute as CFString),
            stringAttribute(element, kAXTitleAttribute as CFString),
            stringAttribute(element, kAXHelpAttribute as CFString)
        ]
        return labels.contains(needle) || labels.contains { $0.hasPrefix("\(needle) (") }
    }
}

func uniqueControl(named needle: String) -> AXUIElement? {
    let matches = controls(named: needle)
    guard matches.count == 1 else {
        fputs("expected one exact '\(needle)' control, observed \(matches.count)\n", stderr)
        return nil
    }
    return matches[0]
}

func firstWindow() -> AXUIElement? {
    (attribute(application, kAXWindowsAttribute as CFString) as? [AXUIElement])?.first
}

func manuscriptEditor() -> AXUIElement? {
    descendants().first { element in
        stringAttribute(element, kAXRoleAttribute as CFString) == kAXTextAreaRole as String &&
            stringAttribute(element, kAXDescriptionAttribute as CFString).hasSuffix(", manuscript editor")
    }
}

@discardableResult
func postKey(_ key: CGKeyCode) -> Bool {
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: key, keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: key, keyDown: false) else {
        return false
    }
    down.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.03)
    up.post(tap: .cghidEventTap)
    return true
}

func editorText(_ editor: AXUIElement) -> String {
    stringAttribute(editor, kAXValueAttribute as CFString)
        .trimmingCharacters(in: .newlines)
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

func documentIds() -> Set<String> {
    Set(descendants().flatMap { element in
        [kAXValueAttribute, kAXTitleAttribute, kAXDescriptionAttribute, kAXHelpAttribute]
            .compactMap { attribute(element, $0 as CFString) as? String }
            .compactMap { jsonObject(in: $0, schema: "delysis.loom-completion-witness.v1") }
            .compactMap { ($0["pre_admission"] as? [String: Any])?["document_id"] as? String }
            .filter { !$0.isEmpty }
    })
}

guard let runningApplication = NSRunningApplication(processIdentifier: pid) else {
    fputs("could not bind the new-document check to Loom's exact application\n", stderr)
    exit(1)
}
runningApplication.activate(options: [.activateAllWindows])
let windowDeadline = Date().addingTimeInterval(5)
var initialWindow: AXUIElement?
repeat {
    initialWindow = firstWindow()
    if initialWindow != nil { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < windowDeadline
guard let window = initialWindow else {
    fputs("could not bind the new-document check to Loom's exact accessible window\n", stderr)
    exit(1)
}
let beforeTitle = stringAttribute(window, kAXTitleAttribute as CFString)
let beforeDocumentIds = documentIds()
guard !beforeDocumentIds.isEmpty else {
    fputs("could not bind the new-document check to Loom's current document identity\n", stderr)
    exit(1)
}
let addDeadline = Date().addingTimeInterval(5)
var add: AXUIElement?
repeat {
    add = uniqueControl(named: "Add")
    if add != nil { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < addDeadline
if let add {
    let expanded = (attribute(add, kAXExpandedAttribute as CFString) as? Bool) == true
    if !expanded {
        guard AXUIElementPerformAction(add, kAXPressAction as CFString) == .success else {
            fputs("could not open Loom's Add menu\n", stderr)
            exit(1)
        }
    }
} else {
    fputs("could not bind Loom's Add menu control\n", stderr)
    exit(1)
}
let createDeadline = Date().addingTimeInterval(5)
var create: AXUIElement?
repeat {
    create = uniqueControl(named: "New document")
    if create != nil { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < createDeadline
guard let create else {
    fputs("could not bind the new-document check to Loom's exact accessible window\n", stderr)
    exit(1)
}
guard AXUIElementPerformAction(create, kAXPressAction as CFString) == .success else {
    fputs("could not press Loom's new-document control\n", stderr)
    exit(1)
}

let deadline = Date().addingTimeInterval(10)
var afterTitle = beforeTitle
var afterDocumentId: String?
repeat {
    // AXPress is an external accessibility action rather than a physical menu
    // selection. Restore the exact app's key status before judging the DOM
    // focus that the product assigned to its new editor.
    runningApplication.activate(options: [.activateAllWindows])
    _ = AXUIElementSetAttributeValue(
        application,
        kAXFrontmostAttribute as CFString,
        kCFBooleanTrue
    )
    // Creating a document may replace WebKit's native accessibility window.
    // Rebind through the exact application PID instead of reading a detached
    // pre-command object whose title can never advance.
    if let currentWindow = firstWindow() {
        afterTitle = stringAttribute(currentWindow, kAXTitleAttribute as CFString)
    }
    afterDocumentId = documentIds().subtracting(beforeDocumentIds).sorted().first
    if afterDocumentId != nil && manuscriptEditor() != nil { break }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < deadline

guard let afterDocumentId, let editor = manuscriptEditor() else {
    fputs(
        "new document did not expose a fresh writing surface " +
        "(before_ids=\(beforeDocumentIds.sorted()), after_ids=\(documentIds().sorted()))\n",
        stderr
    )
    exit(1)
}

// The new identity is published before the async menu handler completes its
// final editor-focus handoff. Wait for that bounded handoff rather than racing
// the first system key against an otherwise valid navigation.
Thread.sleep(forTimeInterval: 3)
runningApplication.activate(options: [.activateAllWindows])
_ = AXUIElementSetAttributeValue(
    application,
    kAXFrontmostAttribute as CFString,
    kCFBooleanTrue
)

// WebKit can retain the contenteditable keyboard target while AX continues to
// name the containing web area until the first edit. Probe that real target
// without relying on a focus-attribute echo, then restore the untouched new
// manuscript before the caller supplies its sentinel.
guard editorText(editor).isEmpty, postKey(7) else {
    fputs("new document was not empty or could not receive its native focus probe\n", stderr)
    exit(1)
}
let insertionDeadline = Date().addingTimeInterval(3)
var focusProbeInserted = false
repeat {
    focusProbeInserted = editorText(editor) == "x" &&
        (attribute(editor, kAXFocusedAttribute as CFString) as? Bool) == true
    if focusProbeInserted { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < insertionDeadline
guard focusProbeInserted, postKey(51) else {
    fputs("new document did not route native input into its focused writing surface\n", stderr)
    exit(1)
}
let rollbackDeadline = Date().addingTimeInterval(3)
var focusProbeReversed = false
repeat {
    focusProbeReversed = editorText(editor).isEmpty &&
        (attribute(editor, kAXFocusedAttribute as CFString) as? Bool) == true
    if focusProbeReversed { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < rollbackDeadline
guard focusProbeReversed else {
    fputs("new-document focus probe did not restore the untouched manuscript\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "title_before": beforeTitle,
    "title_after": afterTitle,
    "document_ids_before": beforeDocumentIds.sorted(),
    "document_id_after": afterDocumentId,
    "focused_editor_observed": true,
    "native_input_focus_probe_reversed": true
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
