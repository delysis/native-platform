import Foundation
#if os(macOS)
import AppKit
import ApplicationServices
#endif

struct CompletionControl: Equatable {
    let mode: String
    let suggestionsEnabled: Bool

    // Match the actual button's separate AX fields, never substrings in help,
    // status prose, ancestors, or obsolete on/off labels. The toolbar switches
    // modes when enabled; pressing it is NOT the on/off operation.
    static func read(role: String, fields: [String]) -> CompletionControl? {
        guard role == "AXButton" else { return nil }
        let modes = ["Ghost text", "Loompad"].filter { fields.contains($0) }
        let actions = ["Enable suggestions", "Switch to Loompad", "Switch to ghost text"]
            .filter { fields.contains($0) }
        guard modes.count == 1, actions.count == 1 else { return nil }
        let mode = modes[0]
        switch actions[0] {
        case "Enable suggestions":
            return CompletionControl(mode: mode, suggestionsEnabled: false)
        case "Switch to Loompad" where mode == "Ghost text":
            return CompletionControl(mode: mode, suggestionsEnabled: true)
        case "Switch to ghost text" where mode == "Loompad":
            return CompletionControl(mode: mode, suggestionsEnabled: true)
        default:
            return nil
        }
    }
}

func requestedEnabled(_ request: String, _ result: String) -> Bool? {
    switch (request, result) {
    case ("enable", "enabled"), ("Turn autocomplete on", "Turn autocomplete off"):
        return true
    case ("disable", "disabled"), ("Turn autocomplete off", "Turn autocomplete on"):
        return false
    default:
        return nil
    }
}

func fail(_ message: String, code: Int32 = 1) -> Never {
    fputs("\(message)\n", stderr)
    exit(code)
}

func emit(_ evidence: [String: Any]) {
    do {
        let data = try JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
        guard let text = String(data: data, encoding: .utf8) else { fail("invalid evidence encoding") }
        print(text)
    } catch {
        fail("could not serialize control evidence: \(error)")
    }
}

// Pure AX-contract checks; this does not establish a native UI or inference run.
func selfTest() {
    var checks = 0
    func check(_ condition: Bool, _ name: String) {
        guard condition else { fail("control contract regression: \(name)") }
        checks += 1
    }
    for mode in ["Ghost text", "Loompad"] {
        check(CompletionControl.read(role: "AXButton", fields: [mode, "Enable suggestions"]) ==
            CompletionControl(mode: mode, suggestionsEnabled: false), "disabled \(mode)")
        let action = mode == "Ghost text" ? "Switch to Loompad" : "Switch to ghost text"
        check(CompletionControl.read(role: "AXButton", fields: [mode, action]) ==
            CompletionControl(mode: mode, suggestionsEnabled: true), "enabled \(mode)")
        check(CompletionControl.read(role: "AXStaticText", fields: [mode, action]) == nil, "reject status prose")
        check(CompletionControl.read(role: "AXButton", fields: [mode]) == nil, "name alone is not state")
    }
    for fields in [
        ["Turn autocomplete on"], ["Turn autocomplete off"],
        ["Help: Ghost text", "Enable suggestions"],
        ["Ghost text", "Switch to ghost text"],
        ["Loompad", "Switch to Loompad"],
        ["Ghost text", "Loompad", "Enable suggestions"],
        ["Ghost text", "Enable suggestions", "Switch to Loompad"]
    ] {
        check(CompletionControl.read(role: "AXButton", fields: fields) == nil, "reject ambiguous or retired fields")
    }
    check(requestedEnabled("enable", "enabled") == true, "enable intent")
    check(requestedEnabled("disable", "disabled") == false, "disable intent")
    // These two pairs are still passed by smoke-macos-app.sh. They are intent
    // arguments only, not strings searched for in the current accessibility UI.
    check(requestedEnabled("Turn autocomplete on", "Turn autocomplete off") == true, "existing enable caller")
    check(requestedEnabled("Turn autocomplete off", "Turn autocomplete on") == false, "existing disable caller")
    check(requestedEnabled("Ghost text", "Loompad") == nil, "mode switch is not a policy toggle")
    check(requestedEnabled("enable", "disabled") == nil, "reject contradictory intent")
    emit(["component": "completion-control-contract", "checks": checks, "native_acceptance": false])
}

if CommandLine.arguments == [CommandLine.arguments[0], "--self-test"] {
    selfTest()
    exit(0)
}

#if os(macOS)
let arguments = Array(CommandLine.arguments.dropFirst())
let queryOnly = arguments.first == "--state"
let pidArgument = queryOnly ? arguments.dropFirst().first : arguments.first
guard let pidText = pidArgument, let pid = Int32(pidText), pid > 0 else {
    fail("usage: set_loom_completion_toggle <pid> enable enabled|disable disabled [require-press|allow-already]; or --state <pid>", code: 2)
}
let desired: Bool?
let requirePress: Bool
if queryOnly {
    guard arguments.count == 2 else { fail("--state requires exactly one PID", code: 2) }
    desired = nil
    requirePress = false
} else {
    guard arguments.count == 3 || arguments.count == 4,
          let value = requestedEnabled(arguments[1], arguments[2]) else {
        fail("unsupported or contradictory completion control intent", code: 2)
    }
    if arguments.count == 4 && !["require-press", "allow-already"].contains(arguments[3]) {
        fail("unsupported press requirement", code: 2)
    }
    desired = value
    requirePress = arguments.count == 4 && arguments[3] == "require-press"
}
guard let running = NSRunningApplication(processIdentifier: pid), !running.isTerminated else {
    fail("target process is unavailable")
}
let application = AXUIElementCreateApplication(pid)
guard AXUIElementSetMessagingTimeout(application, 0.5) == .success else {
    fail("could not bound accessibility messaging time")
}

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

let deadline = ProcessInfo.processInfo.systemUptime + 30
var pressed = false
var originalMode: String?
var lastObserved = "no current control"
while ProcessInfo.processInfo.systemUptime < deadline {
    guard !running.isTerminated else { fail("target process exited during completion control change") }
    var queue = [application]
    var cursor = 0
    var matches: [(CompletionControl, Bool)] = []
    while cursor < queue.count && cursor < 4096 && ProcessInfo.processInfo.systemUptime < deadline {
        let element = queue[cursor]
        cursor += 1
        let fields = [kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute]
            .compactMap { attribute(element, $0 as CFString) as? String }
        let role = attribute(element, kAXRoleAttribute as CFString) as? String ?? ""
        if let state = CompletionControl.read(role: role, fields: fields) {
            matches.append((state, (attribute(element, kAXEnabledAttribute as CFString) as? Bool) == true))
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            guard children.count <= 4096 - queue.count else {
                fail("accessibility traversal exceeded its bound; refusing a partial control match")
            }
            queue.append(contentsOf: children)
        }
    }
    guard cursor == queue.count && ProcessInfo.processInfo.systemUptime < deadline else {
        fail("accessibility traversal did not finish before its deadline")
    }
    guard matches.count <= 1 else { fail("ambiguous current completion controls in the exact process") }
    if let (state, actionable) = matches.first {
        lastObserved = "\(state.mode), suggestions=\(state.suggestionsEnabled), actionable=\(actionable)"
        if let originalMode, originalMode != state.mode {
            fail("completion mode changed while toggling suggestion policy: \(lastObserved)")
        }
        if queryOnly {
            emit(["control_label": state.mode, "suggestions_enabled": state.suggestionsEnabled,
                  "enabled": actionable, "evidence_kind": "accessibility_control_state_only"])
            exit(0)
        }
        if actionable && state.suggestionsEnabled == desired {
            guard pressed || !requirePress else { fail("requested state was already present without the required single activation") }
            emit(["requested_control": arguments[1], "resulting_control": arguments[2],
                  "control_label": state.mode, "suggestions_enabled": state.suggestionsEnabled,
                  "pressed_exactly_once": pressed, "activation": pressed ? "Cmd+Shift+G" : "none",
                  "evidence_kind": "accessibility_control_state_only"])
            exit(0)
        }
        if actionable && !pressed {
            // The app's current production shortcut toggles policy without
            // switching Ghost/Loompad or inserting manuscript text. Recheck
            // exact foreground ownership; events are also addressed to this PID.
            guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else {
                fail("target is not frontmost; refusing to send completion shortcut")
            }
            guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 5, keyDown: true),
                  let up = CGEvent(keyboardEventSource: nil, virtualKey: 5, keyDown: false) else {
                fail("could not construct completion shortcut")
            }
            originalMode = state.mode
            down.flags = [.maskCommand, .maskShift]
            up.flags = [.maskCommand, .maskShift]
            down.postToPid(pid)
            up.postToPid(pid)
            pressed = true
        }
    }
    Thread.sleep(forTimeInterval: 0.05)
}
fail("completion control deadline exceeded; last observed: \(lastObserved); shortcut sent: \(pressed). No inference acceptance established.")
#else
fail("native completion control automation requires macOS; only --self-test is portable")
#endif
