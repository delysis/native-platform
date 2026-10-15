import Foundation
import Qwen3TTS

private struct Result: Encodable {
    let samples: Int
    let sampleRate: Int
}

guard CommandLine.arguments.count == 5 else {
    fputs("usage: qwen3-tts-helper MODEL OUTPUT VOICE_DESCRIPTION TEXT\n", stderr)
    exit(2)
}
let model = URL(fileURLWithPath: CommandLine.arguments[1])
let output = URL(fileURLWithPath: CommandLine.arguments[2])
let voiceDescription = CommandLine.arguments[3]
let text = CommandLine.arguments[4]

do {
    let pipeline = try Qwen3TTSPipeline(modelPath: model)
    let samples = pipeline.generateVoiceDesign(
        text: text,
        voiceDescription: voiceDescription,
        temperature: 0.75,
        maxTokens: 2400
    )
    guard !samples.isEmpty else {
        throw NSError(domain: "MomQwen3TTSHelper", code: 3, userInfo: [NSLocalizedDescriptionKey: "synthesis returned no audio"])
    }
    try AudioSampleWriter.write(samples: samples, to: output)
    let encoded = try JSONEncoder().encode(Result(samples: samples.count, sampleRate: 24_000))
    FileHandle.standardOutput.write(encoded)
    FileHandle.standardOutput.write(Data([0x0A]))
} catch {
    fputs("qwen3-tts synthesis failed: \(error)\n", stderr)
    exit(1)
}
