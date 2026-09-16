import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let targetState = CommandLine.arguments[2]
guard targetState == "on" || targetState == "off" else {
    fputs("expected autocomplete state on or off\n", stderr)
    exit(2)
}
let enabled = targetState == "on"
let requirePress = CommandLine.arguments[3] == "require-press"
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func autocompleteState() -> Bool? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        if let value = attribute(element, kAXValueAttribute as CFString) as? String,
           let data = value.data(using: .utf8),
           let witness = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           witness["schema"] as? String == "delysis.loom-completion-witness.v1" {
            return witness["autocomplete_enabled"] as? Bool
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

var pressed = false
let deadline = ProcessInfo.processInfo.systemUptime + 15
while ProcessInfo.processInfo.systemUptime < deadline {
    if let current = autocompleteState() {
        if current == enabled {
            guard pressed || !requirePress else {
                fputs("autocomplete already had the requested state before the required key press\n", stderr)
                exit(1)
            }
            let evidence: [String: Any] = [
                "requested_state": targetState,
                "observed_enabled": current,
                "dispatch": "Cmd-Shift-G to exact PID",
                "pressed_exactly_once": pressed
            ]
            let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
            print(String(data: data, encoding: .utf8)!)
            exit(0)
        }
        if !pressed {
            // Autocomplete is intentionally a keyboard power tool, not a button.
            guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 5, keyDown: true),
                  let up = CGEvent(keyboardEventSource: nil, virtualKey: 5, keyDown: false) else {
                fputs("could not construct autocomplete shortcut\n", stderr)
                exit(1)
            }
            down.flags = [.maskCommand, .maskShift]
            up.flags = [.maskCommand, .maskShift]
            down.postToPid(pid)
            up.postToPid(pid)
            pressed = true
        }
    }
    Thread.sleep(forTimeInterval: 0.05)
}
fputs("autocomplete did not reach its requested state through the native shortcut\n", stderr)
exit(1)
