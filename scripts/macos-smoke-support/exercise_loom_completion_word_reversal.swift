import Foundation

func isOwnedManuscriptEditor(role: String, labels: [String]) -> Bool {
    role == "AXTextArea" && labels.contains("Untitled, manuscript editor")
}

func exactInsertedCandidatePrefix(original: Data, observed: Data, candidate: Data, insertedBytes: Int, whole: Bool) -> Bool {
    guard insertedBytes > 0, insertedBytes <= candidate.count,
          !whole || insertedBytes == candidate.count else { return false }
    var expected = original
    expected.append(candidate.prefix(insertedBytes))
    return observed == expected
}

// Derive the remaining fan from persisted bytes and independently SHA-verified
// candidates, not from the controller's assertion about its own alternatives.
func remainingCandidateIds(original: Data, observed: Data, runIds: [String], candidates: [String: Data]) -> [String]? {
    guard observed.count > original.count, observed.starts(with: original),
          Set(runIds).count == runIds.count,
          runIds.allSatisfy({ candidates[$0] != nil }) else { return nil }
    let accepted = observed.dropFirst(original.count)
    return runIds.filter { id in
        guard let candidate = candidates[id] else { return false }
        return candidate.count > accepted.count && candidate.starts(with: accepted)
    }
}

func remainingFanMatches(expected: [String], observed: [String], visible: Bool) -> Bool {
    expected == observed && visible == (expected.count > 1)
}

func manuscriptEndCaret(original: Data, expected: Data) -> Int? {
    guard expected.starts(with: original),
          let text = String(data: expected, encoding: .utf8) else { return nil }
    return text.utf16.count
}

func insertionContractTests() {
    let original = Data("hello ".utf8), candidate = Data("world again".utf8)
    var checks = 0
    func check(_ condition: Bool, _ name: String) {
        guard condition else { fputs("insertion contract failed: \(name)\n", stderr); exit(1) }
        checks += 1
    }
    func accepts(_ text: String, _ length: Int, _ whole: Bool = false) -> Bool {
        exactInsertedCandidatePrefix(original: original, observed: Data(text.utf8), candidate: candidate, insertedBytes: length, whole: whole)
    }
    check(accepts("hello world", 5), "exact selected prefix")
    check(!accepts("hello wrong", 5), "same-size substituted bytes")
    check(!accepts("Hello world", 5), "changed original bytes")
    check(!accepts("hello world", 5, true), "partial is not whole remainder")
    check(accepts("hello world again", 11, true), "exact whole remainder")
    check(!accepts("hello ", 0), "empty acceptance")
    check(!accepts("hello world", -1), "negative length")
    check(!accepts("hello world again!", 12), "beyond candidate")
    check(!exactInsertedCandidatePrefix(original: Data(), observed: Data("é".utf8), candidate: Data("e\u{301}".utf8), insertedBytes: 3, whole: true), "Unicode byte identity")
    check(isOwnedManuscriptEditor(role: "AXTextArea", labels: ["Untitled, manuscript editor"]), "owned editor label")
    check(!isOwnedManuscriptEditor(role: "AXTextArea", labels: ["Manuscript editor"]), "reject unscoped legacy editor")
    check(!isOwnedManuscriptEditor(role: "AXTextArea", labels: ["Other, manuscript editor"]), "reject different manuscript")
    check(!isOwnedManuscriptEditor(role: "AXButton", labels: ["Untitled, manuscript editor"]), "reject wrong editor role")
    let ids = ["a", "b", "c", "d"]
    let bodies = ["a": Data("one two".utf8), "b": Data("one three".utf8),
                  "c": Data("one four".utf8), "d": Data("another turn".utf8)]
    let remaining = remainingCandidateIds(original: original, observed: Data("hello one".utf8), runIds: ids, candidates: bodies)
    check(remaining == ["a", "b", "c"], "preserve compatible original order")
    check(remainingFanMatches(expected: remaining!, observed: ["a", "b", "c"], visible: true), "held Option retains compatible fan")
    check(!remainingFanMatches(expected: remaining!, observed: ["a", "b", "c"], visible: false), "reject old hidden-fan decision")
    check(!remainingFanMatches(expected: remaining!, observed: ids, visible: true), "reject incompatible sibling")
    check(!remainingFanMatches(expected: remaining!, observed: ["b", "a", "c"], visible: true), "reject reordered mapping")
    check(remainingFanMatches(expected: ["d"], observed: ["d"], visible: false), "singleton has no fan")
    check(!remainingFanMatches(expected: ["d"], observed: ["d"], visible: true), "reject singleton fan")
    check(remainingCandidateIds(original: original, observed: Data("Hello one".utf8), runIds: ids, candidates: bodies) == nil, "reject changed manuscript prefix")
    check(remainingCandidateIds(original: original, observed: Data("hello one".utf8), runIds: ["missing"], candidates: bodies) == nil, "reject unverified candidate")
    check(remainingCandidateIds(original: original, observed: Data("hello one two".utf8), runIds: ids, candidates: bodies) == [], "exhausted candidates are not alternatives")
    check(manuscriptEndCaret(original: original, expected: original) == 6, "initial manuscript end")
    check(manuscriptEndCaret(original: original, expected: Data("hello one".utf8)) == 9, "accepted word moves the expected caret")
    check(manuscriptEndCaret(original: original, expected: Data("hello 🜁".utf8)) == 8, "caret uses UTF-16 rather than bytes")
    check(manuscriptEndCaret(original: original, expected: Data("Hello one".utf8)) == nil, "reject changed original at refocus")
    check(manuscriptEndCaret(original: Data(), expected: Data([0xff])) == nil, "reject invalid manuscript encoding")
    print("\(checks) insertion-contract assertions passed (not native acceptance)")
}

if CommandLine.arguments.count == 2 && CommandLine.arguments[1] == "--self-test" {
    insertionContractTests(); exit(0)
}

#if os(macOS)
import AppKit
import ApplicationServices
import CryptoKit
import Foundation

guard CommandLine.arguments.count == 6, let pid = Int32(CommandLine.arguments[1]), pid > 0 else {
    fputs("usage: exercise_loom_completion_word_reversal <pid> <manuscript> <prefix> <generation-failure> <project-failure>\n", stderr)
    exit(2)
}
let manuscript = CommandLine.arguments[2]
let prefix = CommandLine.arguments[3]
let asynchronousFailurePaths = [CommandLine.arguments[4], CommandLine.arguments[5]]
    .filter { !$0.isEmpty }
let application = AXUIElementCreateApplication(pid)

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
    return value
}

func stringAttribute(_ element: AXUIElement, _ name: CFString) -> String {
    attribute(element, name) as? String ?? ""
}

func pointAttribute(_ element: AXUIElement, _ name: CFString) -> CGPoint? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cgPoint else { return nil }
    var point = CGPoint.zero
    return AXValueGetValue(value, .cgPoint, &point) ? point : nil
}

func sizeAttribute(_ element: AXUIElement, _ name: CFString) -> CGSize? {
    guard let raw = attribute(element, name), CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cgSize else { return nil }
    var size = CGSize.zero
    return AXValueGetValue(value, .cgSize, &size) ? size : nil
}

func selectedRange(_ element: AXUIElement) -> CFRange? {
    guard let raw = attribute(element, kAXSelectedTextRangeAttribute as CFString),
          CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
    let value = raw as! AXValue
    guard AXValueGetType(value) == .cfRange else { return nil }
    var range = CFRange()
    return AXValueGetValue(value, .cfRange, &range) ? range : nil
}

func setCollapsedEndSelection(_ element: AXUIElement, caret: Int) -> Bool {
    var range = CFRange(location: caret, length: 0)
    guard let value = AXValueCreate(.cfRange, &range) else { return false }
    return AXUIElementSetAttributeValue(
        element,
        kAXSelectedTextRangeAttribute as CFString,
        value
    ) == .success
}

func clickCenter(_ element: AXUIElement) -> Bool {
    guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else { return false }
    guard let origin = pointAttribute(element, kAXPositionAttribute as CFString),
          let size = sizeAttribute(element, kAXSizeAttribute as CFString),
          size.width >= 100, size.height >= 40 else { return false }
    let point = CGPoint(x: origin.x + size.width / 2, y: origin.y + size.height / 2)
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown,
                             mouseCursorPosition: point, mouseButton: .left),
          let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp,
                           mouseCursorPosition: point, mouseButton: .left) else { return false }
    down.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.03)
    up.post(tap: .cghidEventTap)
    return true
}

let stringAttributes = [
    kAXValueAttribute,
    kAXTitleAttribute,
    kAXDescriptionAttribute,
    kAXHelpAttribute
].map { $0 as CFString }

func strings(_ element: AXUIElement) -> [String] {
    stringAttributes.compactMap { attribute(element, $0) as? String }
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
    descendants().first { element in
        isOwnedManuscriptEditor(
            role: stringAttribute(element, kAXRoleAttribute as CFString),
            labels: strings(element)
        )
    }
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

func completionWitness() -> [String: Any]? {
    for element in descendants() {
        for value in strings(element) {
            if let object = jsonObject(
                in: value,
                schema: "delysis.loom-completion-witness.v1"
            ) { return object }
        }
    }
    return nil
}

// These are production shortcuts, not retired titlebar labels. The witness
// below must prove the resulting transition; dispatch alone never passes.
func completionShortcut(_ key: CGKeyCode, field: String, from expected: Bool) -> Bool {
    guard !asynchronousGuardFailed(),
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
          let witness = completionWitness(), witness[field] as? Bool == expected else { return false }
    return postKey(key, down: true, flags: [.maskCommand, .maskShift]) &&
        postKey(key, down: false, flags: [.maskCommand, .maskShift])
}

func visual(_ witness: [String: Any]) -> [String: Any] {
    witness["visual"] as? [String: Any] ?? [:]
}

func bool(_ object: [String: Any], _ key: String) -> Bool {
    object[key] as? Bool ?? false
}

func integer(_ object: [String: Any], _ key: String) -> Int {
    (object[key] as? NSNumber)?.intValue ?? -1
}

func string(_ object: [String: Any], _ key: String) -> String {
    object[key] as? String ?? ""
}

func stringArray(_ object: [String: Any], _ key: String) -> [String] {
    object[key] as? [String] ?? []
}

func familyRunIds(_ witness: [String: Any]) -> [String] {
    (witness["candidates"] as? [[String: Any]] ?? []).map { string($0, "run_id") }
}

func lastAction(_ witness: [String: Any]) -> [String: Any] {
    witness["last_action"] as? [String: Any] ?? [:]
}

func sameFamily(
    _ witness: [String: Any],
    context: String,
    runIds: [String]
) -> Bool {
    string(witness, "context_key") == context &&
        familyRunIds(witness) == runIds &&
        integer(witness, "family_count") == 4
}

func actionNames(_ element: AXUIElement) -> [String] {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success else { return [] }
    return names as? [String] ?? []
}

func selectedState(_ element: AXUIElement) -> Bool {
    let value = attribute(element, kAXSelectedAttribute as CFString)
    if let selected = value as? Bool { return selected }
    return (value as? NSNumber)?.boolValue ?? false
}

let suggestionLabelPattern = try! NSRegularExpression(
    pattern: #"^Suggestion ([1-9][0-9]*) of ([1-9][0-9]*): (.+)$"#
)

func suggestionOrdinal(_ label: String) -> (index: Int, count: Int)? {
    let range = NSRange(label.startIndex..<label.endIndex, in: label)
    guard let match = suggestionLabelPattern.firstMatch(in: label, range: range),
          let indexRange = Range(match.range(at: 1), in: label),
          let countRange = Range(match.range(at: 2), in: label),
          let index = Int(label[indexRange]),
          let count = Int(label[countRange]) else { return nil }
    return (index, count)
}

func subtree(_ root: AXUIElement) -> [AXUIElement] {
    var queue = [root]
    var cursor = 0
    while cursor < queue.count && cursor < 256 {
        let element = queue[cursor]
        cursor += 1
        if let children = attribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] {
            queue.append(contentsOf: children)
        }
    }
    return queue
}

func accessibilityObservation(_ element: AXUIElement) -> [String: Any] {
    [
        "role": stringAttribute(element, kAXRoleAttribute as CFString),
        "subrole": stringAttribute(element, kAXSubroleAttribute as CFString),
        "strings": strings(element),
        "selected": selectedState(element),
        "actions": actionNames(element)
    ]
}

func fanAccessibility() -> (
    listbox: Bool,
    options: [[String: Any]],
    observations: [[String: Any]]
) {
    let listboxes = descendants().filter { element in
        stringAttribute(element, kAXRoleAttribute as CFString) == kAXListRole as String &&
            strings(element).contains("Completion suggestions")
    }
    var byIndex: [Int: [String: Any]] = [:]
    var observations: [[String: Any]] = []
    for listbox in listboxes {
        observations.append(accessibilityObservation(listbox))
        for element in subtree(listbox) {
            for label in strings(element) {
                guard let ordinal = suggestionOrdinal(label), (2...4).contains(ordinal.count),
                      (1...ordinal.count).contains(ordinal.index) else { continue }
                let option: [String: Any] = [
                    "index": ordinal.index,
                    "count": ordinal.count,
                    "label": label,
                    "ax_selected": selectedState(element)
                ]
                if byIndex[ordinal.index] == nil || selectedState(element) {
                    byIndex[ordinal.index] = option
                }
                observations.append(accessibilityObservation(element))
            }
        }
    }
    return (!listboxes.isEmpty, Array(byIndex.values), observations)
}

func exactAccessibleFan(_ witness: [String: Any]) -> [[String: Any]]? {
    let fan = fanAccessibility()
    let candidates = witness["candidates"] as? [[String: Any]] ?? []
    let alternatives = stringArray(visual(witness), "alternativeRunIds")
    guard fan.listbox,
          (2...4).contains(alternatives.count),
          Set(alternatives).count == alternatives.count,
          fan.options.count == alternatives.count,
          candidates.count == 4 else {
        return nil
    }
    var enriched: [[String: Any]] = []
    for option in fan.options.sorted(by: { integer($0, "index") < integer($1, "index") }) {
        let index = integer(option, "index")
        guard index == enriched.count + 1,
              integer(option, "count") == alternatives.count,
              let candidate = candidates.first(where: { string($0, "run_id") == alternatives[index - 1] }) else { return nil }
        var joined = option
        joined["run_id"] = string(candidate, "run_id")
        joined["candidate_id"] = string(candidate, "candidate_id")
        joined["presentation_key"] = string(candidate, "presentation_key")
        enriched.append(joined)
    }
    guard enriched.filter({ bool($0, "ax_selected") }).count == 1,
          let selected = enriched.first(where: { bool($0, "ax_selected") }),
          string(selected, "run_id") == string(witness, "selected_run_id") else {
        return nil
    }
    return enriched
}

func waitForAccessibleFan(
    timeout: TimeInterval,
    _ predicate: ([String: Any]) -> Bool
) -> (witness: [String: Any], options: [[String: Any]])? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return nil }
        if let witness = completionWitness(), predicate(witness),
           let options = exactAccessibleFan(witness) {
            return (witness, options)
        }
        // WebKit publishes the application witness and the rebuilt ARIA
        // subtree on separate accessibility turns. Require both exact views
        // to converge instead of sampling the option rows once immediately
        // after the parent witness changes.
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return nil
}

@discardableResult
func postKey(_ key: CGKeyCode, down: Bool, flags: CGEventFlags) -> Bool {
    guard let event = CGEvent(keyboardEventSource: nil, virtualKey: key, keyDown: down) else {
        return false
    }
    event.flags = flags
    event.postToPid(pid)
    return true
}

func readManuscript() -> Data? {
    try? Data(contentsOf: URL(fileURLWithPath: manuscript), options: [.uncached])
}

func sha256(_ data: Data) -> String {
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}

func asynchronousGuardFailed() -> Bool {
    asynchronousFailurePaths.contains { FileManager.default.fileExists(atPath: $0) }
}

func waitForWitness(
    timeout: TimeInterval,
    _ predicate: ([String: Any]) -> Bool
) -> [String: Any]? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return nil }
        if let witness = completionWitness(), predicate(witness) { return witness }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return nil
}

func waitForChangedManuscript(from original: Data, timeout: TimeInterval) -> Data? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return nil }
        if let current = readManuscript(), current != original,
           let text = String(data: current, encoding: .utf8), text.hasPrefix(prefix) {
            let suffix = text.dropFirst(prefix.count)
            if suffix.rangeOfCharacter(from: .whitespacesAndNewlines.inverted) != nil {
                return current
            }
        }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return nil
}

func waitForExactManuscript(_ expected: Data, timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() { return false }
        if readManuscript() == expected { return true }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return false
}

func focusWritingSurface(expected: Data, timeout: TimeInterval) -> AXUIElement? {
    guard let caret = manuscriptEndCaret(original: Data(prefix.utf8), expected: expected),
          let runningApplication = NSRunningApplication(processIdentifier: pid) else { return nil }
    let deadline = ProcessInfo.processInfo.systemUptime + timeout
    repeat {
        if asynchronousGuardFailed() || runningApplication.isTerminated || readManuscript() != expected { return nil }
        runningApplication.unhide()
        _ = runningApplication.activate(options: [.activateAllWindows])
        _ = AXUIElementSetAttributeValue(
            application,
            kAXFrontmostAttribute as CFString,
            kCFBooleanTrue
        )
        if let writingSurface = editor() {
            _ = AXUIElementSetAttributeValue(
                application,
                kAXFocusedUIElementAttribute as CFString,
                writingSurface
            )
            _ = AXUIElementSetAttributeValue(
               writingSurface,
               kAXFocusedAttribute as CFString,
               kCFBooleanTrue
            )
            if (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) != true {
                _ = clickCenter(writingSurface)
            }
            _ = setCollapsedEndSelection(writingSurface, caret: caret)
            _ = AXUIElementSetAttributeValue(
                writingSurface,
                kAXFocusedAttribute as CFString,
                kCFBooleanTrue
            )
            if NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
               (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true,
               let range = selectedRange(writingSurface),
               range.location == caret, range.length == 0 {
                return writingSurface
            }
        }
        Thread.sleep(forTimeInterval: 0.05)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return nil
}

guard let writingSurface = focusWritingSurface(expected: Data(prefix.utf8), timeout: 5) else {
    fputs("could not focus Loom's exact writing surface for completion reversal\n", stderr)
    exit(1)
}
guard let original = readManuscript(), original == Data(prefix.utf8), prefix.last?.isWhitespace == true else {
    fputs("could not read Loom's isolated manuscript before completion reversal\n", stderr)
    exit(1)
}
Thread.sleep(forTimeInterval: 0.1)

guard let initial = waitForWitness(timeout: 30, { witness in
    let rendered = visual(witness)
    return bool(witness, "session_cached") &&
        integer(witness, "family_count") == 4 &&
        integer(witness, "accepted_chunk_count") == 0 &&
        bool(witness, "autocomplete_enabled") &&
        !bool(witness, "shuttle_enabled") &&
        !string(witness, "inline_visible_key").isEmpty &&
        bool(rendered, "available") &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("Loom did not expose one exact cached four-choice completion witness\n", stderr)
    exit(1)
}
// The isolated journey uses root-level writing/Untitled.md. Its baseline
// ends in whitespace, so presentation never adds an editor-owned separator.
// Bind expected inserted bytes to the SHA-verified immutable candidate, not
// to a length reported by the same controller that performed the insertion.
func verifiedCandidateBytes(_ candidate: [String: Any]) -> Data? {
    let parts = string(candidate, "presentation_key").components(separatedBy: ":")
    guard parts.count == 2 || parts.count == 4, parts[0] != "stream", !parts[0].isEmpty,
          parts[1].count == 64, parts[1].allSatisfy({ "0123456789abcdef".contains($0) }) else { return nil }
    let hash = parts[1], length = integer(candidate, "text_utf8_bytes")
    if parts.count == 4 && (parts[2] != "prose-prefix" || Int(parts[3]) != length) { return nil }
    let path = URL(fileURLWithPath: manuscript).deletingLastPathComponent()
        .appendingPathComponent(".loom/blobs/sha256").appendingPathComponent(String(hash.prefix(2)))
        .appendingPathComponent(String(hash.dropFirst(2)))
    guard let file = try? FileHandle(forReadingFrom: path) else { return nil }
    defer { try? file.close() }
    guard let bytes = try? file.read(upToCount: 256 * 1024 + 1), bytes.count <= 256 * 1024,
          sha256(bytes) == hash, length > 0, length <= bytes.count else { return nil }
    let projected = Data(bytes.prefix(length))
    guard String(data: projected, encoding: .utf8) != nil else { return nil }
    return projected
}
var expectedCandidates: [String: Data] = [:]
for candidate in initial["candidates"] as? [[String: Any]] ?? [] {
    let run = string(candidate, "run_id")
    guard !run.isEmpty, expectedCandidates[run] == nil, let bytes = verifiedCandidateBytes(candidate) else {
        fputs("could not bind expected insertion to one immutable candidate blob\n", stderr); exit(1)
    }
    expectedCandidates[run] = bytes
}
guard expectedCandidates.count == 4 else { fputs("four exact candidate blobs are required\n", stderr); exit(1) }
func exactInsertion(_ observed: Data, witness: [String: Any], whole: Bool = false) -> Bool {
    let action = lastAction(witness)
    guard let candidate = expectedCandidates[string(action, "run_id")] else { return false }
    return exactInsertedCandidatePrefix(original: original, observed: observed, candidate: candidate,
        insertedBytes: integer(action, "inserted_utf8_bytes"), whole: whole)
}

let context = string(initial, "context_key")
let runIds = familyRunIds(initial)
let initialRunId = string(initial, "selected_run_id")
let initialActionSequence = integer(lastAction(initial), "sequence")
guard !context.isEmpty,
      Set(runIds).count == 4,
      !initialRunId.isEmpty else {
    fputs("Loom's initial completion witness lacked exact family identity\n", stderr)
    exit(1)
}

// Keep the physical Option state down across both arrows. The test observes
// persisted manuscript bytes after each event instead of trusting dispatch.
guard postKey(58, down: true, flags: [.maskAlternate]) else {
    fputs("could not construct Loom's Option modifier event\n", stderr)
    exit(1)
}
defer { postKey(58, down: false, flags: []) }

func releaseOptionAndFail(_ message: String) -> Never {
    let fan = fanAccessibility()
    let diagnostic: [String: Any] = [
        "completion_witness": completionWitness() ?? [:],
        "fan_listbox_observed": fan.listbox,
        "fan_options": fan.options,
        "fan_observations": fan.observations
    ]
    if JSONSerialization.isValidJSONObject(diagnostic),
       let data = try? JSONSerialization.data(withJSONObject: diagnostic, options: [.sortedKeys]),
       let json = String(data: data, encoding: .utf8) {
        fputs("completion fan diagnostics: \(json)\n", stderr)
    }
    postKey(58, down: false, flags: [])
    fputs("\(message)\n", stderr)
    exit(1)
}

guard let fanOpenedEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible") &&
        stringArray(rendered, "alternativeRunIds") == runIds
}) else {
    releaseOptionAndFail("physical Option-down did not expose one accessible four-choice fan")
}
let fanOpened = fanOpenedEvidence.witness
let fanOptions = fanOpenedEvidence.options

guard postKey(125, down: true, flags: [.maskAlternate]),
      postKey(125, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Down events")
}
guard let cycledDownEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") != initialRunId &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible")
}) else {
    releaseOptionAndFail("Option-Down did not select a different run while the four-choice fan stayed visible")
}
let cycledDown = cycledDownEvidence.witness
let cycledRunId = string(cycledDown, "selected_run_id")

guard postKey(126, down: true, flags: [.maskAlternate]),
      postKey(126, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Up events")
}
guard let cycledUpEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") == initialRunId &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible")
}) else {
    releaseOptionAndFail("Option-Up did not restore the original run while the four-choice fan stayed visible")
}
let cycledUp = cycledUpEvidence.witness

guard postKey(124, down: true, flags: [.maskAlternate]),
      postKey(124, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Right events")
}
guard let accepted = waitForChangedManuscript(from: original, timeout: 30) else {
    releaseOptionAndFail("Option-Right did not persist one cached completion word")
}
guard let expectedRemainingIds = remainingCandidateIds(
    original: original, observed: accepted, runIds: runIds, candidates: expectedCandidates
) else {
    releaseOptionAndFail("accepted manuscript could not be bound to the verified candidate family")
}
guard let wordAccepted = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    let action = lastAction(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") == initialRunId &&
        integer(witness, "accepted_chunk_count") == 1 &&
        bool(witness, "authority_frozen") &&
        bool(rendered, "optionHeld") &&
        remainingFanMatches(expected: expectedRemainingIds,
            observed: stringArray(rendered, "alternativeRunIds"), visible: bool(rendered, "fanVisible")) &&
        (expectedRemainingIds.count < 2 || exactAccessibleFan(witness) != nil) &&
        string(action, "kind") == "option_word" &&
        string(action, "run_id") == initialRunId &&
        integer(action, "sequence") > initialActionSequence
}) else {
    releaseOptionAndFail("Option-Right did not retain physical Option and exact cached-session authority")
}

guard exactInsertion(accepted, witness: wordAccepted),
      postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not construct Loom's Option-Left events")
}
guard waitForExactManuscript(original, timeout: 30) else {
    releaseOptionAndFail("Option-Left did not restore the exact pre-acceptance manuscript bytes")
}

guard let rolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") == initialRunId &&
        integer(witness, "accepted_chunk_count") == 0 &&
        bool(witness, "authority_frozen") &&
        bool(rendered, "optionHeld") &&
        bool(rendered, "fanVisible") &&
        stringArray(rendered, "alternativeRunIds") == runIds
}) else {
    releaseOptionAndFail("Option-Left did not restore the same cached four-choice fan while Option remained held")
}
let rolledBack = rolledBackEvidence.witness

postKey(58, down: false, flags: [])
guard let optionReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("physical Option-up did not close the completion fan\n", stderr)
    exit(1)
}

guard completionShortcut(38, field: "shuttle_enabled", from: false) else {
    fputs("could not enable Shuttle on the cached completion session\n", stderr)
    exit(1)
}
guard let shuttleEnabled = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        bool(witness, "autocomplete_enabled") &&
        bool(witness, "shuttle_enabled") &&
        bool(witness, "inline_hidden_requested") &&
        string(witness, "inline_visible_key").isEmpty &&
        integer(witness, "accepted_chunk_count") == 0 &&
        bool(rendered, "inlineHidden") &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("Shuttle did not hide the inline presentation while retaining the exact cached family\n", stderr)
    exit(1)
}
guard let shuttleAcceptedBytes = waitForChangedManuscript(from: original, timeout: 30),
      let shuttleAccepted = waitForWitness(timeout: 10, { witness in
          let action = lastAction(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              bool(witness, "autocomplete_enabled") &&
              bool(witness, "shuttle_enabled") &&
              bool(witness, "inline_hidden_requested") &&
              string(witness, "inline_visible_key").isEmpty &&
              integer(witness, "accepted_chunk_count") == 1 &&
              integer(witness, "accepted_utf8_bytes") > 0 &&
              bool(witness, "authority_frozen") &&
              string(action, "kind") == "shuttle_word" &&
              string(action, "run_id") == initialRunId &&
              integer(action, "accepted_utf8_bytes") == integer(witness, "accepted_utf8_bytes") &&
              integer(action, "sequence") > integer(lastAction(wordAccepted), "sequence")
      }) else {
    fputs("Shuttle did not consume exactly one word from the same hidden cached family\n", stderr)
    exit(1)
}
guard exactInsertion(shuttleAcceptedBytes, witness: shuttleAccepted) else {
    fputs("Shuttle's persisted byte delta did not equal its authorized cached word\n", stderr)
    exit(1)
}

guard completionShortcut(38, field: "shuttle_enabled", from: true) else {
    fputs("could not stop Shuttle after its first cached word\n", stderr)
    exit(1)
}
guard let shuttleDisabled = waitForWitness(timeout: 10, { witness in
    return sameFamily(witness, context: context, runIds: runIds) &&
        bool(witness, "autocomplete_enabled") &&
        !bool(witness, "shuttle_enabled") &&
        !bool(witness, "inline_hidden_requested") &&
        integer(witness, "accepted_chunk_count") == 1 &&
        string(lastAction(witness), "kind") == "shuttle_word"
}) else {
    fputs("Shuttle-off did not preserve its exact one-word cached session\n", stderr)
    exit(1)
}

guard focusWritingSurface(expected: shuttleAcceptedBytes, timeout: 5) != nil,
      postKey(58, down: true, flags: [.maskAlternate]),
      postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]) else {
    releaseOptionAndFail("could not dispatch Shuttle's exact Option-Left rollback")
}
guard waitForExactManuscript(original, timeout: 30),
      let shuttleRolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              integer(witness, "accepted_chunk_count") == 0 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") &&
              bool(rendered, "fanVisible") &&
              stringArray(rendered, "alternativeRunIds") == runIds
      }) else {
    releaseOptionAndFail("Option-Left did not exactly reverse Shuttle's cached word and restore its fan")
}
let shuttleRolledBack = shuttleRolledBackEvidence.witness
postKey(58, down: false, flags: [])
guard let shuttleRollbackReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible")
}) else {
    fputs("Option-up did not settle after Shuttle rollback\n", stderr)
    exit(1)
}

// Prove the documented fan Return action against a deliberately non-default
// run, then use the exhausted session's rollback-only plan immediately.
guard postKey(58, down: true, flags: [.maskAlternate]),
      waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) != nil,
      postKey(125, down: true, flags: [.maskAlternate]),
      postKey(125, down: false, flags: [.maskAlternate]),
      let returnSelectedEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") != initialRunId &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("could not select a non-default cached run for fan Return")
}
let returnSelected = returnSelectedEvidence.witness
let returnRunId = string(returnSelected, "selected_run_id")
let returnPreviousSequence = integer(lastAction(returnSelected), "sequence")
guard postKey(36, down: true, flags: [.maskAlternate]),
      postKey(36, down: false, flags: [.maskAlternate]),
      let returnAcceptedBytes = waitForChangedManuscript(from: original, timeout: 30),
      let returnAccepted = waitForWitness(timeout: 10, { witness in
          let rendered = visual(witness)
          let action = lastAction(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == returnRunId &&
              integer(witness, "accepted_chunk_count") == 1 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") &&
              !bool(rendered, "fanVisible") &&
              string(action, "kind") == "fan_return" &&
              string(action, "run_id") == returnRunId &&
              integer(action, "sequence") > returnPreviousSequence
      }) else {
    releaseOptionAndFail("fan Return did not persist the selected cached remainder")
}
let returnAction = lastAction(returnAccepted)
guard exactInsertion(returnAcceptedBytes, witness: returnAccepted, whole: true),
      integer(returnAction, "accepted_utf8_bytes") == integer(returnAction, "inserted_utf8_bytes"),
      (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true,
      postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]),
      waitForExactManuscript(original, timeout: 30),
      let returnRolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == returnRunId &&
              integer(witness, "accepted_chunk_count") == 0 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("fan Return was not exact, focused, or immediately reversible")
}
let returnRolledBack = returnRolledBackEvidence.witness
postKey(58, down: false, flags: [])
guard let returnReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") && !bool(rendered, "fanVisible")
}) else {
    fputs("Option-up did not settle after fan Return rollback\n", stderr)
    exit(1)
}

// Repeat with fan Tab. A literal-tab fallback cannot satisfy the action kind,
// selected-run identity, or exact authorized byte delta below.
guard postKey(58, down: true, flags: [.maskAlternate]),
      waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) != nil,
      postKey(125, down: true, flags: [.maskAlternate]),
      postKey(125, down: false, flags: [.maskAlternate]),
      let tabSelectedEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") != returnRunId &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("could not select another cached run for fan Tab")
}
let tabSelected = tabSelectedEvidence.witness
let tabRunId = string(tabSelected, "selected_run_id")
let tabPreviousSequence = integer(lastAction(tabSelected), "sequence")
guard postKey(48, down: true, flags: [.maskAlternate]),
      postKey(48, down: false, flags: [.maskAlternate]),
      let tabAcceptedBytes = waitForChangedManuscript(from: original, timeout: 30),
      let tabAccepted = waitForWitness(timeout: 10, { witness in
          let rendered = visual(witness)
          let action = lastAction(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == tabRunId &&
              integer(witness, "accepted_chunk_count") == 1 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") &&
              !bool(rendered, "fanVisible") &&
              string(action, "kind") == "fan_tab" &&
              string(action, "run_id") == tabRunId &&
              integer(action, "sequence") > tabPreviousSequence
      }) else {
    releaseOptionAndFail("fan Tab did not persist the selected cached remainder")
}
let tabAction = lastAction(tabAccepted)
guard exactInsertion(tabAcceptedBytes, witness: tabAccepted, whole: true),
      integer(tabAction, "accepted_utf8_bytes") == integer(tabAction, "inserted_utf8_bytes"),
      (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true,
      postKey(123, down: true, flags: [.maskAlternate]),
      postKey(123, down: false, flags: [.maskAlternate]),
      waitForExactManuscript(original, timeout: 30),
      let tabRolledBackEvidence = waitForAccessibleFan(timeout: 10, { witness in
          let rendered = visual(witness)
          return sameFamily(witness, context: context, runIds: runIds) &&
              string(witness, "selected_run_id") == tabRunId &&
              integer(witness, "accepted_chunk_count") == 0 &&
              bool(witness, "authority_frozen") &&
              bool(rendered, "optionHeld") && bool(rendered, "fanVisible")
      }) else {
    releaseOptionAndFail("fan Tab was not exact, focused, or immediately reversible")
}
let tabRolledBack = tabRolledBackEvidence.witness
postKey(58, down: false, flags: [])
guard let tabReleased = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        !bool(rendered, "optionHeld") && !bool(rendered, "fanVisible")
}) else {
    fputs("Option-up did not settle after fan Tab rollback\n", stderr)
    exit(1)
}

guard completionShortcut(5, field: "autocomplete_enabled", from: true) else {
    fputs("could not turn the shared completion engine off after cached checks\n", stderr)
    exit(1)
}
guard let engineDisabled = waitForWitness(timeout: 10, { witness in
    return !bool(witness, "autocomplete_enabled") &&
        !bool(witness, "shuttle_enabled") &&
        !bool(witness, "session_cached") &&
        integer(witness, "family_count") == 0 &&
        string(lastAction(witness), "kind") == "fan_tab"
}) else {
    fputs("shared engine on-to-off did not clear the cached completion session\n", stderr)
    exit(1)
}

let evidence: [String: Any] = [
    "dispatch": "Option held across native Right and Left arrow events",
    "original_bytes": original.count,
    "accepted_bytes": accepted.count,
    "original_sha256": sha256(original),
    "accepted_sha256": sha256(accepted),
    "rollback_sha256": sha256(readManuscript()!),
    "accepted_then_exactly_reversed": true,
    "insertions_match_immutable_candidate_bytes": true,
    "context_key": context,
    "family_run_ids": runIds,
    "initial_selected_run_id": initialRunId,
    "cycled_down_run_id": cycledRunId,
    "accessible_fan_options": fanOptions,
    "initial_witness": initial,
    "fan_opened_witness": fanOpened,
    "cycled_down_witness": cycledDown,
    "cycled_up_witness": cycledUp,
    "word_accepted_witness": wordAccepted,
    "rolled_back_witness": rolledBack,
    "option_released_witness": optionReleased,
    "shuttle_enabled_witness": shuttleEnabled,
    "shuttle_accepted_witness": shuttleAccepted,
    "shuttle_accepted_sha256": sha256(shuttleAcceptedBytes),
    "shuttle_disabled_witness": shuttleDisabled,
    "shuttle_rolled_back_witness": shuttleRolledBack,
    "shuttle_rollback_released_witness": shuttleRollbackReleased,
    "fan_return_selected_witness": returnSelected,
    "fan_return_accepted_witness": returnAccepted,
    "fan_return_accepted_sha256": sha256(returnAcceptedBytes),
    "fan_return_rolled_back_witness": returnRolledBack,
    "fan_return_released_witness": returnReleased,
    "fan_tab_selected_witness": tabSelected,
    "fan_tab_accepted_witness": tabAccepted,
    "fan_tab_accepted_sha256": sha256(tabAcceptedBytes),
    "fan_tab_rolled_back_witness": tabRolledBack,
    "fan_tab_released_witness": tabReleased,
    "engine_disabled_witness": engineDisabled
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)

#else
fputs("native completion interaction requires macOS; only --self-test is portable\n", stderr)
exit(2)
#endif
