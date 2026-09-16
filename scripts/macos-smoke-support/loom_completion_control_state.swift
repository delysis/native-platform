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
    for value in values {
        if let data = value.data(using: .utf8),
           let witness = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           witness["schema"] as? String == "delysis.loom-completion-witness.v1" {
            print(value)
            exit(0)
        }
    }
    if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
        queue.append(contentsOf: children)
    }
}
fputs("could not read Loom's completion state\n", stderr)
exit(1)
