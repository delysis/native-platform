import Foundation
import CoreFoundation

// Shared by the native observer and its portable contract tests. Neither a
// candidate buffer nor a controller-ready flag establishes rendered glyphs.
struct InlineProjection {
    let runId: String
    let candidateId: String
    let key: String
    let text: String
    let candidateBytes: Int
    let sequence: Int64?
}

func string(_ value: [String: Any], _ key: String) -> String { value[key] as? String ?? "" }
func integer(_ value: [String: Any], _ key: String) -> Int {
    guard let number = value[key] as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return -1 }
    return Int(number.stringValue) ?? -1
}
func bool(_ value: [String: Any], _ key: String) -> Bool {
    guard let number = value[key] as? NSNumber, CFGetTypeID(number) == CFBooleanGetTypeID() else { return false }
    return number.boolValue
}

func falseFlag(_ value: [String: Any], _ key: String) -> Bool {
    guard let number = value[key] as? NSNumber, CFGetTypeID(number) == CFBooleanGetTypeID() else { return false }
    return !number.boolValue
}

enum Rejection: String, Error {
    case controller, family, identity, glyph, boundary, sequence
}

func inlineProjection(_ witness: [String: Any], family: [String], manuscript: String) throws -> InlineProjection {
    guard string(witness, "schema") == "delysis.loom-completion-witness.v1",
          string(witness, "mode") == "visual", bool(witness, "session_cached"),
          bool(witness, "autocomplete_enabled"), falseFlag(witness, "shuttle_enabled"),
          integer(witness, "accepted_chunk_count") == 0, falseFlag(witness, "authority_frozen")
    else { throw Rejection.controller }
    let candidates = witness["candidates"] as? [[String: Any]] ?? []
    guard family.count == 4, Set(family).count == 4, candidates.count == 4,
          Set(candidates.map { string($0, "run_id") }) == Set(family)
    else { throw Rejection.family }
    let runId = string(witness, "selected_run_id")
    let candidateId = string(witness, "selected_candidate_id")
    let key = string(witness, "selected_presentation_key")
    let visual = witness["visual"] as? [String: Any] ?? [:]
    guard !key.isEmpty, !candidateId.isEmpty, family.contains(runId),
          key == string(witness, "rendered_presentation_key"), key == string(witness, "inline_visible_key"),
          bool(visual, "available"), falseFlag(visual, "inlineHidden"), falseFlag(visual, "fanVisible"),
          key == string(visual, "selectedPresentationKey"), candidateId == string(visual, "selectedCandidateId"),
          let candidate = candidates.first(where: { string($0, "run_id") == runId && string($0, "candidate_id") == candidateId })
    else { throw Rejection.identity }
    guard string(candidate, "presentation_key") == key else { throw Rejection.identity }
    let inline = visual["inline"] as? [String: Any] ?? [:]
    let text = string(inline, "text")
    let candidateBytes = integer(candidate, "text_utf8_bytes")
    guard string(inline, "presentationKey") == key,
          text.rangeOfCharacter(from: .whitespacesAndNewlines.inverted) != nil,
          integer(inline, "utf8Bytes") == text.utf8.count,
          text.utf8.count == candidateBytes
    else { throw Rejection.glyph }
    guard integer(candidate, "target_byte") == manuscript.utf8.count else { throw Rejection.boundary }
    var sequence: Int64?
    let prefix = "stream:\(runId):"
    if key.hasPrefix(prefix) {
        let suffix = String(key.dropFirst(prefix.count)).components(separatedBy: ":")
        guard let parsed = Int64(suffix[0]), parsed >= 0, String(parsed) == suffix[0],
              suffix.count == 1 || (suffix.count == 3 && suffix[1] == "prose-prefix" && Int(suffix[2]) == candidateBytes)
        else { throw Rejection.sequence }
        sequence = parsed
    }
    return InlineProjection(runId: runId, candidateId: candidateId, key: key, text: text,
                            candidateBytes: candidateBytes, sequence: sequence)
}

func editorValueMatches(_ value: String, manuscript: String, glyph: String) -> Bool {
    // WebKit may exclude aria-hidden decoration from AXValue. This is correct:
    // glyph evidence comes from the DOM observation, not from manuscript bytes.
    let withoutAXLineBreak = value.replacingOccurrences(of: "[\r\n]+$", with: "", options: .regularExpression)
    return withoutAXLineBreak.utf8.elementsEqual(manuscript.utf8) ||
        withoutAXLineBreak.utf8.elementsEqual((manuscript + glyph).utf8)
}

// The smoke owns Untitled.md. Labels are AX title/description, never editor contents.
func namedEditorIndex(_ elements: [(role: String, labels: [String])]) -> Int? {
    let matches = elements.indices.filter {
        elements[$0].role == "AXTextArea" && elements[$0].labels.contains("Untitled, manuscript editor")
    }
    return matches.count == 1 ? matches[0] : nil
}

func selfTest() {
    var assertions = 0
    var failures = 0
    func check(_ condition: Bool, _ message: String) {
        if !condition { fputs("live observer contract failed: \(message)\n", stderr); failures += 1 }
        assertions += 1
    }
    let family = ["r1", "r2", "r3", "r4"]
    let key = "stream:r1:7"
    let visual: [String: Any] = ["available": true, "inlineHidden": false, "fanVisible": false,
        "selectedCandidateId": "run:r1", "selectedPresentationKey": key,
        "inline": ["presentationKey": key, "text": "world again", "utf8Bytes": 11]]
    let witness: [String: Any] = ["schema": "delysis.loom-completion-witness.v1", "mode": "visual", "session_cached": true, "autocomplete_enabled": true,
        "shuttle_enabled": false, "accepted_chunk_count": 0, "authority_frozen": false,
        "selected_run_id": "r1", "selected_candidate_id": "run:r1", "selected_presentation_key": key,
        "rendered_presentation_key": key, "inline_visible_key": key, "visual": visual,
        "candidates": family.map { ["run_id": $0, "candidate_id": "run:\($0)", "presentation_key": "stream:\($0):7", "target_byte": 6,
                                    "text_utf8_bytes": 11] as [String: Any] }]
    func accepts(_ value: [String: Any], _ ids: [String] = family) -> Bool {
        (try? inlineProjection(value, family: ids, manuscript: "hello ")) != nil
    }
    check(accepts(witness), "complete multiword preview")
    check(editorValueMatches("hello ", manuscript: "hello ", glyph: "world"), "aria-hidden glyph absent from AXValue")
    check(editorValueMatches("hello world", manuscript: "hello ", glyph: "world"), "glyph exposed in AXValue")
    check(!editorValueMatches("hello forged", manuscript: "hello ", glyph: "world"), "unrelated AX text")
    check(!editorValueMatches("hello", manuscript: "hello ", glyph: "world"), "lost manuscript space")
    check(!editorValueMatches("e\u{301}", manuscript: "é", glyph: "x"), "canonically equivalent but different manuscript bytes")
    check(!accepts(witness, ["other", "r2", "r3", "r4"]), "stale family")
    check(!accepts(witness, ["r1", "r1", "r3", "r4"]), "duplicate family")
    for field in ["schema", "selected_run_id", "selected_candidate_id", "selected_presentation_key", "rendered_presentation_key", "inline_visible_key"] {
        var changed = witness; changed[field] = "stale"
        check(!accepts(changed), "stale \(field)")
    }
    for inline: [String: Any] in [[:], ["presentationKey": key, "text": "", "utf8Bytes": 0],
        ["presentationKey": key, "text": "world", "utf8Bytes": 11],
        ["presentationKey": key, "text": "world", "utf8Bytes": 5],
        ["presentationKey": "stale", "text": "world", "utf8Bytes": 5]] {
        var changed = witness; var v = visual; v["inline"] = inline; changed["visual"] = v
        check(!accepts(changed), "missing or inconsistent DOM observation")
    }
    for field in ["inlineHidden", "fanVisible"] {
        var changed = witness; var v = visual; v[field] = true; changed["visual"] = v
        check(!accepts(changed), "hidden inline or active fan")
    }
    var changed = witness; changed["accepted_chunk_count"] = 1
    check(!accepts(changed), "consumed family")
    changed = witness; changed["authority_frozen"] = true
    check(!accepts(changed), "frozen family")
    for malformed: Any in [false, 0.5, "0"] {
        changed = witness; changed["accepted_chunk_count"] = malformed
        check(!accepts(changed), "invalid count type")
    }
    changed = witness; changed["autocomplete_enabled"] = 1
    check(!accepts(changed), "number is not a Boolean")
    for field in ["authority_frozen", "shuttle_enabled"] {
        changed = witness; changed.removeValue(forKey: field)
        check(!accepts(changed), "missing false flag \(field)")
    }
    for field in ["inlineHidden", "fanVisible"] {
        changed = witness; var v = visual; v.removeValue(forKey: field); changed["visual"] = v
        check(!accepts(changed), "missing visual false flag \(field)")
    }
    let projection = try! inlineProjection(witness, family: family, manuscript: "hello ")
    check(projection.sequence == 7 && projection.candidateBytes == 11 && projection.text.utf8.count == 11,
          "full-prefix rendered bytes equal the authorized projection")
    check(namedEditorIndex([("AXTextArea", ["Untitled, manuscript editor"])]) == 0, "actual App editor label")
    check(namedEditorIndex([("AXTextArea", ["Context editor"]), ("AXTextArea", ["Untitled, manuscript editor"])]) == 1, "exclude unrelated editor")
    check(namedEditorIndex([("AXButton", ["Untitled, manuscript editor"])]) == nil, "wrong accessibility role")
    check(namedEditorIndex([("AXTextArea", ["Manuscript editor"])]) == nil, "unscoped legacy name")
    check(namedEditorIndex([("AXTextArea", ["Other, manuscript editor"])]) == nil, "different manuscript")
    check(namedEditorIndex([("AXTextArea", ["Untitled, manuscript editor"]), ("AXTextArea", ["Untitled, manuscript editor"])]) == nil, "ambiguous named editors")
    if failures > 0 { fputs("\(failures)/\(assertions) assertions failed\n", stderr); exit(1) }
    print("\(assertions) live-observer contract assertions passed (not native acceptance)")
}

if CommandLine.arguments.count == 2 && CommandLine.arguments[1] == "--self-test" {
    selfTest(); exit(0)
}

#if os(macOS)
import AppKit
import ApplicationServices
import CryptoKit
import SQLite3

let terminalSnapshot = CommandLine.arguments.last == "--terminal-snapshot"
guard CommandLine.arguments.count == (terminalSnapshot ? 11 : 10),
      let pid = Int32(CommandLine.arguments[1]), pid > 0,
      let baseline = Int64(CommandLine.arguments[3]), baseline >= 0, baseline <= Int64.max - 4
else { fputs("usage: start_loom_live_streaming_monitor <pid> <db> <baseline> <manuscript> <stop> <ready> <failure> <generation-failure> <project-failure>\n", stderr); exit(2) }
let databasePath = CommandLine.arguments[2]
let manuscript = CommandLine.arguments[4]
let stopPath = CommandLine.arguments[5], readyPath = CommandLine.arguments[6], failurePath = CommandLine.arguments[7]
let guardPaths = [CommandLine.arguments[8], CommandLine.arguments[9]].filter { !$0.isEmpty }
let manager = FileManager.default
let application = AXUIElementCreateApplication(pid)
AXUIElementSetMessagingTimeout(application, 1)
guard let runningApplication = NSRunningApplication(processIdentifier: pid) else {
    fputs("exact Loom process is absent\n", stderr); exit(1)
}
var databasePointer: OpaquePointer?
guard sqlite3_open_v2(databasePath, &databasePointer, SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX, nil) == SQLITE_OK,
      let database = databasePointer else { fputs("cannot open isolated store read-only\n", stderr); exit(1) }
defer { sqlite3_close(database) }
sqlite3_busy_timeout(database, 50)
let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
var polls = 0, rejected: [String: Int] = [:], lastWitness: [String: Any] = [:]
var postTerminal: [String: Any] = [:]
var observedEditorLabels: [[String]] = []
// Two bounded, read-only snapshots distinguish arrival with a zero caret from
// a later change. These diagnostics never authorize a generation or a pass.
var firstAdmissionObservation: [String: Any] = [:]
var lastAdmissionObservation: [String: Any] = [:]

func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    return AXUIElementCopyAttributeValue(element, name, &value) == .success ? value : nil
}
func diagnosticSelection(_ element: AXUIElement) -> CFRange? {
    guard let raw = attribute(element, kAXSelectedTextRangeAttribute as CFString),
          CFGetTypeID(raw) == AXValueGetTypeID(),
          AXValueGetType(raw as! AXValue) == .cfRange else { return nil }
    var selection = CFRange()
    return AXValueGetValue(raw as! AXValue, .cfRange, &selection) ? selection : nil
}
func strings(_ element: AXUIElement) -> [String] {
    [kAXValueAttribute, kAXTitleAttribute, kAXDescriptionAttribute, kAXHelpAttribute]
        .compactMap { attribute(element, $0 as CFString) as? String }
}
func descendants() -> [AXUIElement]? {
    var queue = [application], cursor = 0
    while cursor < queue.count {
        guard queue.count <= 4096 else { return nil }
        if let children = attribute(queue[cursor], kAXChildrenAttribute as CFString) as? [AXUIElement] { queue.append(contentsOf: children) }
        cursor += 1
    }
    return queue
}
func witness(in elements: [AXUIElement]) -> [String: Any]? {
    var found: [String: [String: Any]] = [:]
    for text in elements.flatMap({ strings($0) }) {
        guard let marker = text.range(of: "\"schema\":\"delysis.loom-completion-witness.v1\""),
              let open = text[..<marker.lowerBound].lastIndex(of: "{"), let close = text.lastIndex(of: "}"), open <= close,
              let data = String(text[open...close]).data(using: .utf8),
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let canonical = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) else { continue }
        found[canonical.base64EncodedString()] = value
    }
    return found.count == 1 ? found.values.first : nil
}
func rows(_ sql: String, run: String? = nil, offset: Bool = false) -> [[String]]? {
    var statement: OpaquePointer?
    guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK, let statement else { return nil }
    defer { sqlite3_finalize(statement) }
    if let run { sqlite3_bind_text(statement, 1, run, -1, transient) }
    if offset { sqlite3_bind_int64(statement, 1, baseline) }
    var result: [[String]] = []
    while true {
        let status = sqlite3_step(statement)
        if status == SQLITE_DONE { return result }
        guard status == SQLITE_ROW else { return nil }
        result.append((0..<sqlite3_column_count(statement)).map { index in
            sqlite3_column_text(statement, index).map { String(cString: $0) } ?? ""
        })
    }
}
func count() -> Int64? { rows("SELECT count(*) FROM generation_runs;")?.first?.first.flatMap(Int64.init) }
func familyIds() -> [String]? { rows("SELECT run_id FROM generation_runs ORDER BY created_at_ms, run_id LIMIT 4 OFFSET ?1;", offset: true)?.compactMap(\.first) }
func openIds() -> [String]? {
    rows("WITH family AS (SELECT run_id, created_at_ms FROM generation_runs ORDER BY created_at_ms, run_id LIMIT 4 OFFSET ?1) SELECT f.run_id FROM family f LEFT JOIN generation_terminals t ON t.run_id=f.run_id WHERE t.run_id IS NULL ORDER BY f.created_at_ms, f.run_id;", offset: true)?.compactMap(\.first)
}
func terminal(_ run: String) -> Bool? {
    guard let value = rows("SELECT count(*) FROM generation_terminals WHERE run_id=?1;", run: run)?.first?.first.flatMap(Int.init) else { return nil }
    return value > 0
}
func terminalFamily(_ candidates: [[String: Any]], family: [String]) -> [[String]]? {
    guard let identities = rows("WITH family AS (SELECT run_id FROM generation_runs ORDER BY created_at_ms, run_id LIMIT 4 OFFSET ?1) SELECT f.run_id,t.candidate_id,c.output_blob_id FROM family f JOIN generation_terminals t ON t.run_id=f.run_id AND t.status='completed' JOIN generation_candidates c ON c.run_id=f.run_id AND c.candidate_id=t.candidate_id ORDER BY f.run_id;", offset: true),
          identities.count == 4, Set(identities.compactMap(\.first)) == Set(family),
          candidates.allSatisfy({ candidate in
              guard let identity = identities.first(where: { $0[0] == string(candidate, "run_id") }) else { return false }
              let base = "\(identity[1]):\(identity[2])"
              let key = string(candidate, "presentation_key")
              return string(candidate, "candidate_id") == "run:\(identity[0])" &&
                  (key == base || key == "\(base):prose-prefix:\(integer(candidate, "text_utf8_bytes"))")
          }) else { return nil }
    return identities
}
func cumulativeText(_ run: String, through sequence: Int64?) -> String? {
    guard let events = rows("SELECT sequence, payload_json FROM generation_events WHERE run_id=?1 AND event_kind = 'text_delta' ORDER BY sequence;", run: run) else { return nil }
    var text = "", exactSequence = sequence == nil
    for row in events {
        guard row.count == 2, let observed = Int64(row[0]) else { return nil }
        if let sequence, observed > sequence { break }
        guard let data = row[1].data(using: .utf8), let payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              string(payload, "kind") == "text_delta", let delta = payload["text"] as? String else { return nil }
        text += delta
        guard text.utf8.count <= 256 * 1024 else { return nil }
        if observed == sequence { exactSequence = true }
    }
    return exactSequence ? text : nil
}
func sha256(_ text: String) -> String { SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined() }
func reject(_ reason: String) { rejected[reason, default: 0] += 1; Thread.sleep(forTimeInterval: 0.05) }
func fail(_ reason: String) -> Never {
    let evidence: [String: Any] = ["schema": "delysis.loom-live-stream-failure.v1", "pid": pid, "reason": reason,
        "polls": polls, "generation_run_count": count() ?? -1, "family_run_ids": familyIds() ?? [],
        "open_run_ids": openIds() ?? [], "rejected_stages": rejected, "last_witness": lastWitness,
        "observed_editor_labels": observedEditorLabels,
        "first_admission_observation": firstAdmissionObservation,
        "last_admission_observation": lastAdmissionObservation,
        "post_terminal_observation": postTerminal, "observed_at_ms": Int64(Date().timeIntervalSince1970 * 1000)]
    do { try JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys]).write(to: URL(fileURLWithPath: failurePath), options: [.atomic]) }
    catch { fputs("cannot write failure evidence: \(error)\n", stderr) }
    fputs("Loom live-stream witness failed: \(reason)\n", stderr); exit(42)
}
let initialDeadline = ProcessInfo.processInfo.systemUptime + (terminalSnapshot ? 3 : 120)
var terminalDeadline: TimeInterval?
_ = manager.createFile(atPath: readyPath, contents: Data())
while ProcessInfo.processInfo.systemUptime < (terminalDeadline ?? initialDeadline) {
    polls += 1
    if manager.fileExists(atPath: stopPath) { fail("observer_stopped_before_live_witness") }
    if guardPaths.contains(where: { manager.fileExists(atPath: $0) }) { fail("asynchronous_guard_failed") }
    if runningApplication.isTerminated { fail("exact_process_exited") }
    guard let observedCount = count() else { reject("store_unreadable"); continue }
    if observedCount > baseline + 4 { fail("unexpected_generation_run") }
    guard let elements = descendants() else { reject("accessibility_truncated"); continue }
    // Focus is a measured prerequisite. The smoke driver establishes it;
    // this observer must not change focus/caret between witness and AX reads.
    let current = witness(in: elements)
    if let current { lastWitness = current }
    let identities: [(role: String, labels: [String])] = elements.map { element in
        ((attribute(element, kAXRoleAttribute as CFString) as? String) ?? "",
         [kAXTitleAttribute, kAXDescriptionAttribute].compactMap { attribute(element, $0 as CFString) as? String })
    }
    observedEditorLabels = identities.filter { $0.role == "AXTextArea" }.prefix(16).map { $0.labels }
    let editorIndex = namedEditorIndex(identities)
    let observedEditor = editorIndex.map { elements[$0] }
    let observedSelection = observedEditor.flatMap { diagnosticSelection($0) }
    let observedFocus = observedEditor.flatMap { attribute($0, kAXFocusedAttribute as CFString) as? Bool }
    let observedValue = observedEditor.flatMap { attribute($0, kAXValueAttribute as CFString) as? String }
    let internalSelection = current?["editor_selection"] as? [String: Any]
    let preAdmission = current?["pre_admission"] as? [String: Any]
    let lifecycle = preAdmission?["lifecycle"] as? [String: Any] ?? [:]
    let observation: [String: Any] = [
        "poll": polls, "generation_run_count": observedCount,
        "frontmost": NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
        "editor_present": observedEditor != nil, "ax_focus_available": observedFocus != nil,
        "ax_focused": observedFocus ?? false,
        "ax_selection_available": observedSelection != nil,
        "ax_caret_utf16": observedSelection?.location ?? -1,
        "ax_selection_length_utf16": observedSelection?.length ?? -1,
        "ax_value_available": observedValue != nil,
        "ax_matches_manuscript": observedValue.map { editorValueMatches($0, manuscript: manuscript, glyph: "") } ?? false,
        "expected_caret_utf16": manuscript.utf16.count, "expected_caret_byte": manuscript.utf8.count,
        "witness_present": current != nil, "internal_selection_present": internalSelection != nil,
        "internal_selection_available": bool(internalSelection ?? [:], "available"),
        "internal_caret_byte": integer(internalSelection ?? [:], "caret_byte_offset"),
        "pre_admission_present": preAdmission != nil,
        "lifecycle_reason": String(string(lifecycle, "reason").prefix(80)),
        "scheduled_present": preAdmission?["scheduled"] is String,
        "scheduled_kind": String(string(preAdmission ?? [:], "scheduled").prefix(80)),
        "scheduler_caret_byte": integer(preAdmission ?? [:], "caret_byte")
    ]
    if firstAdmissionObservation.isEmpty { firstAdmissionObservation = observation }
    lastAdmissionObservation = observation
    // Observe before admission as well. Otherwise a zero-run timeout conceals
    // policy/lifecycle/scope and editor state behind an always-empty witness.
    // None of these diagnostic fields can satisfy the family/render gates.
    guard observedCount == baseline + 4, let family = familyIds(), family.count == 4, let openBefore = openIds() else { reject("family_pending"); continue }
    // Diagnose terminal hydration for five bounded seconds, but NEVER turn a
    // post-terminal render into a passing pre-terminal witness.
    if !terminalSnapshot && openBefore.isEmpty && terminalDeadline == nil { terminalDeadline = ProcessInfo.processInfo.systemUptime + 5 }
    guard let editorIndex else { reject("editor_missing_or_ambiguous"); continue }
    let editor = elements[editorIndex]
    guard let current else { reject("witness_missing_or_ambiguous"); continue }
    let projection: InlineProjection
    do { projection = try inlineProjection(current, family: family, manuscript: manuscript) }
    catch { reject("projection_\((error as? Rejection)?.rawValue ?? "invalid")"); continue }
    var selection = CFRange()
    guard let raw = attribute(editor, kAXSelectedTextRangeAttribute as CFString), CFGetTypeID(raw) == AXValueGetTypeID(),
          AXValueGetType(raw as! AXValue) == .cfRange, AXValueGetValue(raw as! AXValue, .cfRange, &selection),
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
          (attribute(editor, kAXFocusedAttribute as CFString) as? Bool) == true,
          selection.location == manuscript.utf16.count, selection.length == 0,
          editorValueMatches((attribute(editor, kAXValueAttribute as CFString) as? String) ?? "", manuscript: manuscript, glyph: projection.text)
    else { reject("editor_identity_or_caret"); continue }
    guard let durable = cumulativeText(projection.runId, through: projection.sequence), durable.utf8.starts(with: projection.text.utf8) else { reject("event_prefix_mismatch"); continue }
    // Read append-only terminals AFTER the AX/DOM-derived observation. Reading
    // them only before observation cannot establish pre-terminal causality.
    guard let selectedTerminal = terminal(projection.runId), let openAfter = openIds() else { reject("terminal_store_unreadable"); continue }
    if terminalSnapshot, selectedTerminal, openAfter.isEmpty, projection.sequence == nil,
       let candidates = current["candidates"] as? [[String: Any]],
       let authority = terminalFamily(candidates, family: family), count() == baseline + 4 {
        let identity: [String: Any] = ["context_key": string(current, "context_key"), "candidates": candidates,
            "selected_run_id": projection.runId, "selected_candidate_id": projection.candidateId,
            "presentation_key": projection.key, "inline_visible_key": projection.key,
            "authority_frozen": bool(current, "authority_frozen"),
            "inline_utf8_bytes": projection.text.utf8.count, "inline_sha256": sha256(projection.text)]
        let result: [String: Any] = ["schema": "delysis.loom-terminal-render-witness.v1",
            "evidence_kind": "terminal_render_only", "identity": identity, "family_run_ids": family,
            "generation_run_count": observedCount, "terminal_candidate_authority": authority,
            "glyph_source": "observed_dom_widget", "editor_focused": true, "caret_utf16": selection.location]
        let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
        print(String(data: data, encoding: .utf8)!); exit(0)
    }
    if terminalSnapshot { reject("terminal_hydration_pending"); continue }
    if selectedTerminal || projection.sequence == nil || !openAfter.contains(projection.runId) {
        if selectedTerminal { postTerminal = ["run_id": projection.runId, "presentation_key": projection.key, "inline_utf8_bytes": projection.text.utf8.count,
                        "inline_sha256": sha256(projection.text), "selected_run_terminal": selectedTerminal,
                        "correlated_dom_render": true] }
        reject("not_preterminal"); continue
    }
    let evidence: [String: Any] = ["schema": "delysis.loom-live-stream-witness.v1", "pid": pid, "database": databasePath,
        "baseline_generation_runs": baseline, "generation_run_count": observedCount, "family_run_ids": family,
        "open_run_ids_after_accessibility": openAfter, "terminal_count_after_accessibility": 4 - openAfter.count,
        "selected_run_id": projection.runId, "selected_candidate_id": projection.candidateId, "presentation_key": projection.key,
        "stream_sequence": String(projection.sequence!), "candidate_utf8_bytes": projection.candidateBytes,
        "durable_cumulative_utf8_bytes": durable.utf8.count, "durable_cumulative_sha256": sha256(durable),
        "visible_suffix_utf8_bytes": projection.text.utf8.count, "visible_suffix_sha256": sha256(projection.text),
        "visible_suffix_source": "observed_dom_widget", "visible_suffix_is_durable_leading_projection": true,
        "selected_run_terminal_after_accessibility": false, "mode": "visual", "inline_visible_key": projection.key,
        "visual_editor_presentation_key": projection.key, "frontmost_pid": pid, "editor_focused": true,
        "caret_utf16": selection.location, "expected_caret_utf16": manuscript.utf16.count, "polls": polls,
        "rejected_stages": rejected, "observed_at_ms": Int64(Date().timeIntervalSince1970 * 1000)]
    let data = try JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
    print(String(data: data, encoding: .utf8)!); exit(0)
}
fail(postTerminal.isEmpty ? "no_correlated_inline_render" : "render_observed_only_after_terminal")
#else
fputs("native live-stream observation requires macOS; use --self-test for portable contract tests only\n", stderr)
exit(2)
#endif
