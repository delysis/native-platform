import AppKit
import ApplicationServices
import Foundation

// Controls-only check. Native generation is proved by the separate model journey.
guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]), pid > 0,
      let running = NSRunningApplication(processIdentifier: pid) else {
    fputs("usage: exercise_loom_completion_controls <pid>\n", stderr); exit(2)
}
let application = AXUIElementCreateApplication(pid)
AXUIElementSetMessagingTimeout(application, 0.5)
func fail(_ message: String) -> Never { fputs("\(message)\n", stderr); exit(1) }
let helper = Process()
helper.executableURL = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
    .appendingPathComponent("set_loom_completion_toggle")
helper.arguments = [String(pid), "disable", "disabled", "allow-already"]
// The child emits one bounded JSON object; stderr remains visible to the caller.
let output = Pipe()
helper.standardOutput = output
try helper.run()
let deadline = ProcessInfo.processInfo.systemUptime + 35
while helper.isRunning && ProcessInfo.processInfo.systemUptime < deadline { Thread.sleep(forTimeInterval: 0.05) }
if helper.isRunning { helper.terminate(); helper.waitUntilExit(); fail("policy control helper exceeded its deadline") }
helper.waitUntilExit()
guard helper.terminationStatus == 0 else { fail("could not establish suggestions-off using the current control") }
let controlBytes = output.fileHandleForReading.readDataToEndOfFile()
guard let control = try JSONSerialization.jsonObject(with: controlBytes) as? [String: Any],
      control["suggestions_enabled"] as? Bool == false else { fail("control did not prove suggestions-off") }

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    return AXUIElementCopyAttributeValue(element, name, &value) == .success ? value : nil
}
func currentPolicy() -> (autocomplete: Bool, shuttle: Bool)? {
    var queue = [application], cursor = 0
    var values: [Data: [String: Any]] = [:]
    let until = ProcessInfo.processInfo.systemUptime + 2
    while cursor < queue.count {
        guard queue.count <= 4096, ProcessInfo.processInfo.systemUptime < until else { return nil }
        let element = queue[cursor]; cursor += 1
        for name in [kAXValueAttribute, kAXTitleAttribute, kAXDescriptionAttribute] {
            guard let text = attribute(element, name as CFString) as? String,
                  let marker = text.range(of: "\"schema\":\"delysis.loom-completion-witness.v1\""),
                  let open = text[..<marker.lowerBound].lastIndex(of: "{"), let close = text.lastIndex(of: "}"), open <= close,
                  let data = String(text[open...close]).data(using: .utf8),
                  let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let canonical = try? JSONSerialization.data(withJSONObject: json, options: [.sortedKeys]) else { continue }
            values[canonical] = json
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] { queue.append(contentsOf: children) }
    }
    guard values.count == 1, let value = values.values.first,
          let autocomplete = value["autocomplete_enabled"] as? Bool,
          let shuttle = value["shuttle_enabled"] as? Bool else { return nil }
    return (autocomplete, shuttle)
}
func waitForShuttle(_ expected: Bool) -> Bool {
    let deadline = ProcessInfo.processInfo.systemUptime + 15
    repeat {
        guard !running.isTerminated else { return false }
        if let state = currentPolicy(), !state.autocomplete && state.shuttle == expected { return true }
        Thread.sleep(forTimeInterval: 0.05)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return false
}
func toggleShuttle() -> Bool {
    _ = running.activate(options: [])
    Thread.sleep(forTimeInterval: 0.1)
    guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
          let down = CGEvent(keyboardEventSource: nil, virtualKey: 38, keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: 38, keyDown: false) else { return false }
    down.flags = [.maskCommand, .maskShift]; up.flags = [.maskCommand, .maskShift]
    down.postToPid(pid); up.postToPid(pid)
    return true
}
guard waitForShuttle(false), toggleShuttle(), waitForShuttle(true), toggleShuttle(), waitForShuttle(false) else {
    fail("Cmd+Shift+J did not prove Shuttle off-on-off independently of suggestions")
}
let evidence: [String: Any] = ["evidence_kind": "accessibility_controls_only", "autocomplete": "off",
    "shuttle_transition": "off-on-off", "shuttle_enabled_while_autocomplete_off": true,
    "shuttle_activation": "Cmd+Shift+J", "current_control": control]
let data = try JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
