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
