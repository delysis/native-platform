import AppKit
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let expected = CommandLine.arguments[2]
let expectedManuscript = CommandLine.arguments[3]
let asynchronousFailurePaths = [CommandLine.arguments[4], CommandLine.arguments[5]]
    .filter { !$0.isEmpty }
let application = AXUIElementCreateApplication(pid)
guard let runningApplication = NSRunningApplication(processIdentifier: pid) else {
    fputs("Loom's exact completion process exited before visible-presentation focus\n", stderr)
    exit(1)
}

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func rangeAttribute(_ element: AXUIElement, _ name: CFString) -> CFRange? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else {
        return nil
    }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cfRange else { return nil }
    var range = CFRange()
    return AXValueGetValue(value, .cfRange, &range) ? range : nil
}

func withoutTerminalLineBreaks(_ value: String) -> String {
    var normalized = value
    while normalized.last == "\n" || normalized.last == "\r" { normalized.removeLast() }
    return normalized
}

func editor() -> AXUIElement? {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        if (attribute(element, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String {
            return element
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return nil
}

func editorStateIsExact(_ writingSurface: AXUIElement) -> Bool {
    let observed = withoutTerminalLineBreaks(
        (attribute(writingSurface, kAXValueAttribute as CFString) as? String) ?? ""
    )
    let selection = rangeAttribute(
        writingSurface,
        kAXSelectedTextRangeAttribute as CFString
    )
    // WebKit may append a connected ProseMirror decoration to AXValue even
    // when that widget is aria-hidden and absent from canonical manuscript
    // bytes. The exact end-caret plus canonical prefix distinguishes that
    // presentation-only suffix from an editor or persistence divergence.
    return NSWorkspace.shared.frontmostApplication?.processIdentifier == pid &&
        (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true &&
        observed.hasPrefix(expectedManuscript) &&
        selection?.location == expectedManuscript.utf16.count &&
        selection?.length == 0
}

func exactEditorFocusIsCurrent() -> Bool {
    guard let writingSurface = editor() else { return false }
    return editorStateIsExact(writingSurface)
}

func exactEditorFocusDiagnostic() -> String {
    guard let writingSurface = editor() else {
        return "editor=missing,frontmost_pid=\(NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1)"
    }
    let observed = withoutTerminalLineBreaks(
        (attribute(writingSurface, kAXValueAttribute as CFString) as? String) ?? ""
    )
    let selection = rangeAttribute(
        writingSurface,
        kAXSelectedTextRangeAttribute as CFString
    )
    let focused = (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true
    return [
        "frontmost_pid=\(NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1)",
        "focused=\(focused)",
        "value_has_canonical_prefix=\(observed.hasPrefix(expectedManuscript))",
        "selection=\(selection?.location ?? -1):\(selection?.length ?? -1)",
        "expected_selection=\(expectedManuscript.utf16.count):0"
    ].joined(separator: ",")
}

func restoreExactEditorFocus() -> Bool {
    // `activate` may report false when another running instance has the same
    // bundle identifier even though the exact process can still become active.
    // AXFrontmost is PID-bound and is the authoritative activation operation.
    _ = runningApplication.activate(options: [.activateAllWindows])
    guard AXUIElementSetAttributeValue(
            application,
            kAXFrontmostAttribute as CFString,
            kCFBooleanTrue
          ) == .success,
          let writingSurface = editor(),
          AXUIElementSetAttributeValue(
            writingSurface,
            kAXFocusedAttribute as CFString,
            kCFBooleanTrue
          ) == .success else {
        return false
    }
    Thread.sleep(forTimeInterval: 0.05)
    return editorStateIsExact(writingSurface)
}

let focusDeadline = Date().addingTimeInterval(12)
var exactEditorFocused = false
repeat {
    exactEditorFocused = restoreExactEditorFocus()
    if exactEditorFocused { break }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < focusDeadline
guard exactEditorFocused else {
    fputs(
        "Loom did not restore exact foreground editor focus before visible completion proof " +
        "(\(exactEditorFocusDiagnostic()))\n",
        stderr
    )
    exit(1)
}

for _ in 0..<1800 {
    if asynchronousFailurePaths.contains(where: { FileManager.default.fileExists(atPath: $0) }) {
        fputs("Loom failed an asynchronous generation or project_busy guard before the required accessible state\n", stderr)
        exit(1)
    }
    // Packaged smoke owns the visible interaction interval. Reassert the exact
    // PID and canonical caret periodically so another same-bundle window cannot
    // turn the focus-gated ghost requirement into a false negative.
    if !exactEditorFocusIsCurrent() && !restoreExactEditorFocus() {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        let strings = [kAXDescriptionAttribute, kAXTitleAttribute, kAXValueAttribute]
            .compactMap { attribute(element, $0 as CFString) as? String }
        if strings.contains(where: { $0.contains(expected) }) {
            print(expected)
            exit(0)
        }
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    Thread.sleep(forTimeInterval: 0.2)
}
fputs("Loom never exposed the required accessible runtime state: \(expected)\n", stderr)
exit(1)
