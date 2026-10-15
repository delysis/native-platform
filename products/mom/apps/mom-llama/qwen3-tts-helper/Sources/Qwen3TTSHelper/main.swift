import Foundation
import Qwen3TTS

private struct Result: Encodable {
    let samples: Int
    let sampleRate: Int
}

@main
private enum Qwen3TTSHelper {
    static func main() async {
        guard CommandLine.arguments.count == 6 else {
            fputs("usage: qwen3-tts-helper MODEL OUTPUT REFERENCE_WAV REFERENCE_TRANSCRIPT TEXT\n", stderr)
            exit(2)
        }
        let model = URL(fileURLWithPath: CommandLine.arguments[1])
        let output = URL(fileURLWithPath: CommandLine.arguments[2])
        let reference = URL(fileURLWithPath: CommandLine.arguments[3])
        let referenceTranscript = CommandLine.arguments[4]
        let text = CommandLine.arguments[5]

        do {
            // The Base BF16 voice-cloning model is already sized for native
            // execution. Avoid the helper package's runtime module-replacement
            // quantizer, whose vendored projections are not ModuleInfo-wrapped.
            let configuration = Qwen3TTSPipelineConfiguration(applyRuntimeQuantization: false)
            let pipeline = try Qwen3TTSPipeline(modelPath: model, configuration: configuration)
            let referenceData = try Data(contentsOf: reference)
            let referenceSamples = Qwen3TTSPipeline.wavToFloatSamples(data: referenceData)
            guard !referenceSamples.isEmpty,
                  let referenceCodes = pipeline.encodeReferenceAudio(audioSamples: referenceSamples),
                  !referenceCodes.isEmpty else {
                throw NSError(domain: "MomQwen3TTSHelper", code: 3, userInfo: [NSLocalizedDescriptionKey: "reference voice could not be encoded"])
            }
            let sampleCount = try await pipeline.generateToFile(
                text: text,
                referenceTranscript: referenceTranscript,
                referenceAudioCodes: referenceCodes,
                outputURL: output,
                temperature: 0.75
            )
            guard sampleCount > 0 else {
                throw NSError(domain: "MomQwen3TTSHelper", code: 4, userInfo: [NSLocalizedDescriptionKey: "synthesis returned no audio"])
            }
            let encoded = try JSONEncoder().encode(Result(samples: sampleCount, sampleRate: 24_000))
            FileHandle.standardOutput.write(encoded)
            FileHandle.standardOutput.write(Data([0x0A]))
        } catch {
            fputs("qwen3-tts synthesis failed: \(error)\n", stderr)
            exit(1)
        }
    }
}
