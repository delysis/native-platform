import AppKit
import ApplicationServices
import Foundation
import SQLite3

func serviceMainRunLoop(for seconds: TimeInterval = 0.05) {
    RunLoop.current.run(until: Date(timeIntervalSinceNow: seconds))
}

if CommandLine.arguments.count == 2 && CommandLine.arguments[1] == "--self-test" {
    var timerFired = false
    let timer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: false) { _ in
        timerFired = true
    }
    Thread.sleep(forTimeInterval: 0.1)
    guard !timerFired else {
        fputs("sleep-only wait unexpectedly serviced the default run loop\n", stderr)
        exit(1)
    }
    let deadline = ProcessInfo.processInfo.systemUptime + 1
    while !timerFired && ProcessInfo.processInfo.systemUptime < deadline {
        serviceMainRunLoop()
    }
    timer.invalidate()
    guard timerFired else {
        fputs("run-loop wait did not deliver its default-mode timer\n", stderr)
        exit(1)
    }
    print("idle/resume run-loop contract passed")
    exit(0)
}

// Reuse the native DOM/candidate observer instead of equating a one-word
// decoration (possibly aria-hidden) with the entire AXValue candidate buffer.
guard CommandLine.arguments.count == 8, let pid = Int32(CommandLine.arguments[1]), pid > 0,
      let baseline = Int64(CommandLine.arguments[3]), baseline >= 0, baseline <= Int64.max - 4,
      let application = NSRunningApplication(processIdentifier: pid),
      let finder = NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.finder").first
else { fputs("invalid arguments or missing exact Loom/Finder process\n", stderr); exit(2) }
let dbPath = CommandLine.arguments[2], manuscript = CommandLine.arguments[4]
let guardPaths = [CommandLine.arguments[5], CommandLine.arguments[6]].filter { !$0.isEmpty }
let diagnostic = CommandLine.arguments[7]
let manager = FileManager.default
let ax = AXUIElementCreateApplication(pid)
let observer = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
    .appendingPathComponent("start_loom_live_streaming_monitor")
var pointer: OpaquePointer?
guard sqlite3_open_v2(dbPath, &pointer, SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX, nil) == SQLITE_OK,
      let db = pointer else { fputs("cannot open isolated generation store read-only\n", stderr); exit(1) }
defer { sqlite3_close(db) }
sqlite3_busy_timeout(db, 50)
var stage = "before_idle"
var before: [String: Any] = [:], after: [String: Any] = [:]
var observerAttempt = 0
var observerStem = diagnostic + ".observer"
var lastVisibilityActions: [String: Any] = [:]

func axBoolean(_ name: CFString) -> Bool? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(ax, name, &value) == .success else { return nil }
    return value as? Bool
}

func fail(_ reason: String) -> Never {
    let frontmostPid = NSWorkspace.shared.frontmostApplication?.processIdentifier
    let result: [String: Any] = ["schema": "delysis.loom-idle-resume-failure.v1", "stage": stage,
        "reason": reason, "pid": pid, "before": before, "after": after,
        "observer_failure": observerStem + ".failure.json",
        "observer_stderr": observerStem + ".stderr.log",
        "observed_state": [
            "appkit_hidden": application.isHidden,
            "ax_hidden": axBoolean(kAXHiddenAttribute as CFString).map { $0 as Any } ?? NSNull(),
            "frontmost_pid": frontmostPid.map { Int($0) as Any } ?? NSNull(),
            "finder_pid": finder.processIdentifier,
            "application_terminated": application.isTerminated,
            "guard_paths_present": guardPaths.filter { manager.fileExists(atPath: $0) }
        ],
        "last_visibility_actions": lastVisibilityActions]
    if let data = try? JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]) {
        try? data.write(to: URL(fileURLWithPath: diagnostic), options: [.atomic])
    }
    fputs("Loom idle/resume failed at \(stage): \(reason)\n", stderr); exit(1)
}
func guardsHold() -> Bool {
    !application.isTerminated && !guardPaths.contains { manager.fileExists(atPath: $0) }
}
func rows(_ sql: String, offset: Bool = false) -> [[String]]? {
    var statement: OpaquePointer?
    guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK, let statement else { return nil }
    defer { sqlite3_finalize(statement) }
    if offset { sqlite3_bind_int64(statement, 1, baseline) }
    var result: [[String]] = []
    while true {
        let status = sqlite3_step(statement)
        if status == SQLITE_DONE { return result }
        guard status == SQLITE_ROW else { return nil }
        result.append((0..<sqlite3_column_count(statement)).map { i in
            sqlite3_column_text(statement, i).map { String(cString: $0) } ?? ""
        })
    }
}
func storeIdentity() -> [[String]]? {
    guard rows("SELECT count(*) FROM generation_runs;") == [[String(baseline + 4)]],
          let identity = rows("WITH family AS (SELECT run_id FROM generation_runs ORDER BY created_at_ms,run_id LIMIT 4 OFFSET ?1) SELECT f.run_id,t.candidate_id,c.output_blob_id FROM family f JOIN generation_terminals t ON t.run_id=f.run_id AND t.status='completed' JOIN generation_candidates c ON c.run_id=f.run_id AND c.candidate_id=t.candidate_id ORDER BY f.run_id;", offset: true),
          identity.count == 4, Set(identity.compactMap(\.first)).count == 4 else { return nil }
    return identity
}
func snapshot() -> [String: Any]? {
    guard guardsHold() else { return nil }
    observerAttempt += 1
    observerStem = diagnostic + ".observer-\(observerAttempt)"
    let stdoutPath = observerStem + ".stdout.json"
    let stderrPath = observerStem + ".stderr.log"
    _ = manager.createFile(atPath: stdoutPath, contents: nil)
    manager.createFile(atPath: stderrPath, contents: nil)
    guard let stdout = FileHandle(forWritingAtPath: stdoutPath), let stderr = FileHandle(forWritingAtPath: stderrPath) else { return nil }
    defer { try? stdout.close(); try? stderr.close() }
    let child = Process()
    child.executableURL = observer
    child.arguments = [String(pid), dbPath, String(baseline), manuscript,
        diagnostic + ".unused-stop", observerStem + ".ready", observerStem + ".failure.json",
        guardPaths.first ?? "", guardPaths.dropFirst().first ?? "", "--terminal-snapshot"]
    child.standardOutput = stdout; child.standardError = stderr
    do { try child.run() } catch { return nil }
    let deadline = ProcessInfo.processInfo.systemUptime + 8
    while child.isRunning && guardsHold() && ProcessInfo.processInfo.systemUptime < deadline { serviceMainRunLoop() }
    if child.isRunning { child.terminate(); child.waitUntilExit(); return nil } // Only this owned observer, never the app.
    child.waitUntilExit()
    guard child.terminationStatus == 0, let data = try? Data(contentsOf: URL(fileURLWithPath: stdoutPath)), data.count <= 65536,
          let result = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          result["schema"] as? String == "delysis.loom-terminal-render-witness.v1",
          result["evidence_kind"] as? String == "terminal_render_only" else { return nil }
    return result
}
func identityBytes(_ observation: [String: Any]) -> Data? {
    guard let value = observation["identity"] as? [String: Any] else { return nil }
    return try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
}
func waitSnapshot(matching expected: Data? = nil) -> [String: Any]? {
    let deadline = ProcessInfo.processInfo.systemUptime + 15
    repeat {
        guard guardsHold() else { return nil }
        if let value = snapshot(), let identity = identityBytes(value), expected == nil || identity == expected { return value }
        serviceMainRunLoop()
    } while ProcessInfo.processInfo.systemUptime < deadline
    return nil
}
func waitFor(_ predicate: () -> Bool, seconds: TimeInterval = 10) -> Bool {
    let deadline = ProcessInfo.processInfo.systemUptime + seconds
    repeat {
        guard guardsHold() else { return false }
        if predicate() { return true }
        serviceMainRunLoop()
    } while ProcessInfo.processInfo.systemUptime < deadline
    return false
}
func setVisible(_ visible: Bool) {
    let appkitVisibilityResult = visible ? application.unhide() : application.hide()
    let appkitActivationResult = visible
        ? application.activate(options: [.activateAllWindows])
        : false
    let axHiddenResult = AXUIElementSetAttributeValue(
        ax,
        kAXHiddenAttribute as CFString,
        visible ? kCFBooleanFalse : kCFBooleanTrue
    )
    // Same PID-only fallback as the original runner; never address Loom by name.
    var error: NSDictionary?
    let appleScriptResult = NSAppleScript(source: "tell application \"System Events\" to set visible of first application process whose unix id is \(pid) to \(visible ? "true" : "false")")?.executeAndReturnError(&error)
    let axFrontmostResult = visible
        ? AXUIElementSetAttributeValue(ax, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
        : .success
    lastVisibilityActions = [
        "requested_visible": visible,
        "appkit_visibility_result": appkitVisibilityResult,
        "appkit_activation_result": appkitActivationResult,
        "ax_hidden_error": axHiddenResult.rawValue,
        "ax_frontmost_error": axFrontmostResult.rawValue,
        "apple_script_returned_value": appleScriptResult != nil,
        "apple_script_error": error.map { $0.description as Any } ?? NSNull()
    ]
}
guard let stableStore = storeIdentity(), let initial = waitSnapshot(), let stableIdentity = identityBytes(initial) else { fail("no_correlated_terminal_glyph") }
before = initial
var finderError: NSDictionary?
_ = NSAppleScript(source: "tell application id \"com.apple.finder\" to activate")?.executeAndReturnError(&finderError)
guard finderError == nil, waitFor({ NSWorkspace.shared.frontmostApplication?.processIdentifier == finder.processIdentifier }) else { fail("finder_did_not_take_focus") }
setVisible(false)
guard waitFor({ application.isHidden && NSWorkspace.shared.frontmostApplication?.processIdentifier == finder.processIdentifier }) else { fail("exact_process_did_not_hide") }
stage = "idle"
let started = ProcessInfo.processInfo.systemUptime
var polls = 0
while ProcessInfo.processInfo.systemUptime - started < 75 {
    polls += 1
    guard guardsHold(), application.isHidden,
          NSWorkspace.shared.frontmostApplication?.processIdentifier == finder.processIdentifier,
          storeIdentity() == stableStore else { fail("focus_process_generation_or_terminal_identity_changed") }
    serviceMainRunLoop()
}
let elapsed = ProcessInfo.processInfo.systemUptime - started
guard guardsHold(), application.isHidden,
      NSWorkspace.shared.frontmostApplication?.processIdentifier == finder.processIdentifier,
      storeIdentity() == stableStore else { fail("final_idle_invariant_changed") }
stage = "after_resume"
setVisible(true)
var resumeAttempts = 0
guard waitFor({
    application.unhide()
    _ = application.activate(options: [.activateAllWindows])
    _ = AXUIElementSetAttributeValue(ax, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
    resumeAttempts += 1
    if resumeAttempts % 10 == 0 {
        var error: NSDictionary?
        _ = NSAppleScript(source: "tell application \"System Events\" to set frontmost of first application process whose unix id is \(pid) to true")?.executeAndReturnError(&error)
    }
    return !application.isHidden && NSWorkspace.shared.frontmostApplication?.processIdentifier == pid
}),
      let resumed = waitSnapshot(matching: stableIdentity), storeIdentity() == stableStore else { fail("cached_glyph_did_not_resynchronize") }
after = resumed
let result: [String: Any] = ["schema": "delysis.loom-idle-resume-ghost-witness.v1", "pid": pid, "database": dbPath,
    "background_pid": finder.processIdentifier, "application_hidden_during_idle": true, "loom_frontmost_during_idle": false,
    "minimum_idle_seconds": 75, "actual_idle_seconds": elapsed, "idle_polls": polls, "explicit_resume": true,
    "editor_focused_after_resume": true, "caret_utf16_after_resume": manuscript.utf16.count,
    "generation_runs_before_idle": baseline + 4, "generation_runs_after_resume": baseline + 4,
    "family_terminal_count_before_idle": 4, "family_terminal_count_after_resume": 4,
    "family_run_ids": stableStore.map { $0[0] }, "terminal_candidate_authority": stableStore,
    "before": before, "after": after, "exact_ghost_identity_resynchronized": true,
    "new_generation_started": false, "ghost_stole_editor_focus": false]
let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
