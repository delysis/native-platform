import AppKit
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments[1])!

func frame() -> CGRect? {
    let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)! as! [[String: Any]]
    for row in rows {
        guard (row[kCGWindowOwnerPID as String] as? Int32) == pid,
              (row[kCGWindowLayer as String] as? Int) == 0,
              let bounds = row[kCGWindowBounds as String] as? [String: Any],
              let x = bounds["X"] as? Double,
              let y = bounds["Y"] as? Double,
              let width = bounds["Width"] as? Double,
              let height = bounds["Height"] as? Double,
              width > 0, height > 0 else { continue }
        return CGRect(x: x, y: y, width: width, height: height)
    }
    return nil
}

guard let before = frame() else {
    fputs("could not bind titlebar drag to the exact application window\n", stderr)
    exit(1)
}

NSRunningApplication(processIdentifier: pid)?.activate(options: [])
Thread.sleep(forTimeInterval: 0.75)

// The centered native title hit region belongs to AppKit even when its text is
// hidden. Exercise Loom's explicit noninteractive web drag strip to its left.
let start = CGPoint(x: before.minX + before.width * 0.30, y: before.minY + 15)
let horizontal = before.minX + before.width + 72 < 1500 ? 64.0 : -64.0
let vertical = 0.0
let finish = CGPoint(x: start.x + horizontal, y: start.y + vertical)
guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: start, mouseButton: .left),
      let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: finish, mouseButton: .left) else {
    fputs("could not construct titlebar drag events\n", stderr)
    exit(1)
}
down.post(tap: .cghidEventTap)
Thread.sleep(forTimeInterval: 0.30)
for step in 1...8 {
    let fraction = Double(step) / 8.0
    let point = CGPoint(
        x: start.x + horizontal * fraction,
        y: start.y + vertical * fraction
    )
    guard let drag = CGEvent(
        mouseEventSource: nil,
        mouseType: .leftMouseDragged,
        mouseCursorPosition: point,
        mouseButton: .left
    ) else {
        fputs("could not construct an intermediate titlebar drag event\n", stderr)
        exit(1)
    }
    drag.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.08)
}
up.post(tap: .cghidEventTap)

let deadline = Date().addingTimeInterval(4)
var after = before
repeat {
    Thread.sleep(forTimeInterval: 0.1)
    after = frame() ?? before
    if abs(after.minX - before.minX) >= 8 || abs(after.minY - before.minY) >= 8 { break }
} while Date() < deadline

guard abs(after.minX - before.minX) >= 8 || abs(after.minY - before.minY) >= 8 else {
    fputs("titlebar drag was dispatched but the bound window frame did not move\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "pid": pid,
    "before": ["x": before.minX, "y": before.minY, "width": before.width, "height": before.height],
    "after": ["x": after.minX, "y": after.minY, "width": after.width, "height": after.height],
    "delta": ["x": after.minX - before.minX, "y": after.minY - before.minY]
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
