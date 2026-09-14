import AppKit
import ApplicationServices
import CryptoKit
import Foundation
import SQLite3

let pid = Int32(CommandLine.arguments[1])!
let databasePath = CommandLine.arguments[2]
let baseline = Int64(CommandLine.arguments[3])!
let expectedManuscript = CommandLine.arguments[4]
let stopPath = CommandLine.arguments[5]
let readyPath = CommandLine.arguments[6]
let failurePath = CommandLine.arguments[7]
let asynchronousFailurePaths = [CommandLine.arguments[8], CommandLine.arguments[9]]
    .filter { !$0.isEmpty }
let manager = FileManager.default
let application = AXUIElementCreateApplication(pid)
guard let runningApplication = NSRunningApplication(processIdentifier: pid) else {
    fputs("Loom's exact process exited before the live-stream observer initialized\n", stderr)
    exit(1)
}

var database: OpaquePointer?
guard sqlite3_open_v2(
        databasePath,
        &database,
        SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX,
        nil
      ) == SQLITE_OK,
      let database else {
    fputs("could not open Loom's isolated generation store read-only\n", stderr)
    exit(1)
}
defer { sqlite3_close(database) }
let sqliteTransient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

func sha256(_ data: Data) -> String {
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
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

func strings(_ element: AXUIElement) -> [String] {
    [kAXValueAttribute, kAXTitleAttribute, kAXDescriptionAttribute, kAXHelpAttribute]
        .compactMap { attribute(element, $0 as CFString) as? String }
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

func string(_ object: [String: Any], _ key: String) -> String {
    object[key] as? String ?? ""
}

func integer(_ object: [String: Any], _ key: String) -> Int {
    (object[key] as? NSNumber)?.intValue ?? -1
}

func bool(_ object: [String: Any], _ key: String) -> Bool {
    object[key] as? Bool ?? false
}

func withoutTerminalLineBreaks(_ value: String) -> String {
    var normalized = value
    while normalized.last == "\n" || normalized.last == "\r" { normalized.removeLast() }
    return normalized
}

func prepare(_ sql: String) -> OpaquePointer? {
    var statement: OpaquePointer?
    guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else {
        return nil
    }
    return statement
}

func scalarInt(_ sql: String, bindOffset: Bool = false) -> Int64? {
    guard let statement = prepare(sql) else { return nil }
    defer { sqlite3_finalize(statement) }
    if bindOffset { sqlite3_bind_int64(statement, 1, baseline) }
    guard sqlite3_step(statement) == SQLITE_ROW else { return nil }
    return sqlite3_column_int64(statement, 0)
}

func generationCount() -> Int64? {
    scalarInt("SELECT count(*) FROM generation_runs;")
}

func familyRunIds() -> [String]? {
    guard let statement = prepare(
        "SELECT run_id FROM generation_runs ORDER BY created_at_ms, run_id LIMIT 4 OFFSET ?1;"
    ) else { return nil }
    defer { sqlite3_finalize(statement) }
    sqlite3_bind_int64(statement, 1, baseline)
    var runIds: [String] = []
    while sqlite3_step(statement) == SQLITE_ROW {
        guard let raw = sqlite3_column_text(statement, 0) else { return nil }
        runIds.append(String(cString: raw))
    }
    return runIds
}

func openFamilyRunIds() -> [String]? {
    guard let statement = prepare(
        "WITH family AS (SELECT run_id, created_at_ms FROM generation_runs " +
        "ORDER BY created_at_ms, run_id LIMIT 4 OFFSET ?1) " +
        "SELECT f.run_id FROM family f LEFT JOIN generation_terminals t ON t.run_id = f.run_id " +
        "WHERE t.run_id IS NULL ORDER BY f.created_at_ms, f.run_id;"
    ) else { return nil }
    defer { sqlite3_finalize(statement) }
    sqlite3_bind_int64(statement, 1, baseline)
    var runIds: [String] = []
    while sqlite3_step(statement) == SQLITE_ROW {
        guard let raw = sqlite3_column_text(statement, 0) else { return nil }
        runIds.append(String(cString: raw))
    }
    return runIds
}

func selectedRunIsTerminal(_ runId: String) -> Bool? {
    guard let statement = prepare(
        "SELECT count(*) FROM generation_terminals WHERE run_id = ?1;"
    ) else { return nil }
    defer { sqlite3_finalize(statement) }
    sqlite3_bind_text(statement, 1, runId, -1, sqliteTransient)
    guard sqlite3_step(statement) == SQLITE_ROW else { return nil }
    return sqlite3_column_int64(statement, 0) != 0
}

func cumulativeText(_ runId: String, through sequence: Int64) -> String? {
    guard let statement = prepare(
        "SELECT sequence, payload_json FROM generation_events " +
        "WHERE run_id = ?1 AND event_kind = 'text_delta' AND sequence <= ?2 " +
        "ORDER BY sequence;"
    ) else { return nil }
    defer { sqlite3_finalize(statement) }
    sqlite3_bind_text(statement, 1, runId, -1, sqliteTransient)
    sqlite3_bind_int64(statement, 2, sequence)
    var text = ""
    var exactSequenceObserved = false
    while sqlite3_step(statement) == SQLITE_ROW {
        let observedSequence = sqlite3_column_int64(statement, 0)
        guard let raw = sqlite3_column_text(statement, 1),
              let data = String(cString: raw).data(using: .utf8),
              let payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              payload["kind"] as? String == "text_delta",
              let delta = payload["text"] as? String else { return nil }
        text += delta
        if observedSequence == sequence { exactSequenceObserved = true }
    }
    return exactSequenceObserved ? text : nil
}

func fail(_ reason: String, _ detail: [String: Any] = [:]) -> Never {
    var evidence = detail
    evidence["schema"] = "delysis.loom-live-stream-failure.v1"
    evidence["pid"] = pid
    evidence["reason"] = reason
    evidence["observed_at_ms"] = Int64(Date().timeIntervalSince1970 * 1_000)
    if JSONSerialization.isValidJSONObject(evidence),
       let data = try? JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys]) {
        try? data.write(to: URL(fileURLWithPath: failurePath), options: [.atomic])
    }
    fputs("Loom live-stream witness failed: \(reason)\n", stderr)
    exit(42)
}

let expectedCount = baseline + 4
let expectedManuscriptUtf8Bytes = expectedManuscript.lengthOfBytes(using: .utf8)
let deadlineUptime = ProcessInfo.processInfo.systemUptime + 120
var polls = 0
_ = manager.createFile(atPath: readyPath, contents: Data())

while ProcessInfo.processInfo.systemUptime < deadlineUptime {
    polls += 1
    if manager.fileExists(atPath: stopPath) {
        fail("observer_stopped_before_live_witness", ["polls": polls])
    }
    if asynchronousFailurePaths.contains(where: { manager.fileExists(atPath: $0) }) {
        fail("asynchronous_guard_failed", ["polls": polls])
    }
    guard !runningApplication.isTerminated else {
        fail("exact_process_exited", ["polls": polls])
    }
    guard let count = generationCount() else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    if count > expectedCount {
        fail("unexpected_generation_run", ["generation_run_count": count, "polls": polls])
    }
    if count != expectedCount {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    guard let durableFamilyRunIds = familyRunIds(), durableFamilyRunIds.count == 4,
          Set(durableFamilyRunIds).count == 4,
          let openBeforeAccessibility = openFamilyRunIds() else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    if openBeforeAccessibility.isEmpty {
        fail("family_terminal_before_live_witness", [
            "family_run_ids": durableFamilyRunIds,
            "generation_run_count": count,
            "polls": polls
        ])
    }

    _ = runningApplication.activate(options: [.activateAllWindows])
    _ = AXUIElementSetAttributeValue(
        application,
        kAXFrontmostAttribute as CFString,
        kCFBooleanTrue
    )
    let elements = descendants()
    guard let writingSurface = elements.first(where: {
              (attribute($0, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String
          }) else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    _ = AXUIElementSetAttributeValue(
        writingSurface,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
    )
    guard let witness = elements.lazy.compactMap({ element in
              strings(element).lazy.compactMap({ value in
                  jsonObject(in: value, schema: "delysis.loom-completion-witness.v1")
              }).first
          }).first,
          string(witness, "mode") == "visual",
          bool(witness, "session_cached"),
          bool(witness, "autocomplete_enabled"),
          !bool(witness, "shuttle_enabled"),
          integer(witness, "accepted_chunk_count") == 0,
          !bool(witness, "authority_frozen") else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    let selectedRunId = string(witness, "selected_run_id")
    let selectedCandidateId = string(witness, "selected_candidate_id")
    let selectedPresentationKey = string(witness, "selected_presentation_key")
    let renderedPresentationKey = string(witness, "rendered_presentation_key")
    let inlineVisibleKey = string(witness, "inline_visible_key")
    let candidates = witness["candidates"] as? [[String: Any]] ?? []
    let visual = witness["visual"] as? [String: Any] ?? [:]
    guard !selectedRunId.isEmpty,
          durableFamilyRunIds.contains(selectedRunId),
          !selectedPresentationKey.isEmpty,
          selectedPresentationKey == renderedPresentationKey,
          selectedPresentationKey == inlineVisibleKey,
          bool(visual, "available"),
          !bool(visual, "inlineHidden"),
          !bool(visual, "fanVisible"),
          string(visual, "selectedCandidateId") == selectedCandidateId,
          string(visual, "selectedPresentationKey") == selectedPresentationKey,
          let selectedCandidate = candidates.first(where: {
              string($0, "run_id") == selectedRunId &&
                  string($0, "candidate_id") == selectedCandidateId
          }) else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    let candidateUtf8Bytes = integer(selectedCandidate, "text_utf8_bytes")
    let targetByte = integer(selectedCandidate, "target_byte")
    guard candidateUtf8Bytes > 0,
          targetByte == expectedManuscriptUtf8Bytes,
          selectedPresentationKey.hasPrefix("stream:\(selectedRunId):") else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    let sequenceSuffix = selectedPresentationKey.dropFirst("stream:\(selectedRunId):".count)
    guard let sequenceText = sequenceSuffix.split(separator: ":").first.map(String.init),
          let streamSequence = Int64(sequenceText), streamSequence >= 0 else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    if let prosePrefixMarker = selectedPresentationKey.range(of: ":prose-prefix:"),
       Int(selectedPresentationKey[prosePrefixMarker.upperBound...]) != candidateUtf8Bytes {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }

    let observedEditorValue = withoutTerminalLineBreaks(
        (attribute(writingSurface, kAXValueAttribute as CFString) as? String) ?? ""
    )
    let selection = rangeAttribute(writingSurface, kAXSelectedTextRangeAttribute as CFString)
    guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
          (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true,
          observedEditorValue.hasPrefix(expectedManuscript),
          selection?.location == expectedManuscript.utf16.count,
          selection?.length == 0 else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    let visibleSuffix = String(observedEditorValue.dropFirst(expectedManuscript.count))
    let visibleSuffixData = Data(visibleSuffix.utf8)
    guard visibleSuffixData.count == candidateUtf8Bytes,
          visibleSuffix.rangeOfCharacter(from: .whitespacesAndNewlines.inverted) != nil,
          let durableCumulativeText = cumulativeText(selectedRunId, through: streamSequence),
          durableCumulativeText.hasPrefix(visibleSuffix) else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }

    // generation_terminals is append-only. Observing no terminal only after
    // the exact AX snapshot proves this visible text existed pre-terminal;
    // sampling the table first would leave a race that could certify stale UI.
    guard let selectedTerminalAfterAccessibility = selectedRunIsTerminal(selectedRunId),
          let openAfterAccessibility = openFamilyRunIds() else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }
    guard !selectedTerminalAfterAccessibility,
          openAfterAccessibility.contains(selectedRunId) else {
        Thread.sleep(forTimeInterval: 0.05)
        continue
    }

    let durableCumulativeData = Data(durableCumulativeText.utf8)
    let evidence: [String: Any] = [
        "schema": "delysis.loom-live-stream-witness.v1",
        "pid": pid,
        "database": databasePath,
        "baseline_generation_runs": baseline,
        "generation_run_count": count,
        "family_run_ids": durableFamilyRunIds,
        "open_run_ids_after_accessibility": openAfterAccessibility,
        "terminal_count_after_accessibility": 4 - openAfterAccessibility.count,
        "selected_run_id": selectedRunId,
        "selected_candidate_id": selectedCandidateId,
        "presentation_key": selectedPresentationKey,
        "stream_sequence": sequenceText,
        "candidate_utf8_bytes": candidateUtf8Bytes,
        "durable_cumulative_utf8_bytes": durableCumulativeData.count,
        "durable_cumulative_sha256": sha256(durableCumulativeData),
        "visible_suffix_utf8_bytes": visibleSuffixData.count,
        "visible_suffix_sha256": sha256(visibleSuffixData),
        "visible_suffix_is_durable_leading_projection": true,
        "selected_run_terminal_after_accessibility": false,
        "mode": "visual",
        "inline_visible_key": inlineVisibleKey,
        "visual_editor_presentation_key": string(visual, "selectedPresentationKey"),
        "frontmost_pid": pid,
        "editor_focused": true,
        "caret_utf16": selection!.location,
        "expected_caret_utf16": expectedManuscript.utf16.count,
        "polls": polls,
        "observed_at_ms": Int64(Date().timeIntervalSince1970 * 1_000)
    ]
    let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
    print(String(data: data, encoding: .utf8)!)
    exit(0)
}

let timeoutGenerationCount = generationCount()
let timeoutOpenRunIds = openFamilyRunIds()
fail("live_witness_timeout", [
    "generation_run_count": timeoutGenerationCount ?? -1,
    "open_run_ids": timeoutOpenRunIds ?? [],
    "polls": polls
])
