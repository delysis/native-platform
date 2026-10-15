import Foundation
import Qwen3TTS

private struct Result: Encodable {
    let samples: Int
    let sampleRate: Int
}

private struct Request: Decodable {
    let id: String
    let text: String
    let ref_audio: String
    let ref_text: String
}

private struct WireEvent: Encodable {
    let event: String
    var id: String? = nil
    var sample_rate: Int? = nil
    var samples: Int? = nil
    var data: String? = nil
    var detail: String? = nil
}

@main
private enum Qwen3TTSHelperMain {
    static func main() async {
        do {
            let arguments = Array(CommandLine.arguments.dropFirst())
            if arguments.first == "--resident", arguments.count == 2 {
                try await runResident(modelPath: URL(fileURLWithPath: arguments[1]))
            } else if arguments.first == "--icl", arguments.count == 6 {
                try await runICL(
                    modelPath: URL(fileURLWithPath: arguments[1]),
                    referencePath: URL(fileURLWithPath: arguments[2]),
                    transcript: arguments[3],
                    outputPath: URL(fileURLWithPath: arguments[4]),
                    text: arguments[5]
                )
            } else if arguments.count == 4 {
                try runVoiceDesign(arguments: arguments)
            } else {
                fputs("usage: qwen3-tts-helper MODEL OUTPUT VOICE_DESCRIPTION TEXT\n", stderr)
                fputs("       qwen3-tts-helper --icl MODEL REF_AUDIO REF_TEXT OUTPUT TEXT\n", stderr)
                fputs("       qwen3-tts-helper --resident MODEL\n", stderr)
                exit(2)
            }
        } catch {
            fputs("qwen3-tts synthesis failed: \(error)\n", stderr)
            exit(1)
        }
    }

    private static func runVoiceDesign(arguments: [String]) throws {
        let pipeline = try Qwen3TTSPipeline(modelPath: URL(fileURLWithPath: arguments[0]))
        let samples = pipeline.generateVoiceDesign(
            text: arguments[3],
            voiceDescription: arguments[2],
            temperature: 0.75,
            maxTokens: 2400
        )
        guard !samples.isEmpty else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 3,
                          userInfo: [NSLocalizedDescriptionKey: "synthesis returned no audio"])
        }
        try AudioSampleWriter.write(samples: samples, to: URL(fileURLWithPath: arguments[1]))
        writeJSON(Result(samples: samples.count, sampleRate: Qwen3TTSPipeline.sampleRate))
    }

    private static func runICL(
        modelPath: URL,
        referencePath: URL,
        transcript: String,
        outputPath: URL,
        text: String
    ) async throws {
        let pipeline = try Qwen3TTSPipeline(modelPath: modelPath)
        guard pipeline.supportsICL else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 4,
                          userInfo: [NSLocalizedDescriptionKey: "Base ICL audio encoder is unavailable"])
        }
        let codes = try referenceCodes(pipeline: pipeline, path: referencePath)
        let count = try await pipeline.generateToFile(
            text: text,
            referenceTranscript: transcript,
            referenceAudioCodes: codes,
            outputURL: outputPath
        )
        guard count > 0 else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 5,
                          userInfo: [NSLocalizedDescriptionKey: "ICL synthesis returned no audio"])
        }
        writeJSON(Result(samples: count, sampleRate: Qwen3TTSPipeline.sampleRate))
    }

    private static func runResident(modelPath: URL) async throws {
        let pipeline = try Qwen3TTSPipeline(modelPath: modelPath)
        guard pipeline.supportsICL else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 4,
                          userInfo: [NSLocalizedDescriptionKey: "Base ICL audio encoder is unavailable"])
        }
        writeJSON(WireEvent(event: "ready", sample_rate: Qwen3TTSPipeline.sampleRate))

        while let line = readLine() {
            guard let data = line.data(using: .utf8) else { continue }
            let request: Request
            do {
                request = try JSONDecoder().decode(Request.self, from: data)
            } catch {
                writeJSON(WireEvent(event: "fatal", detail: "Invalid request JSON"))
                continue
            }
            do {
                let codes = try referenceCodes(pipeline: pipeline, path: URL(fileURLWithPath: request.ref_audio))
                let stream = pipeline.generateStream(
                    text: request.text,
                    referenceTranscript: request.ref_text,
                    referenceAudioCodes: codes,
                    chunkSize: 5
                )
                for try await chunk in stream {
                    guard !chunk.samples.isEmpty else { continue }
                    let pcm = pcm16LE(chunk.samples)
                    writeJSON(WireEvent(
                        event: "audio",
                        id: request.id,
                        sample_rate: Qwen3TTSPipeline.sampleRate,
                        samples: chunk.samples.count,
                        data: pcm.base64EncodedString()
                    ))
                }
                writeJSON(WireEvent(event: "done", id: request.id))
            } catch {
                writeJSON(WireEvent(event: "error", id: request.id, detail: String(describing: error)))
            }
        }
    }

    private static func referenceCodes(pipeline: Qwen3TTSPipeline, path: URL) throws -> [[Int32]] {
        let data = try Data(contentsOf: path)
        guard data.count >= 44,
              data.prefix(4) == Data("RIFF".utf8),
              data.subdata(in: 8..<12) == Data("WAVE".utf8),
              data[22] == 1, data[23] == 0,
              data[24] == 0xC0, data[25] == 0x5D,
              data[34] == 16, data[35] == 0 else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 6,
                          userInfo: [NSLocalizedDescriptionKey: "Reference must be mono 24 kHz 16-bit PCM WAV"])
        }
        let samples = Qwen3TTSPipeline.wavToFloatSamples(data: data)
        guard !samples.isEmpty, let codes = pipeline.encodeReferenceAudio(audioSamples: samples) else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 7,
                          userInfo: [NSLocalizedDescriptionKey: "Could not encode reference audio for ICL"])
        }
        return codes
    }

    private static func pcm16LE(_ samples: [Float]) -> Data {
        var data = Data(capacity: samples.count * 2)
        for sample in samples {
            let value = Int16((max(-1, min(1, sample)) * 32767).rounded()).littleEndian
            withUnsafeBytes(of: value) { data.append(contentsOf: $0) }
        }
        return data
    }

    private static func writeJSON<T: Encodable>(_ value: T) {
        guard let data = try? JSONEncoder().encode(value) else { return }
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([0x0A]))
    }
}
