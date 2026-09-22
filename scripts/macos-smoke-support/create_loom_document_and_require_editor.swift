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

func control(named needle: String) -> AXUIElement? {
    descendants().first { element in
        let role = stringAttribute(element, kAXRoleAttribute as CFString)
        guard role == kAXButtonRole as String || role == kAXMenuItemRole as String else {
            return false
        }
        return [
            stringAttribute(element, kAXDescriptionAttribute as CFString),
            stringAttribute(element, kAXTitleAttribute as CFString),
            stringAttribute(element, kAXHelpAttribute as CFString)
        ].joined(separator: " ").contains(needle)
    }
}

guard let window = (attribute(application, kAXWindowsAttribute as CFString) as? [AXUIElement])?.first else {
    fputs("could not bind the new-document check to Loom's exact accessible window\n", stderr)
    exit(1)
}
let beforeTitle = stringAttribute(window, kAXTitleAttribute as CFString)
NSRunningApplication(processIdentifier: pid)?.activate(options: [])
let addDeadline = Date().addingTimeInterval(5)
var add: AXUIElement?
repeat {
    add = control(named: "Add")
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
    create = control(named: "New document")
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
