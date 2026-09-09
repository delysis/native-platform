import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let application = AXUIElementCreateApplication(pid)
let personaPanelWitness = "Use a Persona's menu to start a conversation, edit its profile, or remove it from the library."
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

func normalized(_ value: String) -> String {
    value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
}

func pressableControl(exactLabel: String) -> AXUIElement? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 8192 {
        let element = queue[cursor]
        cursor += 1
        let strings = stringAttributes.compactMap { attribute(element, $0) as? String }
        var actions: CFArray?
        if strings.contains(where: { normalized($0) == normalized(exactLabel) }),
           AXUIElementCopyActionNames(element, &actions) == .success,
           (actions as? [String])?.contains(kAXPressAction as String) == true {
            return element
        }
        if let children = attribute(element, kAXChildrenAttribute as String) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

func containsExactString(_ expected: String) -> Bool {
    accessibilityStrings().contains { normalized($0) == normalized(expected) }
}

let settingsDeadline = Date().addingTimeInterval(20)
var settingsControl: AXUIElement?
repeat {
    let strings = accessibilityStrings()
    if let legacy = legacyMarkers.first(where: { needle in
        strings.contains(where: { $0.localizedCaseInsensitiveContains(needle) })
    }) {
        fputs("legacy Mom UI text is visible in the exact PID: \(legacy)\n", stderr)
        exit(1)
    }
    settingsControl = pressableControl(exactLabel: "Settings")
    if settingsControl != nil { break }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < settingsDeadline

guard let settingsControl,
      AXUIElementPerformAction(settingsControl, kAXPressAction as CFString) == .success else {
    fputs("the exact Mom PID did not expose a pressable Settings control\n", stderr)
    exit(1)
}

let personasDeadline = Date().addingTimeInterval(20)
var personasControl: AXUIElement?
repeat {
    personasControl = pressableControl(exactLabel: "Personas")
    if personasControl != nil { break }
    Thread.sleep(forTimeInterval: 0.1)
} while Date() < personasDeadline

guard let personasControl,
      AXUIElementPerformAction(personasControl, kAXPressAction as CFString) == .success else {
    fputs("the exact Mom PID did not expose a pressable Personas settings tab\n", stderr)
    exit(1)
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
    if containsExactString(personaPanelWitness) {
        guard let closeControl = pressableControl(exactLabel: "Close settings"),
              AXUIElementPerformAction(closeControl, kAXPressAction as CFString) == .success else {
            fputs("the exact Mom PID exposed Personas but Settings could not be closed\n", stderr)
            exit(1)
        }
        let restoreDeadline = Date().addingTimeInterval(20)
        repeat {
            if pressableControl(exactLabel: "Close settings") == nil,
               pressableControl(exactLabel: "Settings") != nil {
                break
            }
            Thread.sleep(forTimeInterval: 0.1)
        } while Date() < restoreDeadline
        guard pressableControl(exactLabel: "Close settings") == nil,
              pressableControl(exactLabel: "Settings") != nil else {
            fputs("the exact Mom PID did not restore the initial chat surface after closing Settings\n", stderr)
            exit(1)
        }
        let evidence: [String: Any] = [
            "pid": pid,
            "current_marker": personaPanelWitness,
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
