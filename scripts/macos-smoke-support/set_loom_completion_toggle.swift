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
