import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let stopPath = CommandLine.arguments[2]
let readyPath = CommandLine.arguments[3]
let failurePath = CommandLine.arguments[4]
let application = AXUIElementCreateApplication(pid)
let manager = FileManager.default
let startedAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
var polls = 0

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func projectBusyMatch() -> [String: Any]? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let values = [kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute, kAXValueAttribute]
            .compactMap { attribute(element, $0 as CFString) as? String }
            .filter { !$0.isEmpty }
        if values.contains(where: {
            $0.localizedCaseInsensitiveContains("project_busy") ||
                $0.localizedCaseInsensitiveContains("another bounded project operation is still running")
        }) {
            return [
                "role": (attribute(element, kAXRoleAttribute as CFString) as? String) ?? "",
                "strings": values
            ]
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

_ = manager.createFile(atPath: readyPath, contents: Data())
while !manager.fileExists(atPath: stopPath) {
    polls += 1
    if let match = projectBusyMatch() {
        let evidence: [String: Any] = [
            "pid": pid,
            "started_at_ms": startedAtMs,
            "detected_at_ms": Int64(Date().timeIntervalSince1970 * 1_000),
            "polls": polls,
            "project_busy_alert_observed": true,
            "match": match
        ]
        let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
        try? data.write(to: URL(fileURLWithPath: failurePath), options: [.atomic])
        print(String(data: data, encoding: .utf8)!)
        fputs("Loom exposed a project_busy alert during native smoke\n", stderr)
        exit(42)
    }
    Thread.sleep(forTimeInterval: 0.05)
}

let evidence: [String: Any] = [
    "pid": pid,
    "started_at_ms": startedAtMs,
    "finished_at_ms": Int64(Date().timeIntervalSince1970 * 1_000),
    "polls": polls,
    "project_busy_alert_observed": false
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
