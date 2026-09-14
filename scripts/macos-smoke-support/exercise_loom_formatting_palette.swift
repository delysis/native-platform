import AppKit
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let actionName = CommandLine.arguments[2]
let linkDestination = CommandLine.arguments[3]
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func strings(_ element: AXUIElement) -> [String] {
    [kAXValueAttribute, kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute]
        .compactMap { attribute(element, $0 as CFString) as? String }
        .filter { !$0.isEmpty }
}

func supportsPress(_ element: AXUIElement) -> Bool {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success,
          let actions = names as? [String] else { return false }
    return actions.contains(kAXPressAction as String)
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

struct CanonicalSelection {
    let raw: CFRange
    let canonical: CFRange
    let valueUtf16: Int
    let terminalLineBreakUtf16: Int
}

// WebKit's AX text area may expose a structural trailing line break for a
// heading/list wrapper even though that separator is absent from the exact
// canonical manuscript value. Project the raw AX range into the canonical
// value's coordinate space; never compare a stripped string to an unstripped
// range.
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
        raw: raw,
        canonical: CFRange(
            location: canonicalLocation,
            length: canonicalEnd - canonicalLocation
        ),
        valueUtf16: canonicalValueUtf16,
        terminalLineBreakUtf16: rawValueUtf16 - canonicalValueUtf16
    )
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

func bool(_ object: [String: Any], _ key: String) -> Bool {
    object[key] as? Bool ?? false
}

func integer(_ object: [String: Any], _ key: String) -> Int? {
    (object[key] as? NSNumber)?.intValue
}

func string(_ object: [String: Any], _ key: String) -> String? {
    object[key] as? String
}

// Compare selection semantics in the editor's canonical document model. Raw
// AX offsets can move when a paragraph becomes a heading or list because
// WebKit exposes structural line breaks that are not manuscript characters.
func sameSemanticSelection(_ before: [String: Any], _ after: [String: Any]) -> Bool {
    guard bool(before, "available"),
          bool(after, "available"),
          integer(before, "epoch") != nil,
          integer(after, "epoch") != nil else { return false }
    if bool(before, "empty") {
        guard bool(after, "empty"),
              integer(before, "caret_byte_offset") != nil,
              integer(after, "caret_byte_offset") != nil else { return false }
        if bool(before, "caret_at_end") { return bool(after, "caret_at_end") }
        return string(before, "selection_kind") == string(after, "selection_kind") &&
            integer(before, "from") == integer(after, "from") &&
            integer(before, "to") == integer(after, "to")
    }
    if bool(before, "all_visible_text") {
        return !bool(after, "empty") && bool(after, "all_visible_text")
    }
    return !bool(after, "empty") &&
        string(before, "selection_kind") == string(after, "selection_kind") &&
        integer(before, "from") == integer(after, "from") &&
        integer(before, "to") == integer(after, "to")
}

func sameAXSelectionSemantics(_ before: CanonicalSelection, _ after: CanonicalSelection) -> Bool {
    if before.canonical.length == 0 && before.canonical.location == before.valueUtf16 {
        return after.canonical.length == 0 && after.canonical.location == after.valueUtf16
    }
    if before.canonical.location == 0 && before.canonical.length == before.valueUtf16 {
        return after.canonical.location == 0 && after.canonical.length == after.valueUtf16
    }
    return before.canonical.location == after.canonical.location &&
        before.canonical.length == after.canonical.length
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

func press(_ element: AXUIElement) -> Bool {
    supportsPress(element) &&
        AXUIElementPerformAction(element, kAXPressAction as CFString) == .success
}

func button(named needle: String) -> AXUIElement? {
    descendants().first { element in
        strings(element).contains(needle) &&
            supportsPress(element) &&
            (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false
    }
}

func waitForButton(_ name: String, timeout: TimeInterval = 5) -> AXUIElement? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if let match = button(named: name) { return match }
        Thread.sleep(forTimeInterval: 0.1)
    } while Date() < deadline
    return nil
}

func textField(named needle: String) -> AXUIElement? {
    descendants().first { element in
        strings(element).contains(needle) &&
            (attribute(element, kAXRoleAttribute as CFString) as? String) == kAXTextFieldRole as String &&
            (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false
    }
}

guard let editor = descendants().first(where: {
    (attribute($0, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String
}),
      let beforeSelection = canonicalSelection(editor),
      let beforeSelectionWitness = editorSelectionWitness(),
      bool(beforeSelectionWitness, "available"),
      integer(beforeSelectionWitness, "epoch") != nil else {
    fputs("could not bind the formatting action to Loom's accessible manuscript selection\n", stderr)
    exit(1)
}

NSRunningApplication(processIdentifier: pid)?.activate(options: [])
// Never infer a closed palette from one lagging AX descendant and accidentally
// toggle an already-open lease closed. Prefer the owner's expanded state and
// accept either stable palette child as corroboration.
guard let format = waitForButton("Format text") else {
    fputs("could not bind Loom's exact formatting palette owner\n", stderr)
    exit(1)
}
let paletteIsOpen = (attribute(format, kAXExpandedAttribute as CFString) as? Bool) == true ||
    button(named: "Title") != nil ||
    textField(named: "Link destination") != nil
if !paletteIsOpen {
    guard press(format) else {
        fputs("could not open Loom's formatting palette through its exact titlebar control\n", stderr)
        exit(1)
    }
    guard waitForButton("Title") != nil else {
        fputs("Loom's formatting palette owner expanded without its stable controls\n", stderr)
        exit(1)
    }
}

if actionName == "Link" {
    let destinationDeadline = Date().addingTimeInterval(5)
    var destination: AXUIElement?
    repeat {
        destination = textField(named: "Link destination")
        if destination != nil { break }
        Thread.sleep(forTimeInterval: 0.1)
    } while Date() < destinationDeadline
    guard let destination,
          AXUIElementSetAttributeValue(
            destination,
            kAXFocusedAttribute as CFString,
            kCFBooleanTrue
          ) == .success,
          let selectAllDown = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
          let selectAllUp = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
        fputs("could not focus Loom's exact Link destination field through Accessibility\n", stderr)
        exit(1)
    }
    selectAllDown.flags = [.maskCommand]
    selectAllUp.flags = [.maskCommand]
    selectAllDown.postToPid(pid)
    selectAllUp.postToPid(pid)
    for character in linkDestination {
        var utf16 = Array(String(character).utf16)
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
            fputs("could not construct Loom's PID-targeted Link destination input\n", stderr)
            exit(1)
        }
        down.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
        down.postToPid(pid)
        up.postToPid(pid)
        Thread.sleep(forTimeInterval: 0.01)
    }
    let valueDeadline = Date().addingTimeInterval(5)
    while Date() < valueDeadline {
        if (attribute(destination, kAXValueAttribute as CFString) as? String) == linkDestination,
           button(named: "Link") != nil { break }
        Thread.sleep(forTimeInterval: 0.05)
    }
    guard (attribute(destination, kAXValueAttribute as CFString) as? String) == linkDestination,
          button(named: "Link") != nil else {
        fputs("Loom did not bind the PID-targeted Link destination or enable its action\n", stderr)
        exit(1)
    }
}

guard let action = waitForButton(actionName), press(action) else {
    fputs("could not invoke \(actionName) from Loom's open formatting palette\n", stderr)
    exit(1)
}

let focusDeadline = Date().addingTimeInterval(5)
var afterSelection: CanonicalSelection?
var afterSelectionWitness: [String: Any]?
var editorFocused = false
var exactSelectionSince: Date?
var exactSelectionEpoch: Int?
repeat {
    editorFocused = (attribute(editor, kAXFocusedAttribute as CFString) as? Bool) == true
    afterSelection = canonicalSelection(editor)
    afterSelectionWitness = editorSelectionWitness()
    if editorFocused,
       let currentAX = afterSelection,
       let current = afterSelectionWitness,
       sameAXSelectionSemantics(beforeSelection, currentAX),
       sameSemanticSelection(beforeSelectionWitness, current),
       let epoch = integer(current, "epoch") {
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
} while Date() < focusDeadline
guard editorFocused,
      let afterSelection,
      let afterSelectionWitness,
      sameAXSelectionSemantics(beforeSelection, afterSelection),
      sameSemanticSelection(beforeSelectionWitness, afterSelectionWitness),
      let exactSelectionSince,
      Date().timeIntervalSince(exactSelectionSince) >= 0.25,
      exactSelectionEpoch == integer(afterSelectionWitness, "epoch") else {
    let afterLocation = afterSelection?.canonical.location ?? -1
    let afterLength = afterSelection?.canonical.length ?? -1
    fputs(
        "Loom's formatting palette did not stably restore the exact manuscript selection " +
        "after \(actionName) (before=\(beforeSelection.canonical.location):" +
        "\(beforeSelection.canonical.length), " +
        "after=\(afterLocation):\(afterLength), focused=\(editorFocused), " +
        "internal_before=\(beforeSelectionWitness), " +
        "internal_after=\(String(describing: afterSelectionWitness)))\n",
        stderr
    )
    exit(1)
}

let evidence: [String: Any] = [
    "control_path": "Format text -> \(actionName)",
    "dispatch": "AXPress on exact accessible controls bound to the target PID",
    "link_destination": linkDestination.isEmpty ? NSNull() : linkDestination,
    "selection_before": [
        "location": beforeSelection.canonical.location,
        "length": beforeSelection.canonical.length,
        "raw_location": beforeSelection.raw.location,
        "raw_length": beforeSelection.raw.length,
        "terminal_line_break_utf16": beforeSelection.terminalLineBreakUtf16
    ],
    "selection_after": [
        "location": afterSelection.canonical.location,
        "length": afterSelection.canonical.length,
        "raw_location": afterSelection.raw.location,
        "raw_length": afterSelection.raw.length,
        "terminal_line_break_utf16": afterSelection.terminalLineBreakUtf16
    ],
    "internal_selection_before": beforeSelectionWitness,
    "internal_selection_after": afterSelectionWitness,
    "editor_refocused": true
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
