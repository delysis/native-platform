import Foundation

// Read the same exact current-control contract used by the toggle driver.
// This helper is observation-only: --state never posts an input event.
guard CommandLine.arguments.count == 2,
      let pid = Int32(CommandLine.arguments[1]), pid > 0 else {
    fputs("usage: loom_completion_control_state <pid>\n", stderr)
    exit(2)
}
let helper = URL(fileURLWithPath: CommandLine.arguments[0])
    .deletingLastPathComponent().appendingPathComponent("set_loom_completion_toggle")
guard FileManager.default.isExecutableFile(atPath: helper.path) else {
    fputs("the compiled completion-control helper is missing\n", stderr)
    exit(1)
}
let query = Process()
query.executableURL = helper
query.arguments = ["--state", String(pid)]
query.standardOutput = FileHandle.standardOutput
query.standardError = FileHandle.standardError
do {
    try query.run()
    query.waitUntilExit()
    exit(query.terminationStatus)
} catch {
    fputs("could not read the completion control: \(error)\n", stderr)
    exit(1)
}
