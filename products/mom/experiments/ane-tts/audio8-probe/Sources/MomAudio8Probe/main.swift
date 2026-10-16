import CoreAIKit
import CoreAIKitVision
import Foundation

private actor ChunkTimes {
    private var values: [Double] = []

    func append(_ value: Double) {
        values.append(value)
    }

    func snapshot() -> [Double] {
        values
    }
}

@main
@available(macOS 27, *)
struct MomAudio8Probe {
    static func main() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let root = environment["AUDIO8_ROOT"] else {
            fatalError("AUDIO8_ROOT is required")
        }
        let preference = environment["AUDIO8_COMPUTE"] ?? "gpu"
        let computeUnits: GraphModel.ComputeUnits = switch preference {
        case "ane": .neuralEngine
        case "cpu": .cpuOnly
        default: .gpu
        }
        let synthesizer = try await Audio8TTS(
            paths: .standard(root: URL(fileURLWithPath: root)),
            computeUnits: computeUnits
        )
        synthesizer.chunkFrames = Int(environment["AUDIO8_CHUNK_FRAMES"] ?? "32") ?? 32
        let text = environment["AUDIO8_TEXT"]
            ?? "The meeting has been moved to Thursday afternoon."
        let maxFrames = Int(environment["AUDIO8_MAX_FRAMES"] ?? "160")
        let chunkTimes = ChunkTimes()
        let start = ContinuousClock.now
        let stats = try await synthesizer.synthesizeStreaming(
            text,
            seed: 7,
            maxFrames: maxFrames
        ) { _ in
            let elapsed = start.duration(to: .now)
            let seconds = Double(elapsed.components.seconds)
                + Double(elapsed.components.attoseconds) / 1e18
            await chunkTimes.append(seconds)
        }
        let output: [String: Any] = [
            "compute_preference": preference,
            "load_s": synthesizer.loadSeconds,
            "prompt_tokens": stats.promptTokens,
            "frames": stats.frames,
            "audio_s": stats.audioSeconds,
            "wall_s": stats.wallSeconds,
            "first_audio_s": stats.firstAudioSeconds,
            "prefill_s": stats.prefillSeconds,
            "frame_s": stats.frameSeconds,
            "codec_s": stats.codecSeconds,
            "rtf": stats.rtf,
            "ended_with_eos": stats.endedWithEOS,
            "engine_calls": stats.engineCalls,
            "chunk_times_s": await chunkTimes.snapshot(),
        ]
        let data = try JSONSerialization.data(
            withJSONObject: output,
            options: [.prettyPrinted, .sortedKeys]
        )
        print(String(decoding: data, as: UTF8.self))
    }
}
