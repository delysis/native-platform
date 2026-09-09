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
