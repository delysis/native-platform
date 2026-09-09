import AppKit
import ApplicationServices
import CryptoKit
import Foundation

let pid = Int32(CommandLine.arguments[1])!
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
        stringAttribute(element, kAXRoleAttribute as CFString) == kAXTextAreaRole as String
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

func supportsPress(_ element: AXUIElement) -> Bool {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success,
          let actions = names as? [String] else { return false }
    return actions.contains(kAXPressAction as String)
}

func button(named name: String) -> AXUIElement? {
    descendants().first { element in
        strings(element).contains(where: { $0.contains(name) }) &&
            supportsPress(element) &&
            (attribute(element, kAXEnabledAttribute as CFString) as? Bool) != false
    }
}

func pressButton(named name: String, timeout: TimeInterval = 10) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if let control = button(named: name),
           AXUIElementPerformAction(control, kAXPressAction as CFString) == .success {
            return true
        }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return false
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
                guard let ordinal = suggestionOrdinal(label), ordinal.count == 4 else { continue }
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
    guard fan.listbox,
          fan.options.count == 4,
          candidates.count == 4 else {
        return nil
    }
    var enriched: [[String: Any]] = []
    for option in fan.options.sorted(by: { integer($0, "index") < integer($1, "index") }) {
        let index = integer(option, "index")
        guard index == enriched.count + 1 else { return nil }
        let candidate = candidates[index - 1]
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

NSRunningApplication(processIdentifier: pid)?.activate(options: [])
guard let writingSurface = editor(),
      AXUIElementSetAttributeValue(
        writingSurface,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
      ) == .success else {
    fputs("could not focus Loom's exact writing surface for completion reversal\n", stderr)
    exit(1)
}
guard let original = readManuscript() else {
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
guard let wordAccepted = waitForWitness(timeout: 10, { witness in
    let rendered = visual(witness)
    let action = lastAction(witness)
    return sameFamily(witness, context: context, runIds: runIds) &&
        string(witness, "selected_run_id") == initialRunId &&
        integer(witness, "accepted_chunk_count") == 1 &&
        bool(witness, "authority_frozen") &&
        bool(rendered, "optionHeld") &&
        !bool(rendered, "fanVisible") &&
        string(action, "kind") == "option_word" &&
        string(action, "run_id") == initialRunId &&
        integer(action, "sequence") > initialActionSequence
}) else {
    releaseOptionAndFail("Option-Right did not retain physical Option and exact cached-session authority")
}

guard postKey(123, down: true, flags: [.maskAlternate]),
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

guard pressButton(named: "Turn Shuttle on") else {
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
let shuttleAction = lastAction(shuttleAccepted)
guard shuttleAcceptedBytes.count - original.count == integer(shuttleAction, "inserted_utf8_bytes") else {
    fputs("Shuttle's persisted byte delta did not equal its authorized cached word\n", stderr)
    exit(1)
}

guard pressButton(named: "Turn Shuttle off") else {
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

guard AXUIElementSetAttributeValue(
        writingSurface,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
      ) == .success,
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
guard returnAcceptedBytes.count - original.count == integer(returnAction, "inserted_utf8_bytes"),
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
guard tabAcceptedBytes.count - original.count == integer(tabAction, "inserted_utf8_bytes"),
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

guard pressButton(named: "Turn autocomplete off") else {
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
