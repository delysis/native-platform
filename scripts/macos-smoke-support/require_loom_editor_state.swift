import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let expected = CommandLine.arguments[2]
let selectionMode = CommandLine.arguments[3]
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

func withoutTerminalLineBreaks(_ value: String) -> String {
    var normalized = value
    while normalized.last == "\n" || normalized.last == "\r" { normalized.removeLast() }
    return normalized
}

struct CanonicalSelection {
    let raw: CFRange
    let canonical: CFRange
    let terminalLineBreakUtf16: Int
}

func canonicalSelection(_ element: AXUIElement) -> CanonicalSelection? {
    guard let raw = rangeAttribute(element, kAXSelectedTextRangeAttribute as CFString),
          let rawValue = attribute(element, kAXValueAttribute as CFString) as? String else {
        return nil
    }
    let canonicalValue = withoutTerminalLineBreaks(rawValue)
    let canonicalValueUtf16 = canonicalValue.utf16.count
    let canonicalLocation = min(max(raw.location, 0), canonicalValueUtf16)
    let rawEnd = max(raw.location, 0) + max(raw.length, 0)
    let canonicalEnd = min(max(rawEnd, canonicalLocation), canonicalValueUtf16)
    return CanonicalSelection(
        raw: raw,
        canonical: CFRange(
            location: canonicalLocation,
            length: canonicalEnd - canonicalLocation
        ),
        terminalLineBreakUtf16: rawValue.utf16.count - canonicalValueUtf16
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

func integer(_ object: [String: Any]?, _ key: String) -> Int? {
    (object?[key] as? NSNumber)?.intValue
}

func selectionMatches(
    _ witness: [String: Any]?,
    _ selection: CanonicalSelection?
) -> Bool {
    guard bool(witness, "available"),
          integer(witness, "epoch") != nil,
          let selection else { return false }
    switch selectionMode {
    case "caret-end":
        return bool(witness, "empty") &&
            bool(witness, "caret_at_end") &&
            integer(witness, "caret_byte_offset") != nil &&
            selection.canonical.location == expected.utf16.count &&
            selection.canonical.length == 0
    case "select-all":
        return !bool(witness, "empty") &&
            bool(witness, "all_visible_text") &&
            selection.canonical.location == 0 &&
            selection.canonical.length == expected.utf16.count
    default:
        return true
    }
}

let deadline = Date().addingTimeInterval(12)
var writingSurface: AXUIElement?
var observedValue = ""
var observedSelection: CanonicalSelection?
var observedSelectionWitness: [String: Any]?
var focused = false
var exactSelectionSince: Date?
var exactSelectionEpoch: Int?
repeat {
    writingSurface = editor()
    if let current = writingSurface {
        observedValue = withoutTerminalLineBreaks(
            (attribute(current, kAXValueAttribute as CFString) as? String) ?? ""
        )
        observedSelection = canonicalSelection(current)
        observedSelectionWitness = editorSelectionWitness()
        focused = (attribute(current, kAXFocusedAttribute as CFString) as? Bool) == true
    } else {
        observedValue = ""
        observedSelection = nil
        observedSelectionWitness = nil
        focused = false
    }
    if observedValue == expected,
       focused,
       selectionMatches(observedSelectionWitness, observedSelection),
       let epoch = integer(observedSelectionWitness, "epoch") {
        if exactSelectionEpoch != epoch {
            exactSelectionEpoch = epoch
            exactSelectionSince = Date()
        }
        if let exactSelectionSince,
           Date().timeIntervalSince(exactSelectionSince) >= 0.25 { break }
    } else {
        exactSelectionSince = nil
        exactSelectionEpoch = nil
    }
    Thread.sleep(forTimeInterval: 0.05)
} while Date() < deadline

guard writingSurface != nil,
      observedValue == expected,
      focused,
      selectionMatches(observedSelectionWitness, observedSelection),
      let exactSelectionSince,
      Date().timeIntervalSince(exactSelectionSince) >= 0.25,
      exactSelectionEpoch == integer(observedSelectionWitness, "epoch"),
      let observedSelection,
      let observedSelectionWitness else {
    let location = observedSelection?.canonical.location ?? -1
    let length = observedSelection?.canonical.length ?? -1
    let rawLocation = observedSelection?.raw.location ?? -1
    let rawLength = observedSelection?.raw.length ?? -1
    fputs(
        "Loom's live AX editor diverged from the exact canonical manuscript or lost focus/selection " +
        "(value=\(String(reflecting: observedValue)), expected=\(String(reflecting: expected)), " +
        "focused=\(focused), selection=\(location):\(length), " +
        "raw_selection=\(rawLocation):\(rawLength), mode=\(selectionMode), " +
        "internal_selection=\(String(describing: observedSelectionWitness)))\n",
        stderr
    )
    exit(1)
}
let evidence: [String: Any] = [
    "canonical_editor_value": observedValue,
    "focused": true,
    "selection_mode": selectionMode,
    "selection": [
        "location": observedSelection.canonical.location,
        "length": observedSelection.canonical.length,
        "raw_location": observedSelection.raw.location,
        "raw_length": observedSelection.raw.length,
        "terminal_line_break_utf16": observedSelection.terminalLineBreakUtf16
    ],
    "internal_selection": observedSelectionWitness
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
