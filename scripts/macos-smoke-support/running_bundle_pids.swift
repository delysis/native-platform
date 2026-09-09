import AppKit
import Foundation

let bundleIdentifier = CommandLine.arguments[1]
for application in NSRunningApplication.runningApplications(
    withBundleIdentifier: bundleIdentifier
).filter({ !$0.isTerminated }).sorted(by: {
    $0.processIdentifier < $1.processIdentifier
}) {
    print(application.processIdentifier)
}
