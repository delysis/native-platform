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
