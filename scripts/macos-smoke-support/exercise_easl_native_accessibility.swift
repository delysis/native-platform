import Foundation

struct ProbeArguments {
    let pid: Int32
    let sha256: String
    init(_ arguments: [String]) throws {
        guard arguments.count == 3,
              let pid = Int32(arguments[0]), pid > 0,
              arguments[1].count == 64,
              arguments[1].utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              arguments[2] == "--exercise-ephemeral" else { throw ProbeError.stage("arguments") }
        self.pid = pid
        self.sha256 = arguments[1]
    }
}
enum ProbeError: Error { case stage(String) }
func sameBytes(_ left: String, _ right: String) -> Bool { left.utf8.elementsEqual(right.utf8) }

// These checks run the actual argument/byte-comparison code. They are not AX evidence.
func selfTest() throws {
    var passed = 0
    func check(_ condition: Bool) throws {
        guard condition else { throw ProbeError.stage("portable-contract") }
        passed += 1
    }
    let hash = String(repeating: "a", count: 64)
    try check(try ProbeArguments(["42", hash, "--exercise-ephemeral"]).pid == 42)
    for invalid in [
        [], ["42"], ["0", hash, "--exercise-ephemeral"],
        ["-1", hash, "--exercise-ephemeral"], ["no-pid", hash, "--exercise-ephemeral"],
        ["2147483648", hash, "--exercise-ephemeral"],
        ["42", String(repeating: "a", count: 63), "--exercise-ephemeral"],
        ["42", String(repeating: "A", count: 64), "--exercise-ephemeral"],
        ["42", hash, "--force"], ["42", hash, "--exercise-ephemeral", "extra"]
    ] {
        var rejected = false
        do { _ = try ProbeArguments(invalid) } catch { rejected = true }
        try check(rejected)
    }
    try check(sameBytes("café 👨‍👩‍👧‍👦", "café 👨‍👩‍👧‍👦"))
    try check(!sameBytes("é", "e\u{301}"))
    try check(!sameBytes("line\r\n", "line\n"))
    print("{\"schema\":\"delysis.easl-ax-helper-components.v1\",\"passed\":\(passed),\"native_executed\":false}")
}

#if os(macOS)
import AppKit
import ApplicationServices
import CryptoKit

final class NativeProbe {
    let arguments: ProbeArguments
    let application: AXUIElement
    let running: NSRunningApplication
    let launched: Date
    let executable: URL
    let expected = ["First text field", "Second text field"]
    private(set) var checks = 0

    init(_ arguments: ProbeArguments) throws {
        self.arguments = arguments
        guard AXIsProcessTrusted() else { throw ProbeError.stage("accessibility-permission") }
        guard let running = NSRunningApplication(processIdentifier: arguments.pid),
              !running.isTerminated, let executable = running.executableURL,
              let launched = running.launchDate else {
            throw ProbeError.stage("exact-process")
        }
        self.running = running
        self.launched = launched
        self.executable = executable.resolvingSymlinksInPath()
        self.application = AXUIElementCreateApplication(arguments.pid)
        let data = try Data(contentsOf: executable.resolvingSymlinksInPath(), options: [.mappedIfSafe])
        guard Self.digest(data) == arguments.sha256 else { throw ProbeError.stage("executable-sha256") }
    }
    static func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
    func require(_ condition: Bool, _ stage: String) throws {
        guard condition else { throw ProbeError.stage(stage) }
        checks += 1
    }
    func owner() throws {
        guard !running.isTerminated,
              let current = NSRunningApplication(processIdentifier: arguments.pid),
              !current.isTerminated, current.launchDate == launched,
              current.executableURL?.resolvingSymlinksInPath() == executable else {
            throw ProbeError.stage("exact-process-exited-or-replaced")
        }
    }
    func attribute(_ element: AXUIElement, _ name: CFString) -> CFTypeRef? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name, &value) == .success else { return nil }
        return value
    }
    func value(_ element: AXUIElement) -> String? { attribute(element, kAXValueAttribute as CFString) as? String }
    func range(_ element: AXUIElement) -> CFRange? {
        guard let raw = attribute(element, kAXSelectedTextRangeAttribute as CFString),
              CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
        let value = raw as! AXValue
        guard AXValueGetType(value) == .cfRange else { return nil }
        var result = CFRange()
        return AXValueGetValue(value, .cfRange, &result) ? result : nil
    }
    func children(_ element: AXUIElement) throws -> [AXUIElement] {
        var raw: CFTypeRef?
        let result = AXUIElementCopyAttributeValue(element, kAXChildrenAttribute as CFString, &raw)
        if result == .attributeUnsupported || result == .noValue { return [] }
        guard result == .success, let values = raw as? [AXUIElement] else {
            throw ProbeError.stage("accessibility-tree-unreadable")
        }
        return values
    }
    func fields() throws -> [AXUIElement] {
        try owner()
        var queue = [application]
        var cursor = 0
        var found: [[AXUIElement]] = [[], []]
        while cursor < queue.count {
            guard queue.count <= 4096 else { throw ProbeError.stage("accessibility-tree-limit") }
            let element = queue[cursor]
            cursor += 1
            guard let role = attribute(element, kAXRoleAttribute as CFString) as? String else {
                throw ProbeError.stage("accessibility-tree-unreadable")
            }
            guard role != "AXWebArea" else { throw ProbeError.stage("web-area-observed") }
            if role == kAXTextAreaRole as String {
                let labels = [kAXTitleAttribute, kAXDescriptionAttribute].compactMap {
                    attribute(element, $0 as CFString) as? String
                }
                for index in 0..<2 where labels.contains(where: { sameBytes($0, expected[index]) }) {
                    found[index].append(element)
                }
            }
            let descendants = try children(element)
            guard descendants.count <= 4096 - queue.count else { throw ProbeError.stage("accessibility-tree-limit") }
            queue.append(contentsOf: descendants)
        }
        guard found.allSatisfy({ $0.count == 1 }) else { throw ProbeError.stage("unique-native-text-fields") }
        return [found[0][0], found[1][0]]
    }
    func wait(_ stage: String, _ condition: () throws -> Bool) throws {
        let deadline = ProcessInfo.processInfo.systemUptime + 8
        repeat {
            try owner()
            if try condition() { checks += 1; return }
            Thread.sleep(forTimeInterval: 0.04)
        } while ProcessInfo.processInfo.systemUptime < deadline
        throw ProbeError.stage(stage)
    }
    func focus(_ index: Int) throws {
        let fields = try fields()
        try require(AXUIElementSetAttributeValue(application, kAXFrontmostAttribute as CFString, kCFBooleanTrue) == .success, "foreground-request")
        try require(AXUIElementSetAttributeValue(fields[index], kAXFocusedAttribute as CFString, kCFBooleanTrue) == .success, "field-focus-request")
        try wait("observed-field-focus") {
            let fields = try self.fields()
            return NSWorkspace.shared.frontmostApplication?.processIdentifier == self.arguments.pid &&
                (self.attribute(fields[index], kAXFocusedAttribute as CFString) as? Bool) == true
        }
    }
    func setValue(_ index: Int, from: String, to: String, peer: String) throws {
        try focus(index)
        let fields = try fields()
        try require(value(fields[index]).map { sameBytes($0, from) } == true, "unexpected-text-before-value")
        try require(value(fields[1-index]).map { sameBytes($0, peer) } == true, "unexpected-peer-before-value")
        try require(AXUIElementSetAttributeValue(fields[index], kAXValueAttribute as CFString, to as CFString) == .success, "value-request")
        try wait("exact-value-roundtrip") {
            let fields = try self.fields()
            return self.value(fields[index]).map { sameBytes($0, to) } == true &&
                self.value(fields[1-index]).map { sameBytes($0, peer) } == true
        }
    }
    func history(_ index: Int, redo: Bool, from: String, to: String, peer: String) throws {
        try focus(index)
        let fields = try fields()
        try require(value(fields[index]).map { sameBytes($0, from) } == true, "unexpected-text-before-history")
        try require(value(fields[1-index]).map { sameBytes($0, peer) } == true, "unexpected-peer-before-history")
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 6, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 6, keyDown: false) else {
            throw ProbeError.stage("history-key-event")
        }
        let flags: CGEventFlags = redo ? [.maskCommand, .maskShift] : [.maskCommand]
        down.flags = flags
        up.flags = flags
        try owner()
        down.postToPid(arguments.pid)
        up.postToPid(arguments.pid)
        try wait("ordinary-key-history") {
            let fields = try self.fields()
            return self.value(fields[index]).map { sameBytes($0, to) } == true &&
                self.value(fields[1-index]).map { sameBytes($0, peer) } == true
        }
    }
    func closeAndObserveExit() throws {
        try owner()
        guard let windows = attribute(application, kAXWindowsAttribute as CFString) as? [AXUIElement],
              windows.count == 1,
              let rawButton = attribute(windows[0], kAXCloseButtonAttribute as CFString),
              CFGetTypeID(rawButton) == AXUIElementGetTypeID() else {
            throw ProbeError.stage("unique-native-close-button")
        }
        let button = rawButton as! AXUIElement
        var buttonOwner: pid_t = 0
        guard AXUIElementGetPid(button, &buttonOwner) == .success,
              buttonOwner == arguments.pid else { throw ProbeError.stage("native-close-owner") }
        try require(AXUIElementPerformAction(button, kAXPressAction as CFString) == .success, "native-close-request")
        let deadline = ProcessInfo.processInfo.systemUptime + 8
        while !running.isTerminated && ProcessInfo.processInfo.systemUptime < deadline {
            Thread.sleep(forTimeInterval: 0.04)
        }
        try require(running.isTerminated, "native-process-exit")
        // Termination alone is not a clean-exit certificate. The launching agent
        // must independently wait for this exact child and require exit status 0.
    }
    func run() throws {
        // Caller must open a fresh ephemeral probe. Never replace existing writing.
        var discovered: [AXUIElement]? = nil
        try wait("native-tree-activation") {
            do { discovered = try self.fields(); return true }
            catch ProbeError.stage("unique-native-text-fields") { return false }
        }
        guard let initial = discovered else { throw ProbeError.stage("native-tree-activation") }
        try require(initial.allSatisfy { value($0).map { sameBytes($0, "") } == true }, "nonempty-editor-refused")
        let first = "café 👨‍👩‍👧‍👦 e\u{301}\nfirst buffer"
        let second = "日本語 Ελληνικά العربية\nsecond buffer"
        try setValue(0, from: "", to: first, peer: "")
        try setValue(1, from: "", to: second, peer: first)
        let current = try fields()
        var selection = CFRange(location: 0, length: second.utf16.count)
        guard let encoded = AXValueCreate(.cfRange, &selection) else { throw ProbeError.stage("selection-value") }
        try require(AXUIElementSetAttributeValue(current[1], kAXSelectedTextRangeAttribute as CFString, encoded) == .success, "selection-request")
        try wait("utf16-selection-roundtrip") {
            let fields = try self.fields()
            let selection = self.range(fields[1])
            return selection?.location == 0 && selection?.length == second.utf16.count
        }
        try history(1, redo: false, from: second, to: "", peer: first)
        try history(1, redo: true, from: "", to: second, peer: first)
        let replacement = first + "\nreplacement"
        try setValue(0, from: first, to: replacement, peer: second)
        try history(0, redo: false, from: replacement, to: first, peer: second)
        try history(0, redo: false, from: first, to: "", peer: second)
        try history(0, redo: true, from: "", to: first, peer: second)
        try history(0, redo: true, from: first, to: replacement, peer: second)
        try closeAndObserveExit()
        let report: [String: Any] = [
            "schema": "delysis.easl-native-accessibility.check.v1",
            "pid": arguments.pid,
            "executable_sha256": arguments.sha256,
            "checks": checks,
            "two_unique_native_editors": true,
            "exact_utf8_roundtrips": true,
            "utf16_selection_roundtrip": true,
            "native_key_undo_redo": true,
            "peer_isolation": true,
            "native_close_and_exit_observed": true,
            "field_utf8_bytes": [replacement.utf8.count, second.utf8.count],
            "field_sha256": [Self.digest(Data(replacement.utf8)), Self.digest(Data(second.utf8))],
            "ime_exercised": false,
            "voiceover_exercised": false,
            "qualified": false
        ]
        let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
        print(String(decoding: data, as: UTF8.self))
    }
}
#endif

do {
    let arguments = Array(CommandLine.arguments.dropFirst())
    if arguments == ["--self-test"] {
        try selfTest()
    } else {
        let options = try ProbeArguments(arguments)
        #if os(macOS)
        try NativeProbe(options).run()
        #else
        _ = options
        throw ProbeError.stage("macos-required")
        #endif
    }
} catch {
    let stage: String
    if case let ProbeError.stage(value) = error { stage = value } else { stage = "native-operation" }
    let data = try! JSONSerialization.data(withJSONObject: [
        "schema": "delysis.easl-native-accessibility.failure.v1", "stage": stage, "qualified": false
    ], options: [.sortedKeys])
    fputs(String(decoding: data, as: UTF8.self) + "\n", stderr)
    exit(1)
}
