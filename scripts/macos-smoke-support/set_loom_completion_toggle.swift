import Foundation
import CoreFoundation
#if os(macOS)
import AppKit
import ApplicationServices
#endif

struct CompletionControl: Equatable {
    let mode: String
    let suggestionsEnabled: Bool

    // Match the actual button's separate AX fields, never substrings in help,
    // status prose, ancestors, or obsolete on/off labels. The toolbar switches
    // modes when enabled; pressing it is NOT the on/off operation.
    static func read(role: String, fields: [String]) -> CompletionControl? {
        guard role == "AXButton" else { return nil }
        let modes = ["Ghost text", "Loompad"].filter { fields.contains($0) }
        let actions = ["Enable suggestions", "Switch to Loompad", "Switch to ghost text"]
            .filter { fields.contains($0) }
        guard modes.count == 1, actions.count == 1 else { return nil }
        let mode = modes[0]
        switch actions[0] {
        case "Enable suggestions":
            return CompletionControl(mode: mode, suggestionsEnabled: false)
        case "Switch to Loompad" where mode == "Ghost text":
            return CompletionControl(mode: mode, suggestionsEnabled: true)
        case "Switch to ghost text" where mode == "Loompad":
            return CompletionControl(mode: mode, suggestionsEnabled: true)
        default:
            return nil
        }
    }
}

enum CompletionActivation: String {
    case policyShortcut = "Cmd+Shift+G"
    case modePress = "AXPress"
}

struct CompletionStep {
    let before: CompletionControl
    let after: CompletionControl
    let activation: CompletionActivation
}

// A no-model chrome exercise ends with policy off and the captured mode intact.
// It does not assert that a writer loaded, generated, or rendered any text.
func completionExercise(from initial: CompletionControl) -> [CompletionStep] {
    let off = CompletionControl(mode: initial.mode, suggestionsEnabled: false)
    let on = CompletionControl(mode: initial.mode, suggestionsEnabled: true)
    let other = CompletionControl(mode: initial.mode == "Ghost text" ? "Loompad" : "Ghost text",
                                  suggestionsEnabled: true)
    var result: [CompletionStep] = []
    if initial.suggestionsEnabled {
        result.append(CompletionStep(before: initial, after: off, activation: .policyShortcut))
    }
    result.append(contentsOf: [
        CompletionStep(before: off, after: on, activation: .policyShortcut),
        CompletionStep(before: on, after: other, activation: .modePress),
        CompletionStep(before: other, after: on, activation: .modePress),
        CompletionStep(before: on, after: off, activation: .policyShortcut)
    ])
    return result
}

enum CompletionTransitionResult: Equatable {
    case waiting
    case activate(CompletionActivation)
    case complete
}

enum CompletionTransitionError: Error {
    case unexpectedState
}

struct CompletionTransition {
    let step: CompletionStep
    private var sent = false

    init(step: CompletionStep) { self.step = step }

    mutating func observe(_ state: CompletionControl, actionable: Bool) throws -> CompletionTransitionResult {
        guard state == step.before || state == step.after else {
            throw CompletionTransitionError.unexpectedState
        }
        guard actionable else { return .waiting }
        if sent {
            return state == step.after ? .complete : .waiting
        }
        guard state == step.before else { throw CompletionTransitionError.unexpectedState }
        sent = true
        return .activate(step.activation)
    }
}

// Renderer-owned metadata is diagnostic input, never independent visual or
// durable-store proof. Do not echo opaque IDs, paths, unknown keys, or prose.
func witnessBoolean(_ value: Any?) -> Bool? {
    guard let number = value as? NSNumber,
          CFGetTypeID(number) == CFBooleanGetTypeID() else { return nil }
    return number.boolValue
}

func witnessCount(_ value: Any?) -> Int? {
    guard let number = value as? NSNumber,
          CFGetTypeID(number) != CFBooleanGetTypeID(),
          number.doubleValue.isFinite,
          (0...1_000_000).contains(number.doubleValue),
          number.doubleValue.rounded(.towardZero) == number.doubleValue else { return nil }
    return number.intValue
}

func witnessReport(_ stage: String) -> [String: Any] {
    ["schema": "delysis.loom-completion-diagnostic.v1", "stage": stage,
     "visible_ghost_proven": false, "evidence_kind": "renderer_metadata_only"]
}

func inspectCompletionWitness(_ json: String) -> [String: Any] {
    guard json.utf8.count <= 65_536 else { return witnessReport("oversized_witness") }
    guard let data = json.data(using: .utf8),
          let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          value["schema"] as? String == "delysis.loom-completion-witness.v1",
          let mode = value["mode"] as? String, ["visual", "source"].contains(mode),
          let cached = witnessBoolean(value["session_cached"]),
          let enabled = witnessBoolean(value["autocomplete_enabled"]),
          let shuttle = witnessBoolean(value["shuttle_enabled"]),
          let hidden = witnessBoolean(value["inline_hidden_requested"]),
          let frozen = witnessBoolean(value["authority_frozen"]),
          let accepted = witnessCount(value["accepted_chunk_count"]),
          let count = witnessCount(value["family_count"]), count <= 256,
          let candidates = value["candidates"] as? [[String: Any]], candidates.count <= 256 else {
        return witnessReport("malformed_witness")
    }
    let allowedPhases = ["inactive", "stale_scope", "awaiting_family", "awaiting_hydration",
                         "pending", "ready", "dismissed", "terminal_shortfall"]
    let suppliedPhase = (value["family_phase"] as? [String: Any])?["kind"] as? String ?? ""
    let phase = allowedPhases.contains(suppliedPhase) ? suppliedPhase : "unknown"
    let selectedRun = value["selected_run_id"] as? String ?? ""
    let selectedCandidate = value["selected_candidate_id"] as? String ?? ""
    let selectedKey = value["selected_presentation_key"] as? String ?? ""
    let renderedKey = value["rendered_presentation_key"] as? String ?? ""
    let visibleKey = value["inline_visible_key"] as? String ?? ""
    let visual = value["visual"] as? [String: Any] ?? [:]
    let selectedMatches = candidates.filter {
        $0["run_id"] as? String == selectedRun && $0["candidate_id"] as? String == selectedCandidate
    }
    let keysAgree = !selectedKey.isEmpty && selectedKey == renderedKey && selectedKey == visibleKey
    let keyKind: String
    if selectedKey.range(of: "^stream:[^:]{1,128}:[0-9]+(:prose-prefix:[0-9]+)?$", options: .regularExpression) != nil {
        keyKind = "stream"
    } else if selectedKey.range(of: "^[^:]{1,128}:[0-9a-f]{64}(:prose-prefix:[0-9]+)?$", options: .regularExpression) != nil {
        keyKind = "candidate_blob"
    } else { keyKind = selectedKey.isEmpty ? "missing" : "other" }
    let stage: String
    if (value["writer_id"] as? String ?? "").isEmpty { stage = "writer_unavailable" }
    else if !enabled { stage = "policy_off" }
    else if !cached { stage = "session_not_cached" }
    else if shuttle { stage = "inline_suppressed_by_mode" }
    else if hidden { stage = "inline_hidden_requested" }
    else if accepted != 0 { stage = "accepted_prefix" }
    else if frozen { stage = "authority_frozen" }
    else if count != candidates.count { stage = "family_cardinality_mismatch" }
    else if selectedRun.isEmpty || selectedCandidate.isEmpty || selectedMatches.count != 1 {
        stage = "selected_candidate_missing_or_ambiguous"
    } else if !keysAgree { stage = "presentation_keys_disagree" }
    else if selectedMatches[0]["presentation_key"] as? String != selectedKey ||
            (witnessCount(selectedMatches[0]["text_utf8_bytes"]) ?? 0) <= 0 {
        stage = "selected_candidate_metadata_invalid"
    }
    else if mode == "source" { stage = "source_projection_reported" }
    else if witnessBoolean(visual["available"]) != true { stage = "visual_projection_unavailable" }
    else if witnessBoolean(visual["inlineHidden"]) != false { stage = "visual_inline_hidden" }
    else if witnessBoolean(visual["fanVisible"]) != false { stage = "visual_fan_visible" }
    else if visual["selectedCandidateId"] as? String != selectedCandidate ||
            visual["selectedPresentationKey"] as? String != selectedKey { stage = "visual_identity_mismatch" }
    else if keyKind == "stream" { stage = "stream_projection_reported" }
    else if keyKind == "candidate_blob" { stage = "terminal_projection_reported" }
    else { stage = "unclassified_projection_reported" }
    var result = witnessReport(stage)
    result["mode"] = mode
    result["family_phase"] = phase
    result["family_count"] = count
    result["candidate_count"] = candidates.count
    result["session_cached"] = cached
    result["presentation_key_kind"] = keyKind
    result["presentation_keys_agree"] = keysAgree
    result["accepted_chunk_count"] = accepted
    result["authority_frozen"] = frozen
    return result
}

struct CompletionWitnessCapture {
    private var containers = 0
    private var payloads = Set<String>()
    private var oversized = false

    mutating func beginContainer() { containers += 1 }
    mutating func observe(_ text: String) {
        guard text.utf8.count <= 65_536 else { oversized = true; return }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        // The current, explicitly labelled note contains JSON, not arbitrary
        // substring search through manuscript or unrelated status text.
        if trimmed.hasPrefix("{") && payloads.count < 2 { payloads.insert(trimmed) }
    }
    var report: [String: Any] {
        if containers == 0 { return witnessReport("witness_container_missing") }
        if containers != 1 { return witnessReport("witness_container_ambiguous") }
        if oversized { return witnessReport("oversized_witness") }
        if payloads.count > 1 { return witnessReport("witness_payload_ambiguous") }
        guard let json = payloads.first else { return witnessReport("witness_payload_missing") }
        return inspectCompletionWitness(json)
    }
}

func requestedEnabled(_ request: String, _ result: String) -> Bool? {
    switch (request, result) {
    case ("enable", "enabled"), ("Turn autocomplete on", "Turn autocomplete off"):
        return true
    case ("disable", "disabled"), ("Turn autocomplete off", "Turn autocomplete on"):
        return false
    default:
        return nil
    }
}

func fail(_ message: String, code: Int32 = 1) -> Never {
    fputs("\(message)\n", stderr)
    exit(code)
}

func emit(_ evidence: [String: Any]) {
    do {
        let data = try JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
        guard let text = String(data: data, encoding: .utf8) else { fail("invalid evidence encoding") }
        print(text)
    } catch {
        fail("could not serialize control evidence: \(error)")
    }
}

// Pure AX-contract checks; this does not establish a native UI or inference run.
func selfTest() {
    var checks = exerciseContractTests() + witnessContractTests()
    func check(_ condition: Bool, _ name: String) {
        guard condition else { fail("control contract regression: \(name)") }
        checks += 1
    }
    for mode in ["Ghost text", "Loompad"] {
        check(CompletionControl.read(role: "AXButton", fields: [mode, "Enable suggestions"]) ==
            CompletionControl(mode: mode, suggestionsEnabled: false), "disabled \(mode)")
        let action = mode == "Ghost text" ? "Switch to Loompad" : "Switch to ghost text"
        check(CompletionControl.read(role: "AXButton", fields: [mode, action]) ==
            CompletionControl(mode: mode, suggestionsEnabled: true), "enabled \(mode)")
        check(CompletionControl.read(role: "AXStaticText", fields: [mode, action]) == nil, "reject status prose")
        check(CompletionControl.read(role: "AXButton", fields: [mode]) == nil, "name alone is not state")
    }
    for fields in [
        ["Turn autocomplete on"], ["Turn autocomplete off"],
        ["Help: Ghost text", "Enable suggestions"],
        ["Ghost text", "Switch to ghost text"],
        ["Loompad", "Switch to Loompad"],
        ["Ghost text", "Loompad", "Enable suggestions"],
        ["Ghost text", "Enable suggestions", "Switch to Loompad"]
    ] {
        check(CompletionControl.read(role: "AXButton", fields: fields) == nil, "reject ambiguous or retired fields")
    }
    check(requestedEnabled("enable", "enabled") == true, "enable intent")
    check(requestedEnabled("disable", "disabled") == false, "disable intent")
    // These two pairs are still passed by smoke-macos-app.sh. They are intent
    // arguments only, not strings searched for in the current accessibility UI.
    check(requestedEnabled("Turn autocomplete on", "Turn autocomplete off") == true, "existing enable caller")
    check(requestedEnabled("Turn autocomplete off", "Turn autocomplete on") == false, "existing disable caller")
    check(requestedEnabled("Ghost text", "Loompad") == nil, "mode switch is not a policy toggle")
    check(requestedEnabled("enable", "disabled") == nil, "reject contradictory intent")
    emit(["component": "completion-control-contract", "checks": checks, "native_acceptance": false])
}

func exerciseContractTests() -> Int {
    var checks = 0
    func check(_ value: Bool, _ name: String) {
        guard value else { fail("exercise contract regression: \(name)") }
        checks += 1
    }
    for mode in ["Ghost text", "Loompad"] {
        for initiallyEnabled in [false, true] {
            let initial = CompletionControl(mode: mode, suggestionsEnabled: initiallyEnabled)
            let steps = completionExercise(from: initial)
            check(steps.count == (initiallyEnabled ? 5 : 4), "bounded exercise length")
            check(steps.first?.before == initial, "starts from captured state")
            check(steps.last?.after == CompletionControl(mode: mode, suggestionsEnabled: false), "leaves policy off and mode unchanged")
            check(steps.filter { $0.activation == .modePress }.count == 2, "two real mode changes")
            var last = initial
            for step in steps {
                check(step.before == last, "no skipped state")
                check(step.before != step.after, "every action changes observed state")
                switch step.activation {
                case .policyShortcut:
                    check(step.before.mode == step.after.mode, "shortcut never switches modes")
                    check(step.before.suggestionsEnabled != step.after.suggestionsEnabled, "shortcut changes policy")
                case .modePress:
                    check(step.before.mode != step.after.mode, "press changes mode")
                    check(step.before.suggestionsEnabled && step.after.suggestionsEnabled, "press is not an off button")
                }
                var pending = CompletionTransition(step: step)
                check((try? pending.observe(step.before, actionable: false)) == .waiting, "busy control cannot activate")
                check((try? pending.observe(step.before, actionable: true)) == .activate(step.activation), "exact before state admits action")
                for _ in 0..<5 {
                    check((try? pending.observe(step.before, actionable: true)) == .waiting, "late acknowledgement cannot duplicate input")
                }
                check((try? pending.observe(step.after, actionable: false)) == .waiting, "busy target is not success")
                check((try? pending.observe(step.after, actionable: true)) == .complete, "exact settled acknowledgement completes")
                var unactivated = CompletionTransition(step: step)
                check((try? unactivated.observe(step.after, actionable: true)) == nil, "external change is not our success")
                let alien = CompletionControl(mode: "alien", suggestionsEnabled: true)
                check((try? pending.observe(alien, actionable: true)) == nil, "foreign mode fails closed")
                last = step.after
            }
        }
    }
    return checks
}

func witnessContractTests() -> Int {
    var checks = 0
    func check(_ condition: Bool, _ name: String) {
        guard condition else { fail("witness diagnostic regression: \(name)") }
        checks += 1
    }
    let key = "native-candidate:" + String(repeating: "a", count: 64)
    var witness: [String: Any] = [
        "schema": "delysis.loom-completion-witness.v1", "mode": "visual",
        "writer_id": "writer", "session_cached": true, "family_count": 4,
        "family_phase": ["kind": "ready"], "autocomplete_enabled": true,
        "shuttle_enabled": false, "inline_hidden_requested": false,
        "accepted_chunk_count": 0, "authority_frozen": false,
        "selected_run_id": "run1", "selected_candidate_id": "run:run1",
        "selected_presentation_key": key, "rendered_presentation_key": key,
        "inline_visible_key": key,
        "candidates": (1...4).map { ["run_id": "run\($0)", "candidate_id": "run:run\($0)",
                                     "presentation_key": key, "text_utf8_bytes": 8] as [String: Any] },
        "visual": ["available": true, "inlineHidden": false, "fanVisible": false,
                   "selectedCandidateId": "run:run1", "selectedPresentationKey": key]
    ]
    func inspect(_ object: [String: Any]) -> [String: Any] {
        let data = try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        return inspectCompletionWitness(String(data: data, encoding: .utf8)!)
    }
    func reason(_ object: [String: Any]) -> String { inspect(object)["stage"] as? String ?? "" }
    check(reason(witness) == "terminal_projection_reported", "terminal projection is not a streaming failure diagnosis")
    check(inspect(witness)["visible_ghost_proven"] as? Bool == false, "renderer witness alone never proves rendering")
    let privateMarker = "PRIVATE_MANUSCRIPT_SENTINEL"
    witness["private_unknown_field"] = privateMarker
    let report = inspect(witness)
    check(!String(describing: report).contains(privateMarker), "unknown payload fields are not logged")
    check(report["selected_run_id"] == nil && report["selected_presentation_key"] == nil,
          "opaque private identity strings are not copied")
    for (field, value, stage) in [
        ("writer_id", "", "writer_unavailable"),
        ("mode", "source", "source_projection_reported"),
        ("selected_presentation_key", "stream:run1:8", "presentation_keys_disagree")
    ] {
        var changed = witness; changed[field] = value
        check(reason(changed) == stage, "diagnoses \(field)")
    }
    for (field, stage) in [("session_cached", "session_not_cached"),
                           ("autocomplete_enabled", "policy_off")] {
        var changed = witness; changed[field] = false
        check(reason(changed) == stage, "diagnoses \(field)")
    }
    for (field, stage) in [("shuttle_enabled", "inline_suppressed_by_mode"),
                           ("inline_hidden_requested", "inline_hidden_requested"),
                           ("authority_frozen", "authority_frozen")] {
        var changed = witness; changed[field] = true
        check(reason(changed) == stage, "diagnoses \(field)")
    }
    var changed = witness; changed["accepted_chunk_count"] = 1
    check(reason(changed) == "accepted_prefix", "accepted suffix is distinguished")
    changed = witness; changed["candidates"] = (1...4).map { ["run_id": "foreign\($0)", "candidate_id": "run:foreign\($0)"] }
    check(reason(changed) == "selected_candidate_missing_or_ambiguous", "candidate membership")
    changed = witness; changed["visual"] = ["available": false]
    check(reason(changed) == "visual_projection_unavailable", "visual unavailable")
    changed = witness; changed["visual"] = ["available": true, "inlineHidden": true]
    check(reason(changed) == "visual_inline_hidden", "visual hidden")
    changed = witness; changed["visual"] = ["available": true, "inlineHidden": false, "fanVisible": true]
    check(reason(changed) == "visual_fan_visible", "fan is not in-caret ghost")
    changed = witness; changed["session_cached"] = 1
    check(reason(changed) == "malformed_witness", "integer cannot impersonate Boolean")
    changed = witness; changed["family_count"] = true
    check(reason(changed) == "malformed_witness", "Boolean cannot impersonate count")
    changed = witness; changed["family_phase"] = ["kind": privateMarker]
    check(inspect(changed)["family_phase"] as? String == "unknown", "phase is whitelisted")
    check(inspectCompletionWitness("not JSON")["stage"] as? String == "malformed_witness", "malformed payload")
    check(inspectCompletionWitness(String(repeating: " ", count: 65537))["stage"] as? String == "oversized_witness", "bounded payload")
    for stream in ["stream:run1:8", "stream:run1:8:prose-prefix:5"] {
        changed = witness
        for field in ["selected_presentation_key", "rendered_presentation_key", "inline_visible_key"] { changed[field] = stream }
        var streamCandidates = changed["candidates"] as! [[String: Any]]
        streamCandidates[0]["presentation_key"] = stream
        changed["candidates"] = streamCandidates
        changed["visual"] = ["available": true, "inlineHidden": false, "fanVisible": false,
                             "selectedCandidateId": "run:run1", "selectedPresentationKey": stream]
        check(reason(changed) == "stream_projection_reported", "reports stream key")
        check(inspect(changed)["visible_ghost_proven"] as? Bool == false, "stream metadata is not independent proof")
    }
    for count in [0, 3, 5] {
        var changed = witness; changed["family_count"] = count
        check(reason(changed) == "family_cardinality_mismatch", "family count disagrees with candidates")
    }
    for length in [0, -1] {
        var changed = witness
        var candidates = changed["candidates"] as! [[String: Any]]
        candidates[0]["text_utf8_bytes"] = length
        changed["candidates"] = candidates
        check(reason(changed) == "selected_candidate_metadata_invalid", "selected text length invalid")
    }
    changed = witness
    var candidates = changed["candidates"] as! [[String: Any]]
    candidates[0]["presentation_key"] = "foreign"
    changed["candidates"] = candidates
    check(reason(changed) == "selected_candidate_metadata_invalid", "selected candidate key disagrees")
    var capture = CompletionWitnessCapture()
    check(capture.report["stage"] as? String == "witness_container_missing", "missing note")
    capture.beginContainer()
    check(capture.report["stage"] as? String == "witness_payload_missing", "missing payload")
    let data = try! JSONSerialization.data(withJSONObject: witness, options: [.sortedKeys])
    let json = String(data: data, encoding: .utf8)!
    capture.observe(json)
    capture.observe(json)
    check(capture.report["stage"] as? String == "terminal_projection_reported", "duplicate AX fields are one payload")
    capture.observe("{\"schema\":\"delysis.loom-completion-witness.v1\"}")
    check(capture.report["stage"] as? String == "witness_payload_ambiguous", "divergent payloads fail closed")
    capture.beginContainer()
    check(capture.report["stage"] as? String == "witness_container_ambiguous", "multiple note containers fail closed")
    return checks
}

if CommandLine.arguments == [CommandLine.arguments[0], "--self-test"] {
    selfTest()
    exit(0)
}

#if os(macOS)
let arguments = Array(CommandLine.arguments.dropFirst())
let queryOnly = arguments.first == "--state"
let exercise = arguments.first == "--exercise"
let pidArgument = (queryOnly || exercise) ? arguments.dropFirst().first : arguments.first
guard let pidText = pidArgument, let pid = Int32(pidText), pid > 0 else {
    fail("usage: set_loom_completion_toggle <pid> enable enabled|disable disabled [require-press|allow-already]; or --state|--exercise <pid>", code: 2)
}
let desired: Bool?
let requirePress: Bool
if queryOnly || exercise {
    guard arguments.count == 2 else { fail("observation/exercise requires exactly one PID", code: 2) }
    desired = nil
    requirePress = false
} else {
    guard arguments.count == 3 || arguments.count == 4,
          let value = requestedEnabled(arguments[1], arguments[2]) else {
        fail("unsupported or contradictory completion control intent", code: 2)
    }
    if arguments.count == 4 && !["require-press", "allow-already"].contains(arguments[3]) {
        fail("unsupported press requirement", code: 2)
    }
    desired = value
    requirePress = arguments.count == 4 && arguments[3] == "require-press"
}
guard let running = NSRunningApplication(processIdentifier: pid), !running.isTerminated,
      let launchDate = running.launchDate, let executable = running.executableURL else {
    fail("target process identity is unavailable")
}
let application = AXUIElementCreateApplication(pid)
guard AXUIElementSetMessagingTimeout(application, 0.5) == .success else {
    fail("could not bound accessibility messaging time")
}

struct ControlObservation {
    let element: AXUIElement
    let state: CompletionControl
    let actionable: Bool
}

enum ControlReadError: Error { case incomplete }

func attribute(_ element: AXUIElement, _ name: CFString) throws -> CFTypeRef? {
    var value: CFTypeRef?
    switch AXUIElementCopyAttributeValue(element, name, &value) {
    case .success: return value
    case .noValue, .attributeUnsupported: return nil
    default: throw ControlReadError.incomplete
    }
}

func requireProcess(frontmost: Bool = false) {
    guard !running.isTerminated,
          let current = NSRunningApplication(processIdentifier: pid), !current.isTerminated,
          current.launchDate == launchDate, current.executableURL == executable else {
        fail("target process identity changed during completion control observation")
    }
    if frontmost && NSWorkspace.shared.frontmostApplication?.processIdentifier != pid {
        fail("target is not frontmost; refusing to send completion input")
    }
}

func snapshot(until deadline: TimeInterval) throws -> (control: ControlObservation?, witness: [String: Any]) {
    requireProcess()
    var queue: [(AXUIElement, Bool)] = [(application, false)]
    var witness = CompletionWitnessCapture()
    var cursor = 0
    var matches: [ControlObservation] = []
    while cursor < queue.count && cursor < 4096 && ProcessInfo.processInfo.systemUptime < deadline {
        let (element, parentIsWitness) = queue[cursor]
        cursor += 1
        let fields = try [kAXDescriptionAttribute, kAXTitleAttribute, kAXHelpAttribute]
            .compactMap { try attribute(element, $0 as CFString) as? String }
        let role = try attribute(element, kAXRoleAttribute as CFString) as? String ?? ""
        let witnessContainer = queryOnly && fields.contains("Completion session witness")
        if witnessContainer { witness.beginContainer() }
        let insideWitness = queryOnly && (parentIsWitness || witnessContainer)
        if insideWitness {
            for field in fields { witness.observe(field) }
            if let value = try attribute(element, kAXValueAttribute as CFString) as? String {
                witness.observe(value)
            }
        }
        if let state = CompletionControl.read(role: role, fields: fields) {
            let actionable = try attribute(element, kAXEnabledAttribute as CFString) as? Bool == true
            matches.append(ControlObservation(element: element, state: state, actionable: actionable))
        }
        if let raw = try attribute(element, kAXChildrenAttribute as CFString) {
            guard let children = raw as? [AXUIElement] else { throw ControlReadError.incomplete }
            guard children.count <= 4096 - queue.count else {
                fail("accessibility traversal exceeded its bound; refusing a partial control match")
            }
            queue.append(contentsOf: children.map { ($0, insideWitness) })
        }
    }
    guard cursor == queue.count && ProcessInfo.processInfo.systemUptime < deadline else {
        throw ControlReadError.incomplete
    }
    guard matches.count <= 1 else { fail("ambiguous current completion controls in the exact process") }
    return (matches.first, witness.report)
}

func control(until deadline: TimeInterval) throws -> ControlObservation? {
    try snapshot(until: deadline).control
}

func activate(_ action: CompletionActivation, observed: ControlObservation) {
    requireProcess(frontmost: true)
    var elementPid: pid_t = 0
    guard observed.actionable,
          AXUIElementGetPid(observed.element, &elementPid) == .success, elementPid == pid else {
        fail("completion control does not belong to the captured process")
    }
    switch action {
    case .policyShortcut:
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 5, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 5, keyDown: false) else {
            fail("could not construct completion shortcut")
        }
        down.flags = [.maskCommand, .maskShift]
        up.flags = [.maskCommand, .maskShift]
        down.postToPid(pid)
        up.postToPid(pid)
    case .modePress:
        guard AXUIElementPerformAction(observed.element, kAXPressAction as CFString) == .success else {
            fail("could not activate the current completion mode control")
        }
    }
}

func observedTransition(_ step: CompletionStep, until overallDeadline: TimeInterval) {
    let deadline = min(overallDeadline, ProcessInfo.processInfo.systemUptime + 30)
    var pending = CompletionTransition(step: step)
    while ProcessInfo.processInfo.systemUptime < deadline {
        if let observed = try? control(until: deadline) {
            let result: CompletionTransitionResult
            do { result = try pending.observe(observed.state, actionable: observed.actionable) }
            catch { fail("completion state changed outside the requested transition") }
            switch result {
            case .waiting: break
            case .activate(let action): activate(action, observed: observed)
            case .complete: return
            }
        }
        Thread.sleep(forTimeInterval: 0.05)
    }
    fail("completion transition deadline exceeded; no repeated input or inferred success")
}

let deadline = ProcessInfo.processInfo.systemUptime + (exercise ? 90 : 30)
while ProcessInfo.processInfo.systemUptime < deadline {
    if queryOnly {
        do {
            let reading = try snapshot(until: deadline)
            var evidence: [String: Any] = ["control_state": "missing", "enabled": false,
                "evidence_kind": "accessibility_control_state_only", "completion_witness": reading.witness]
            if let observed = reading.control {
                evidence["control_state"] = "present"
                evidence["control_label"] = observed.state.mode
                evidence["suggestions_enabled"] = observed.state.suggestionsEnabled
                evidence["enabled"] = observed.actionable
            }
            emit(evidence)
            exit(0)
        } catch {
            Thread.sleep(forTimeInterval: 0.05)
            continue
        }
    }
    if let observed = try? control(until: deadline) {
        let state = observed.state
        if observed.actionable {
            if exercise {
                let steps = completionExercise(from: state)
                var evidence: [[String: Any]] = []
                for step in steps {
                    observedTransition(step, until: deadline)
                    evidence.append(["activation": step.activation.rawValue,
                        "before_mode": step.before.mode, "before_enabled": step.before.suggestionsEnabled,
                        "after_mode": step.after.mode, "after_enabled": step.after.suggestionsEnabled])
                }
                emit(["schema": "delysis.loom-completion-control-exercise.v1", "pid": pid,
                      "original_mode": state.mode, "suggestions_enabled": false,
                      "observed_transitions": evidence,
                      "evidence_kind": "accessibility_control_state_only", "generation_acceptance": false])
                exit(0)
            }
            guard let desired else { fail("missing suggestion policy intent", code: 2) }
            var pressed = false
            if state.suggestionsEnabled != desired {
                observedTransition(CompletionStep(before: state,
                    after: CompletionControl(mode: state.mode, suggestionsEnabled: desired),
                    activation: .policyShortcut), until: deadline)
                // Receipt flag records a completed observed transition, not
                // merely the intention to send input.
                pressed = true
            }
            guard pressed || !requirePress else {
                fail("requested state was already present without the required single activation")
            }
            emit(["requested_control": arguments[1], "resulting_control": arguments[2],
                  "control_label": state.mode, "suggestions_enabled": desired,
                  "pressed_exactly_once": pressed, "activation": pressed ? "Cmd+Shift+G" : "none",
                  "evidence_kind": "accessibility_control_state_only"])
            exit(0)
        }
    }
    Thread.sleep(forTimeInterval: 0.05)
}
fail("completion control deadline exceeded; no inference acceptance established")
#else
fail("native completion control automation requires macOS; only --self-test is portable")
#endif
