import AppKit
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
guard let runningApplication = NSRunningApplication(processIdentifier: pid) else {
    fputs("Loom exited before exact foreground activation\n", stderr)
    exit(1)
}
let application = AXUIElementCreateApplication(pid)
let deadlineUptime = ProcessInfo.processInfo.systemUptime + 30
var activationAttempts = 0
while ProcessInfo.processInfo.systemUptime < deadlineUptime {
    // LaunchServices can publish NSRunningApplication before the first native
    // window exists. Activation requested only once at that boundary is lost;
    // retry the exact PID until AppKit and Accessibility agree on ownership.
    runningApplication.unhide()
    _ = runningApplication.activate(options: [.activateAllWindows])
    _ = AXUIElementSetAttributeValue(
        application,
        kAXFrontmostAttribute as CFString,
        kCFBooleanTrue
    )
    activationAttempts += 1
    if activationAttempts % 10 == 0 {
        var frontmostError: NSDictionary?
        let frontmostSource =
            "tell application \"System Events\" to set frontmost of first application process " +
            "whose unix id is \(pid) to true"
        if let frontmostScript = NSAppleScript(source: frontmostSource) {
            _ = frontmostScript.executeAndReturnError(&frontmostError)
        }
    }
    if !runningApplication.isHidden,
       NSWorkspace.shared.frontmostApplication?.processIdentifier == pid {
        exit(0)
    }
    Thread.sleep(forTimeInterval: 0.05)
}
let frontmostPid = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1
fputs(
    "Loom's exact process did not become visible and frontmost " +
    "(hidden=\(runningApplication.isHidden), active=\(runningApplication.isActive), " +
    "terminated=\(runningApplication.isTerminated), frontmost=\(frontmostPid), " +
    "attempts=\(activationAttempts))\n",
    stderr
)
exit(1)
