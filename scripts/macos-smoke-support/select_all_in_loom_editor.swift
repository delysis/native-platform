import AppKit
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let expected = CommandLine.arguments[2]
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func rangeAttribute(_ element: AXUIElement, _ name: CFString) -> CFRange? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cfRange else { return nil }
    var range = CFRange()
    return AXValueGetValue(value, .cfRange, &range) ? range : nil
}

struct CanonicalSelection {
    let value: String
    let raw: CFRange
    let canonical: CFRange
    let terminalLineBreakUtf16: Int
}

func canonicalSelection(_ element: AXUIElement) -> CanonicalSelection? {
    guard let raw = rangeAttribute(element, kAXSelectedTextRangeAttribute as CFString),
          var value = attribute(element, kAXValueAttribute as CFString) as? String else {
        return nil
    }
    let rawValueUtf16 = value.utf16.count
    while value.last == "\n" || value.last == "\r" { value.removeLast() }
    let canonicalValueUtf16 = value.utf16.count
    let canonicalLocation = min(max(raw.location, 0), canonicalValueUtf16)
    let rawEnd = max(raw.location, 0) + max(raw.length, 0)
    let canonicalEnd = min(max(rawEnd, canonicalLocation), canonicalValueUtf16)
    return CanonicalSelection(
        value: value,
        raw: raw,
        canonical: CFRange(
            location: canonicalLocation,
            length: canonicalEnd - canonicalLocation
        ),
        terminalLineBreakUtf16: rawValueUtf16 - canonicalValueUtf16
    )
}

func descendants() -> [AXUIElement] {
    var queue = [application]
    var cursor = 0
    while cursor < queue.count && cursor < 4096 {
        let element = queue[cursor]
        cursor += 1
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return queue
}

func editor() -> AXUIElement? {
    descendants().first {
        (attribute($0, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String
    }
}

func strings(_ element: AXUIElement) -> [String] {
    [kAXValueAttribute, kAXTitleAttribute, kAXDescriptionAttribute, kAXHelpAttribute]
        .compactMap { attribute(element, $0 as CFString) as? String }
}

func jsonObject(in text: String, schema: String) -> [String: Any]? {
    guard let schemaRange = text.range(of: "\"schema\":\"\(schema)\"") else { return nil }
    let prefix = text[..<schemaRange.lowerBound]
    guard let open = prefix.lastIndex(of: "{"),
          let close = text.lastIndex(of: "}"),
          open <= close,
          let data = String(text[open...close]).data(using: .utf8),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          object["schema"] as? String == schema else { return nil }
    return object
}

func editorSelectionWitness() -> [String: Any]? {
    descendants().flatMap { element in
        strings(element).compactMap { value in
            jsonObject(in: value, schema: "delysis.loom-completion-witness.v1")
        }
    }.compactMap { $0["editor_selection"] as? [String: Any] }.max { left, right in
        ((left["epoch"] as? NSNumber)?.intValue ?? -1) <
            ((right["epoch"] as? NSNumber)?.intValue ?? -1)
    }
}

func bool(_ object: [String: Any]?, _ key: String) -> Bool {
    object?[key] as? Bool ?? false
}

guard let writingSurface = editor() else {
    fputs("could not bind Loom's exact editor for Select-All\n", stderr)
    exit(1)
}
NSRunningApplication(processIdentifier: pid)?.activate(options: [])

let deadline = Date().addingTimeInterval(5)
var observed: CanonicalSelection?
var observedSelectionWitness: [String: Any]?
var focused = false
var exactSelectionSince: Date?
var exactSelectionEpoch: Int?
var nextDispatch = Date.distantPast
var dispatchCount = 0
repeat {
    observed = canonicalSelection(writingSurface)
    observedSelectionWitness = editorSelectionWitness()
    focused = (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true
    if let current = observed,
       current.value == expected,
       current.canonical.location == 0,
       current.canonical.length == expected.utf16.count,
       focused,
       bool(observedSelectionWitness, "available"),
       !bool(observedSelectionWitness, "empty"),
       bool(observedSelectionWitness, "all_visible_text"),
       let epoch = (observedSelectionWitness?["epoch"] as? NSNumber)?.intValue {
        if exactSelectionEpoch != epoch {
            exactSelectionEpoch = epoch
            exactSelectionSince = Date()
        }
        if let exactSelectionSince,
           Date().timeIntervalSince(exactSelectionSince) >= 0.25 { break }
    } else {
        exactSelectionSince = nil
        exactSelectionEpoch = nil
        if Date() >= nextDispatch {
            guard AXUIElementSetAttributeValue(
                writingSurface,
                kAXFocusedAttribute as CFString,
                kCFBooleanTrue
            ) == .success,
                  let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
                  let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
                fputs("could not refocus Loom's exact editor for Select-All\n", stderr)
                exit(1)
            }
            down.flags = [.maskCommand]
            up.flags = [.maskCommand]
            down.postToPid(pid)
            Thread.sleep(forTimeInterval: 0.03)
            up.postToPid(pid)
            dispatchCount += 1
            nextDispatch = Date().addingTimeInterval(0.4)
        }
    }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < deadline
guard let observed,
      observed.value == expected,
      observed.canonical.location == 0,
      observed.canonical.length == expected.utf16.count,
      focused,
      bool(observedSelectionWitness, "available"),
      !bool(observedSelectionWitness, "empty"),
      bool(observedSelectionWitness, "all_visible_text"),
      let exactSelectionSince,
      Date().timeIntervalSince(exactSelectionSince) >= 0.25,
      exactSelectionEpoch == (observedSelectionWitness?["epoch"] as? NSNumber)?.intValue,
      let observedSelectionWitness else {
    let location = observed?.canonical.location ?? -1
    let length = observed?.canonical.length ?? -1
    let rawLocation = observed?.raw.location ?? -1
    let rawLength = observed?.raw.length ?? -1
    fputs(
        "Loom's exact editor did not retain the full internal manuscript selection " +
        "(focused=\(focused), selection=\(location):\(length), " +
        "raw=\(rawLocation):\(rawLength), " +
        "internal_selection=\(String(describing: observedSelectionWitness)))\n",
        stderr
    )
    exit(1)
}
let evidence: [String: Any] = [
    "dispatch": "PID-targeted Command-A",
    "dispatch_count": dispatchCount,
    "selection": [
        "location": observed.canonical.location,
        "length": observed.canonical.length,
        "raw_location": observed.raw.location,
        "raw_length": observed.raw.length,
        "terminal_line_break_utf16": observed.terminalLineBreakUtf16
    ],
    "internal_selection": observedSelectionWitness
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
