import Foundation

// Reuse the current matcher and one-input/one-acknowledgement driver.
// This exercises chrome only; it does not certify native generation or rendering.
guard CommandLine.arguments.count == 2,
      let pid = Int32(CommandLine.arguments[1]), pid > 0 else {
    fputs("usage: exercise_loom_completion_controls <pid>\n", stderr)
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
query.arguments = ["--exercise", String(pid)]
query.standardOutput = FileHandle.standardOutput
query.standardError = FileHandle.standardError
do {
    try query.run()
    query.waitUntilExit()
    exit(query.terminationStatus)
} catch {
    fputs("could not exercise the completion controls: \(error)\n", stderr)
    exit(1)
}
