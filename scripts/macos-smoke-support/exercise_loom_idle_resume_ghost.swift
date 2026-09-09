import AppKit
import ApplicationServices
import CryptoKit
import Foundation
import SQLite3

let pid = Int32(CommandLine.arguments[1])!
let databasePath = CommandLine.arguments[2]
let baseline = Int64(CommandLine.arguments[3])!
let expectedManuscript = CommandLine.arguments[4]
let asynchronousFailurePaths = [CommandLine.arguments[5], CommandLine.arguments[6]]
    .filter { !$0.isEmpty }
let identityFailurePath = CommandLine.arguments[7]
let application = AXUIElementCreateApplication(pid)
guard let runningApplication = NSRunningApplication(processIdentifier: pid) else {
    fputs("Loom's exact process exited before the idle/resume witness\n", stderr)
    exit(1)
}
guard let backgroundApplication = NSRunningApplication
        .runningApplications(withBundleIdentifier: "com.apple.finder")
        .first else {
    fputs("Finder was unavailable as the native background-focus owner\n", stderr)
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
    fputs("could not open Loom's isolated store for idle/resume evidence\n", stderr)
    exit(1)
}
defer { sqlite3_close(database) }

struct CandidateIdentity: Equatable {
    let runId: String
    let candidateId: String
    let presentationKey: String
    let targetByte: Int
    let textUtf8Bytes: Int
}

struct DurableCandidateIdentity: Equatable {
    let runId: String
    let candidateId: String
    let outputBlobId: String
}

struct GhostIdentity: Equatable {
    let contextKey: String
    let candidates: [CandidateIdentity]
    let selectedRunId: String
    let selectedCandidateId: String
    let selectedPresentationKey: String
    let inlineVisibleKey: String
    let authorityFrozen: Bool
    let visibleSuffixUtf8Bytes: Int
    let visibleSuffixSha256: String
}

var lastObservedGhostIdentity: GhostIdentity?
var lastRawGhostObservation: [String: Any]?

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

func generationCount() -> Int64? {
    guard let statement = prepare("SELECT count(*) FROM generation_runs;") else { return nil }
    defer { sqlite3_finalize(statement) }
    guard sqlite3_step(statement) == SQLITE_ROW else { return nil }
    return sqlite3_column_int64(statement, 0)
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

func familyTerminalCount() -> Int64? {
    guard let statement = prepare(
        "WITH family AS (SELECT run_id FROM generation_runs " +
        "ORDER BY created_at_ms, run_id LIMIT 4 OFFSET ?1) " +
        "SELECT count(*) FROM family JOIN generation_terminals USING (run_id);"
    ) else { return nil }
    defer { sqlite3_finalize(statement) }
    sqlite3_bind_int64(statement, 1, baseline)
    guard sqlite3_step(statement) == SQLITE_ROW else { return nil }
    return sqlite3_column_int64(statement, 0)
}

func familyTerminalCandidates() -> [DurableCandidateIdentity]? {
    guard let statement = prepare(
        "WITH family AS (SELECT run_id FROM generation_runs " +
        "ORDER BY created_at_ms, run_id LIMIT 4 OFFSET ?1) " +
        "SELECT f.run_id, t.candidate_id, c.output_blob_id FROM family f " +
        "JOIN generation_terminals t ON t.run_id = f.run_id AND t.status = 'completed' " +
        "JOIN generation_candidates c ON c.run_id = f.run_id " +
        "AND c.candidate_id = t.candidate_id ORDER BY f.run_id;"
    ) else { return nil }
    defer { sqlite3_finalize(statement) }
    sqlite3_bind_int64(statement, 1, baseline)
    var candidates: [DurableCandidateIdentity] = []
    while sqlite3_step(statement) == SQLITE_ROW {
        guard let rawRunId = sqlite3_column_text(statement, 0),
              let rawCandidateId = sqlite3_column_text(statement, 1),
              let rawOutputBlobId = sqlite3_column_text(statement, 2) else {
            return nil
        }
        candidates.append(DurableCandidateIdentity(
            runId: String(cString: rawRunId),
            candidateId: String(cString: rawCandidateId),
            outputBlobId: String(cString: rawOutputBlobId)
        ))
    }
    return candidates
}

func presentationMatchesDurableCandidate(
    _ candidate: CandidateIdentity,
    _ durable: DurableCandidateIdentity
) -> Bool {
    let base = "\(durable.candidateId):\(durable.outputBlobId)"
    return candidate.presentationKey == base ||
        candidate.presentationKey == "\(base):prose-prefix:\(candidate.textUtf8Bytes)"
}

func ghostIdentityMatchesDurableFamily(
    _ identity: GhostIdentity,
    _ durableCandidates: [DurableCandidateIdentity]
) -> Bool {
    guard durableCandidates.count == 4,
          Set(durableCandidates.map(\.runId)).count == 4,
          Set(durableCandidates.map(\.candidateId)).count == 4,
          identity.candidates.count == durableCandidates.count else {
        return false
    }
    let durableByRun = Dictionary(
        uniqueKeysWithValues: durableCandidates.map { ($0.runId, $0) }
    )
    return identity.candidates.allSatisfy { candidate in
        guard let durable = durableByRun[candidate.runId] else { return false }
        return candidate.candidateId == "run:\(candidate.runId)" &&
            !candidate.presentationKey.hasPrefix("stream:") &&
            presentationMatchesDurableCandidate(candidate, durable)
    }
}

func candidateEvidence(_ candidates: [CandidateIdentity]) -> [[String: Any]] {
    candidates.map {
        [
            "run_id": $0.runId,
            "candidate_id": $0.candidateId,
            "presentation_key": $0.presentationKey,
            "target_byte": $0.targetByte,
            "text_utf8_bytes": $0.textUtf8Bytes
        ]
    }
}

func durableCandidateEvidence(
    _ candidates: [DurableCandidateIdentity]
) -> [[String: Any]] {
    candidates.map {
        [
            "run_id": $0.runId,
            "terminal_candidate_id": $0.candidateId,
            "output_blob_id": $0.outputBlobId
        ]
    }
}

func ghostIdentityEvidence(_ identity: GhostIdentity?) -> Any {
    guard let identity else { return NSNull() }
    return [
        "context_key": identity.contextKey,
        "candidates": candidateEvidence(identity.candidates),
        "selected_run_id": identity.selectedRunId,
        "selected_candidate_id": identity.selectedCandidateId,
        "selected_presentation_key": identity.selectedPresentationKey,
        "inline_visible_key": identity.inlineVisibleKey,
        "authority_frozen": identity.authorityFrozen,
        "visible_suffix_utf8_bytes": identity.visibleSuffixUtf8Bytes,
        "visible_suffix_sha256": identity.visibleSuffixSha256
    ] as [String: Any]
}

func jsonValue(_ value: Any?) -> Any {
    value ?? NSNull()
}

func completionWitnessCore(_ witness: [String: Any]) -> [String: Any] {
    let visual = witness["visual"] as? [String: Any] ?? [:]
    let editorSelection = witness["editor_selection"] as? [String: Any] ?? [:]
    let candidates = (witness["candidates"] as? [[String: Any]] ?? []).map {
        [
            "run_id": string($0, "run_id"),
            "candidate_id": string($0, "candidate_id"),
            "presentation_key": string($0, "presentation_key"),
            "target_byte": integer($0, "target_byte"),
            "text_utf8_bytes": integer($0, "text_utf8_bytes")
        ] as [String: Any]
    }
    return [
        "schema": string(witness, "schema"),
        "mode": string(witness, "mode"),
        "context_key": string(witness, "context_key"),
        "session_cached": bool(witness, "session_cached"),
        "family_count": integer(witness, "family_count"),
        "candidates": candidates,
        "selected_run_id": string(witness, "selected_run_id"),
        "selected_candidate_id": string(witness, "selected_candidate_id"),
        "selected_presentation_key": string(witness, "selected_presentation_key"),
        "rendered_presentation_key": string(witness, "rendered_presentation_key"),
        "inline_visible_key": string(witness, "inline_visible_key"),
        "accepted_chunk_count": integer(witness, "accepted_chunk_count"),
        "authority_frozen": bool(witness, "authority_frozen"),
        "autocomplete_enabled": bool(witness, "autocomplete_enabled"),
        "shuttle_enabled": bool(witness, "shuttle_enabled"),
        "inline_hidden_requested": bool(witness, "inline_hidden_requested"),
        "visual": [
            "available": bool(visual, "available"),
            "option_held": bool(visual, "optionHeld"),
            "fan_visible": bool(visual, "fanVisible"),
            "inline_hidden": bool(visual, "inlineHidden"),
            "selected_candidate_id": string(visual, "selectedCandidateId"),
            "selected_presentation_key": string(visual, "selectedPresentationKey"),
            "alternative_candidate_ids": visual["alternativeCandidateIds"] as? [String] ?? [],
            "alternative_presentation_keys": visual["alternativePresentationKeys"] as? [String] ?? [],
            "alternative_run_ids": visual["alternativeRunIds"] as? [String] ?? []
        ] as [String: Any],
        "editor_selection": [
            "available": bool(editorSelection, "available"),
            "epoch": integer(editorSelection, "epoch"),
            "selection_kind": string(editorSelection, "selection_kind"),
            "from": integer(editorSelection, "from"),
            "to": integer(editorSelection, "to"),
            "empty": bool(editorSelection, "empty"),
            "all_visible_text": bool(editorSelection, "all_visible_text"),
            "caret_at_end": bool(editorSelection, "caret_at_end"),
            "caret_byte_offset": integer(editorSelection, "caret_byte_offset")
        ] as [String: Any]
    ]
}

func rawGhostObservation() -> [String: Any] {
    let elements = descendants()
    let writingSurface = elements.first(where: {
        (attribute($0, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String
    })
    let schema = "delysis.loom-completion-witness.v1"
    let schemaBearingValues = elements.flatMap(strings).filter {
        $0.contains("\"schema\":\"\(schema)\"")
    }
    let witness = schemaBearingValues.lazy.compactMap {
        jsonObject(in: $0, schema: schema)
    }.first
    let frontmostPid = NSWorkspace.shared.frontmostApplication?.processIdentifier
    var observation: [String: Any] = [
        "expected_pid": pid,
        "frontmost_pid": jsonValue(frontmostPid),
        "frontmost_matches_expected": frontmostPid == pid,
        "application_hidden": runningApplication.isHidden,
        "application_active": runningApplication.isActive,
        "application_terminated": runningApplication.isTerminated,
        "ax_application_frontmost": jsonValue(
            attribute(application, kAXFrontmostAttribute as CFString) as? Bool
        ),
        "writing_surface_present": writingSurface != nil,
        "completion_witness_text_found": !schemaBearingValues.isEmpty,
        "completion_witness_parsed": witness != nil,
        "completion_witness": witness.map(completionWitnessCore) ?? NSNull()
    ]
    guard let writingSurface else {
        observation["writing_surface"] = NSNull()
        return observation
    }
    let focused = attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool
    let selectedRange = rangeAttribute(
        writingSurface,
        kAXSelectedTextRangeAttribute as CFString
    )
    let value = attribute(writingSurface, kAXValueAttribute as CFString) as? String
    var writingSurfaceEvidence: [String: Any] = [
        "focused": jsonValue(focused),
        "selected_text_range": selectedRange.map {
            ["location": $0.location, "length": $0.length] as [String: Any]
        } ?? NSNull(),
        "selection_matches_expected": selectedRange?.location == expectedManuscript.utf16.count &&
            selectedRange?.length == 0,
        "ax_value_available": value != nil
    ]
    if let value {
        let valueData = Data(value.utf8)
        writingSurfaceEvidence["ax_value_utf16_length"] = value.utf16.count
        writingSurfaceEvidence["ax_value_utf8_bytes"] = valueData.count
        writingSurfaceEvidence["ax_value_sha256"] = sha256(valueData)
        writingSurfaceEvidence["ax_value_has_expected_manuscript_prefix"] =
            value.hasPrefix(expectedManuscript)
        if value.hasPrefix(expectedManuscript) {
            let suffix = String(value.dropFirst(expectedManuscript.count))
            let suffixData = Data(suffix.utf8)
            writingSurfaceEvidence["ax_value_suffix_utf8_bytes"] = suffixData.count
            writingSurfaceEvidence["ax_value_suffix_sha256"] = sha256(suffixData)
            writingSurfaceEvidence["ax_value_suffix_nonblank"] =
                suffix.rangeOfCharacter(from: .whitespacesAndNewlines.inverted) != nil
        }
    }
    observation["writing_surface"] = writingSurfaceEvidence
    return observation
}

func reportGhostIdentityFailure(
    stage: String,
    before: GhostIdentity?,
    beforeRaw: [String: Any]?,
    lastObserved: GhostIdentity?,
    lastRaw: [String: Any]?,
    durableCandidates: [DurableCandidateIdentity]
) {
    let diagnostic: [String: Any] = [
        "schema": "delysis.loom-idle-resume-ghost-failure.v1",
        "stage": stage,
        "before_identity": ghostIdentityEvidence(before),
        "before_raw_observation": beforeRaw ?? NSNull(),
        "last_observed_identity": ghostIdentityEvidence(lastObserved),
        "last_raw_observation": lastRaw ?? NSNull(),
        "terminal_candidate_authority": durableCandidateEvidence(durableCandidates)
    ]
    if let data = try? JSONSerialization.data(withJSONObject: diagnostic, options: [.sortedKeys]),
       let encoded = String(data: data, encoding: .utf8) {
        if !identityFailurePath.isEmpty {
            try? data.write(to: URL(fileURLWithPath: identityFailurePath), options: .atomic)
        }
        fputs("idle/resume ghost diagnostic: \(encoded)\n", stderr)
    }
}

func asynchronousGuardFailed() -> Bool {
    asynchronousFailurePaths.contains { FileManager.default.fileExists(atPath: $0) }
}

func restoreExactEditorFocus() -> Bool {
    _ = runningApplication.activate(options: [.activateAllWindows])
    guard AXUIElementSetAttributeValue(
            application,
            kAXFrontmostAttribute as CFString,
            kCFBooleanTrue
          ) == .success else { return false }
    guard let writingSurface = descendants().first(where: {
              (attribute($0, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String
          }),
          AXUIElementSetAttributeValue(
              writingSurface,
              kAXFocusedAttribute as CFString,
              kCFBooleanTrue
          ) == .success else { return false }
    return true
}

func resumeExactApplication() -> String? {
    runningApplication.unhide()
    var dispatch = "NSRunningApplication.unhide"
    let accessibilityUnhide = AXUIElementSetAttributeValue(
        application,
        kAXHiddenAttribute as CFString,
        kCFBooleanFalse
    )
    if accessibilityUnhide == .success {
        dispatch += " then PID-addressed AXHidden=false"
    }
    let accessibilityDeadline = ProcessInfo.processInfo.systemUptime + 0.5
    while runningApplication.isHidden &&
        ProcessInfo.processInfo.systemUptime < accessibilityDeadline {
        Thread.sleep(forTimeInterval: 0.05)
    }
    if runningApplication.isHidden {
        var visibilityError: NSDictionary?
        let visibilitySource =
            "tell application \"System Events\" to set visible of first application process " +
            "whose unix id is \(pid) to true"
        guard let visibilityScript = NSAppleScript(source: visibilitySource) else { return nil }
        _ = visibilityScript.executeAndReturnError(&visibilityError)
        guard visibilityError == nil else { return nil }
        dispatch += " then exact-PID System Events visible=true"
    }

    let foregroundDeadline = ProcessInfo.processInfo.systemUptime + 10
    var attempts = 0
    while ProcessInfo.processInfo.systemUptime < foregroundDeadline {
        runningApplication.unhide()
        _ = runningApplication.activate(options: [.activateAllWindows])
        _ = AXUIElementSetAttributeValue(
            application,
            kAXFrontmostAttribute as CFString,
            kCFBooleanTrue
        )
        attempts += 1
        if attempts % 10 == 0 {
            var frontmostError: NSDictionary?
            let frontmostSource =
                "tell application \"System Events\" to set frontmost of first application process " +
                "whose unix id is \(pid) to true"
            if let frontmostScript = NSAppleScript(source: frontmostSource) {
                _ = frontmostScript.executeAndReturnError(&frontmostError)
            }
        }
        if !runningApplication.isHidden,
           NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
           restoreExactEditorFocus() {
            return dispatch
        }
        Thread.sleep(forTimeInterval: 0.05)
    }
    return nil
}

func currentGhostIdentity() -> GhostIdentity? {
    let elements = descendants()
    guard let writingSurface = elements.first(where: {
              (attribute($0, kAXRoleAttribute as CFString) as? String) == kAXTextAreaRole as String
          }),
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
          (attribute(writingSurface, kAXFocusedAttribute as CFString) as? Bool) == true,
          let selection = rangeAttribute(writingSurface, kAXSelectedTextRangeAttribute as CFString),
          selection.location == expectedManuscript.utf16.count,
          selection.length == 0,
          let witness = elements.lazy.compactMap({ element in
              strings(element).lazy.compactMap({ value in
                  jsonObject(in: value, schema: "delysis.loom-completion-witness.v1")
              }).first
          }).first,
          string(witness, "mode") == "visual",
          bool(witness, "session_cached"),
          integer(witness, "family_count") == 4,
          integer(witness, "accepted_chunk_count") == 0,
          bool(witness, "autocomplete_enabled"),
          !bool(witness, "shuttle_enabled") else {
        return nil
    }
    let selectedRunId = string(witness, "selected_run_id")
    let selectedCandidateId = string(witness, "selected_candidate_id")
    let selectedPresentationKey = string(witness, "selected_presentation_key")
    let renderedPresentationKey = string(witness, "rendered_presentation_key")
    let inlineVisibleKey = string(witness, "inline_visible_key")
    let visual = witness["visual"] as? [String: Any] ?? [:]
    let candidates = (witness["candidates"] as? [[String: Any]] ?? []).map {
        CandidateIdentity(
            runId: string($0, "run_id"),
            candidateId: string($0, "candidate_id"),
            presentationKey: string($0, "presentation_key"),
            targetByte: integer($0, "target_byte"),
            textUtf8Bytes: integer($0, "text_utf8_bytes")
        )
    }
    guard candidates.count == 4,
          Set(candidates.map(\.runId)).count == 4,
          candidates.allSatisfy({ candidate in
              !candidate.runId.isEmpty &&
                  !candidate.candidateId.isEmpty &&
                  !candidate.presentationKey.isEmpty &&
                  candidate.targetByte == expectedManuscript.lengthOfBytes(using: .utf8) &&
                  candidate.textUtf8Bytes > 0
          }),
          let selected = candidates.first(where: {
              $0.runId == selectedRunId && $0.candidateId == selectedCandidateId
          }),
          selected.textUtf8Bytes > 0,
          !selectedPresentationKey.isEmpty,
          selected.presentationKey == selectedPresentationKey,
          selectedPresentationKey == renderedPresentationKey,
          selectedPresentationKey == inlineVisibleKey,
          bool(visual, "available"),
          !bool(visual, "inlineHidden"),
          !bool(visual, "fanVisible"),
          string(visual, "selectedCandidateId") == selectedCandidateId,
          string(visual, "selectedPresentationKey") == selectedPresentationKey else {
        return nil
    }
    let observed = withoutTerminalLineBreaks(
        (attribute(writingSurface, kAXValueAttribute as CFString) as? String) ?? ""
    )
    guard observed.hasPrefix(expectedManuscript) else { return nil }
    let visibleSuffix = String(observed.dropFirst(expectedManuscript.count))
    let visibleSuffixData = Data(visibleSuffix.utf8)
    guard visibleSuffixData.count == selected.textUtf8Bytes,
          visibleSuffix.rangeOfCharacter(from: .whitespacesAndNewlines.inverted) != nil else {
        return nil
    }
    return GhostIdentity(
        contextKey: string(witness, "context_key"),
        candidates: candidates,
        selectedRunId: selectedRunId,
        selectedCandidateId: selectedCandidateId,
        selectedPresentationKey: selectedPresentationKey,
        inlineVisibleKey: inlineVisibleKey,
        authorityFrozen: bool(witness, "authority_frozen"),
        visibleSuffixUtf8Bytes: visibleSuffixData.count,
        visibleSuffixSha256: sha256(visibleSuffixData)
    )
}

func waitForGhostIdentity(
    timeout: TimeInterval,
    durableCandidates: [DurableCandidateIdentity],
    expected: GhostIdentity? = nil
) -> GhostIdentity? {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if asynchronousGuardFailed() || runningApplication.isTerminated { return nil }
        _ = restoreExactEditorFocus()
        let observed = currentGhostIdentity()
        lastRawGhostObservation = rawGhostObservation()
        if let observed {
            lastObservedGhostIdentity = observed
            if ghostIdentityMatchesDurableFamily(observed, durableCandidates) &&
               (expected == nil || observed == expected) {
                return observed
            }
        }
        Thread.sleep(forTimeInterval: 0.05)
    } while Date() < deadline
    return nil
}

let expectedGenerationCount = baseline + 4
guard generationCount() == expectedGenerationCount,
      familyTerminalCount() == 4,
      let durableFamilyRunIds = familyRunIds(),
      durableFamilyRunIds.count == 4,
      Set(durableFamilyRunIds).count == 4 else {
    fputs("Loom did not durably complete one exact family before native idle\n", stderr)
    exit(1)
}
guard let durableFamilyCandidates = familyTerminalCandidates(),
      durableFamilyCandidates.count == 4,
      Set(durableFamilyCandidates.map(\.runId)) == Set(durableFamilyRunIds),
      Set(durableFamilyCandidates.map(\.candidateId)).count == 4 else {
    fputs("Loom did not expose four exact terminal candidate authorities before native idle\n", stderr)
    exit(1)
}
lastObservedGhostIdentity = nil
lastRawGhostObservation = nil
guard let before = waitForGhostIdentity(
        timeout: 15,
        durableCandidates: durableFamilyCandidates
      ),
      !before.contextKey.isEmpty,
      Set(before.candidates.map(\.runId)) == Set(durableFamilyRunIds) else {
    reportGhostIdentityFailure(
        stage: "before_idle",
        before: nil,
        beforeRaw: nil,
        lastObserved: lastObservedGhostIdentity,
        lastRaw: lastRawGhostObservation,
        durableCandidates: durableFamilyCandidates
    )
    fputs("Loom did not expose one exact terminal cached ghost before native idle\n", stderr)
    exit(1)
}
let beforeRawGhostObservation = lastRawGhostObservation

var backgroundActivationError: NSDictionary?
guard let backgroundActivation = NSAppleScript(
        source: "tell application id \"com.apple.finder\" to activate"
      ) else {
    fputs("could not construct Finder activation for the native idle interval\n", stderr)
    exit(1)
}
_ = backgroundActivation.executeAndReturnError(&backgroundActivationError)
guard backgroundActivationError == nil else {
    fputs("could not activate Finder as the native idle focus owner\n", stderr)
    exit(1)
}
let backgroundActivationDeadlineUptime = ProcessInfo.processInfo.systemUptime + 10
while NSWorkspace.shared.frontmostApplication?.processIdentifier !=
    backgroundApplication.processIdentifier &&
    ProcessInfo.processInfo.systemUptime < backgroundActivationDeadlineUptime {
    Thread.sleep(forTimeInterval: 0.05)
}
guard NSWorkspace.shared.frontmostApplication?.processIdentifier ==
        backgroundApplication.processIdentifier else {
    fputs("Finder never became the exact native idle focus owner\n", stderr)
    exit(1)
}

let nativeHideAccepted = runningApplication.hide()
var hideDispatch = "NSRunningApplication.hide"
let nativeHideDeadlineUptime = ProcessInfo.processInfo.systemUptime + 1
while !runningApplication.isHidden &&
    ProcessInfo.processInfo.systemUptime < nativeHideDeadlineUptime {
    Thread.sleep(forTimeInterval: 0.05)
}
if !runningApplication.isHidden {
    let hideResult = AXUIElementSetAttributeValue(
        application,
        kAXHiddenAttribute as CFString,
        kCFBooleanTrue
    )
    guard hideResult == .success else {
        fputs("could not hide Loom's exact process for the native idle interval\n", stderr)
        exit(1)
    }
    hideDispatch = nativeHideAccepted
        ? "NSRunningApplication.hide then PID-addressed AXHidden"
        : "PID-addressed AXHidden"
}
let accessibilityHideDeadlineUptime = ProcessInfo.processInfo.systemUptime + 0.5
while !runningApplication.isHidden &&
    ProcessInfo.processInfo.systemUptime < accessibilityHideDeadlineUptime {
    Thread.sleep(forTimeInterval: 0.05)
}
if !runningApplication.isHidden {
    var visibilityError: NSDictionary?
    let visibilitySource =
        "tell application \"System Events\" to set visible of first application process " +
        "whose unix id is \(pid) to false"
    guard let visibilityScript = NSAppleScript(source: visibilitySource) else {
        fputs("could not construct exact-PID Loom visibility mutation\n", stderr)
        exit(1)
    }
    _ = visibilityScript.executeAndReturnError(&visibilityError)
    guard visibilityError == nil else {
        fputs("could not hide Loom through its exact System Events process\n", stderr)
        exit(1)
    }
    hideDispatch += " then exact-PID System Events visible=false"
}
let backgroundDeadlineUptime = ProcessInfo.processInfo.systemUptime + 10
while (
    !runningApplication.isHidden ||
    NSWorkspace.shared.frontmostApplication?.processIdentifier == pid
) && ProcessInfo.processInfo.systemUptime < backgroundDeadlineUptime {
    Thread.sleep(forTimeInterval: 0.05)
}
guard runningApplication.isHidden,
      NSWorkspace.shared.frontmostApplication?.processIdentifier ==
        backgroundApplication.processIdentifier else {
    let frontmostPid = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1
    fputs(
        "Loom's exact process never entered the hidden background state " +
        "(hidden=\(runningApplication.isHidden), active=\(runningApplication.isActive), " +
        "frontmost=\(frontmostPid), finder=\(backgroundApplication.processIdentifier), " +
        "dispatch=\(hideDispatch))\n",
        stderr
    )
    exit(1)
}

// Cross a full minute hidden so the witness covers WebKit's delayed
// background throttling/suspension boundary, not merely an immediate
// blur/visibility round trip.
let minimumIdleSeconds: TimeInterval = 75
let idleStartedAtUptime = ProcessInfo.processInfo.systemUptime
var idlePolls = 0
while ProcessInfo.processInfo.systemUptime - idleStartedAtUptime < minimumIdleSeconds {
    idlePolls += 1
    guard !asynchronousGuardFailed(),
          !runningApplication.isTerminated,
          runningApplication.isHidden,
          NSWorkspace.shared.frontmostApplication?.processIdentifier ==
            backgroundApplication.processIdentifier,
          generationCount() == expectedGenerationCount,
          familyTerminalCount() == 4,
          familyTerminalCandidates() == durableFamilyCandidates else {
        fputs("Loom stole focus, exited, or generated again during native idle\n", stderr)
        exit(1)
    }
    Thread.sleep(forTimeInterval: 0.05)
}
let actualIdleSeconds = ProcessInfo.processInfo.systemUptime - idleStartedAtUptime

guard let resumeDispatch = resumeExactApplication() else {
    reportGhostIdentityFailure(
        stage: "native_resume",
        before: before,
        beforeRaw: beforeRawGhostObservation,
        lastObserved: nil,
        lastRaw: rawGhostObservation(),
        durableCandidates: durableFamilyCandidates
    )
    fputs("Loom's exact process never became visible and frontmost after native idle\n", stderr)
    exit(1)
}
lastObservedGhostIdentity = nil
lastRawGhostObservation = nil
guard let after = waitForGhostIdentity(
        timeout: 15,
        durableCandidates: durableFamilyCandidates,
        expected: before
      ),
      generationCount() == expectedGenerationCount,
      familyTerminalCount() == 4,
      familyTerminalCandidates() == durableFamilyCandidates else {
    reportGhostIdentityFailure(
        stage: "after_resume",
        before: before,
        beforeRaw: beforeRawGhostObservation,
        lastObserved: lastObservedGhostIdentity,
        lastRaw: lastRawGhostObservation,
        durableCandidates: durableFamilyCandidates
    )
    fputs("the exact cached WYSIWYG ghost did not resynchronize after native idle\n", stderr)
    exit(1)
}

let exactCandidateEvidence = candidateEvidence(after.candidates)
let evidence: [String: Any] = [
    "schema": "delysis.loom-idle-resume-ghost-witness.v1",
    "pid": pid,
    "database": databasePath,
    "background_pid": backgroundApplication.processIdentifier,
    "background_bundle_id": backgroundApplication.bundleIdentifier ?? "",
    "background_frontmost_pid_during_idle": backgroundApplication.processIdentifier,
    "background_activation_dispatch": "Finder Apple event",
    "hide_dispatch": hideDispatch,
    "resume_dispatch": resumeDispatch,
    "application_hidden_during_idle": true,
    "loom_frontmost_during_idle": false,
    "minimum_idle_seconds": minimumIdleSeconds,
    "actual_idle_seconds": actualIdleSeconds,
    "idle_polls": idlePolls,
    "explicit_resume": true,
    "editor_focused_after_resume": true,
    "caret_utf16_after_resume": expectedManuscript.utf16.count,
    "generation_runs_before_idle": expectedGenerationCount,
    "generation_runs_after_resume": expectedGenerationCount,
    "family_terminal_count_before_idle": 4,
    "family_terminal_count_after_resume": 4,
    "family_run_ids": durableFamilyRunIds,
    "context_key_before": before.contextKey,
    "context_key_after": after.contextKey,
    "selected_run_id_before": before.selectedRunId,
    "selected_run_id_after": after.selectedRunId,
    "presentation_key_before": before.selectedPresentationKey,
    "presentation_key_after": after.selectedPresentationKey,
    "inline_visible_key_before": before.inlineVisibleKey,
    "inline_visible_key_after": after.inlineVisibleKey,
    "authority_frozen_before": before.authorityFrozen,
    "authority_frozen_after": after.authorityFrozen,
    "visible_suffix_utf8_bytes_before": before.visibleSuffixUtf8Bytes,
    "visible_suffix_utf8_bytes_after": after.visibleSuffixUtf8Bytes,
    "visible_suffix_sha256_before": before.visibleSuffixSha256,
    "visible_suffix_sha256_after": after.visibleSuffixSha256,
    "candidate_identity_before_and_after": exactCandidateEvidence,
    "terminal_candidate_authority": durableCandidateEvidence(durableFamilyCandidates),
    "exact_ghost_identity_resynchronized": true,
    "new_generation_started": false,
    "ghost_stole_editor_focus": false
]
let data = try! JSONSerialization.data(withJSONObject: evidence, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
