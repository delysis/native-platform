import ApplicationServices
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]) else {
    fputs("usage: position_loom_window_right <pid>\n", stderr)
    exit(2)
}

func onScreenWindowFrame(for pid: Int32) -> CGRect? {
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    for window in windows {
        guard (window[kCGWindowOwnerPID as String] as? Int32) == pid,
              (window[kCGWindowLayer as String] as? Int) == 0,
              let bounds = window[kCGWindowBounds as String] as? [String: Any],
              let x = bounds["X"] as? Double,
              let y = bounds["Y"] as? Double,
              let width = bounds["Width"] as? Double,
              let height = bounds["Height"] as? Double,
              width > 0, height > 0 else {
            continue
        }
        return CGRect(x: x, y: y, width: width, height: height)
    }
    return nil
}

guard let before = onScreenWindowFrame(for: pid) else {
    fputs("could not find Loom's exact on-screen window to position\n", stderr)
    exit(1)
}

let application = AXUIElementCreateApplication(pid)
var windowsValue: CFTypeRef?
guard AXUIElementCopyAttributeValue(application, kAXWindowsAttribute as CFString, &windowsValue) == .success,
      let windows = windowsValue as? [AXUIElement],
      let window = windows.first else {
    fputs("could not bind window positioning to Loom's exact accessibility window\n", stderr)
    exit(1)
}

// Native interaction still needs a frontmost target. Keep the acceptance
// window visibly confined to the right half of the primary display so a human
// can retain the left side for unrelated work between test interactions.
let display = CGDisplayBounds(CGMainDisplayID())
let width = min(before.width, max(640, display.width * 0.48))
let height = min(before.height, max(600, display.height - 80))
let target = CGRect(
    x: display.maxX - width - 16,
    y: max(display.minY + 40, min(before.minY, display.maxY - height - 40)),
    width: width,
    height: height
)
var position = target.origin
var size = target.size
guard let positionValue = AXValueCreate(.cgPoint, &position),
      let sizeValue = AXValueCreate(.cgSize, &size),
      AXUIElementSetAttributeValue(window, kAXPositionAttribute as CFString, positionValue) == .success,
      AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString, sizeValue) == .success else {
    fputs("could not position Loom's exact accessibility window on the right\n", stderr)
    exit(1)
}

let deadline = Date().addingTimeInterval(4)
var after = before
repeat {
    Thread.sleep(forTimeInterval: 0.05)
    after = onScreenWindowFrame(for: pid) ?? before
    if abs(after.minX - target.minX) < 4, abs(after.width - target.width) < 4 { break }
} while Date() < deadline

guard abs(after.minX - target.minX) < 4, abs(after.width - target.width) < 4 else {
    fputs("Loom window did not reach the requested right-side frame\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "pid": pid,
    "before": ["x": before.minX, "y": before.minY, "width": before.width, "height": before.height],
    "after": ["x": after.minX, "y": after.minY, "width": after.width, "height": after.height],
    "display": ["x": display.minX, "y": display.minY, "width": display.width, "height": display.height]
]
let data = try JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(decoding: data, as: UTF8.self))
