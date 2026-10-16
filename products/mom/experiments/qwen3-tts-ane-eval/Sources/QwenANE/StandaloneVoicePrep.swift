import CoreML
import CryptoKit
import Foundation

public struct QwenPreparedVoice: Sendable {
    public let voice: QwenVoiceFiles
    public let cacheHit: Bool
}

/// Minimal, evaluation-only voice preparation for Mom's already-normalized
/// 24 kHz mono PCM16 references. The upstream production prep also accepts
/// arbitrary AVFoundation-readable inputs; refusing other formats here keeps
/// the ANE experiment narrow and prevents an accidental generic voice fallback.
public enum QwenStandaloneVoicePrep {
    private static let sampleRate = 24_000
    private static let minSamples = 12_000
    private static let maxSamples = 960_000
    private static let speakerFrames = 4096
    private static let cacheVersion = 1

    public static func prepared(
        referenceWAV: Data,
        transcript: String,
        cacheDirectory: URL,
        modelsDirectory: URL,
        modelRevision: String,
        speakerDimension: Int
    ) throws -> QwenPreparedVoice {
        let text = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw QwenANEError.invalid("the reference transcript is empty") }
        let sourceSHA = SHA256.hash(data: referenceWAV).map { String(format: "%02x", $0) }.joined()
        if let cached = cached(
            directory: cacheDirectory,
            sourceSHA: sourceSHA,
            transcript: text,
            modelRevision: modelRevision,
            speakerDimension: speakerDimension
        ) {
            return QwenPreparedVoice(voice: cached, cacheHit: true)
        }

        let samples = try pcm16Mono24k(referenceWAV)
        guard samples.count >= minSamples else {
            throw QwenANEError.invalid("the reference is shorter than 0.5 seconds")
        }
        guard samples.count <= maxSamples else {
            throw QwenANEError.invalid("the reference is longer than 40 seconds")
        }

        let coreML = modelsDirectory.appendingPathComponent("coreml")
        let speech = try loadCPUModel(coreML.appendingPathComponent("QwenSpeechEncoder.mlmodelc"))
        let speaker = try loadCPUModel(coreML.appendingPathComponent("QwenSpeakerEncoder.mlmodelc"))
        guard samples.count <= inputLength(speech, "wav", axis: 2, default: maxSamples) else {
            throw QwenANEError.invalid("the reference exceeds the speech encoder input")
        }
        let voice = QwenVoiceFiles(
            refText: text,
            refCodes: try codes(speech, samples),
            spkEmbedding: try embedding(speaker, samples, dimension: speakerDimension)
        )
        try write(
            voice,
            sourceSHA: sourceSHA,
            transcript: text,
            modelRevision: modelRevision,
            to: cacheDirectory
        )
        return QwenPreparedVoice(voice: voice, cacheHit: false)
    }

    private static func loadCPUModel(_ url: URL) throws -> MLModel {
        guard FileManager.default.fileExists(atPath: url.path) else {
            throw QwenANEError.invalid("missing voice-prep model: \(url.lastPathComponent)")
        }
        let configuration = MLModelConfiguration()
        // Upstream found CPU_AND_NE could hang the ANE compiler for these
        // one-time fp32 encoders. This placement is intentional and reported.
        configuration.computeUnits = .cpuOnly
        return try MLModel(contentsOf: url, configuration: configuration)
    }

    private static func inputLength(_ model: MLModel, _ name: String, axis: Int, default value: Int) -> Int {
        let shape = model.modelDescription.inputDescriptionsByName[name]?.multiArrayConstraint?.shape ?? []
        return axis < shape.count ? shape[axis].intValue : value
    }

    private static func codeFrames(samples: Int) -> Int {
        [4, 5, 6, 8, 2].reduce(samples) { ($0 + $1 - 1) / $1 }
    }

    private static func codes(_ model: MLModel, _ samples: [Float]) throws -> [[Int]] {
        let capacity = inputLength(model, "wav", axis: 2, default: maxSamples)
        let wav = try MLMultiArray(shape: [1, 1, NSNumber(value: capacity)], dataType: .float32)
        let pointer = wav.dataPointer.bindMemory(to: Float.self, capacity: capacity)
        pointer.initialize(repeating: 0, count: capacity)
        samples.withUnsafeBufferPointer { pointer.update(from: $0.baseAddress!, count: samples.count) }
        let count = try MLMultiArray(shape: [1], dataType: .int32)
        count[0] = NSNumber(value: samples.count)
        let output = try model.prediction(
            from: MLDictionaryFeatureProvider(dictionary: ["wav": wav, "n_samples": count])
        )
        guard let array = output.featureValue(for: "codes")?.multiArrayValue,
              array.shape.count == 3,
              array.shape[1].intValue == 16 else {
            throw QwenANEError.invalid("the speech encoder returned no (1,16,T) codes")
        }
        let frames = codeFrames(samples: samples.count)
        guard frames <= array.shape[2].intValue else {
            throw QwenANEError.invalid("the speech encoder returned too few frames")
        }
        return (0..<16).map { group in
            (0..<frames).map { frame in array[[0, group, frame] as [NSNumber]].intValue }
        }
    }

    private static func embedding(_ model: MLModel, _ samples: [Float], dimension: Int) throws -> [Float] {
        let (mel, frames) = QwenMel.logMel(samples)
        let capacity = inputLength(model, "mel", axis: 1, default: speakerFrames)
        guard frames <= capacity else { throw QwenANEError.invalid("the speaker mel exceeds the encoder input") }
        let input = try MLMultiArray(
            shape: [1, NSNumber(value: capacity), NSNumber(value: QwenMel.nMels)],
            dataType: .float32
        )
        let pointer = input.dataPointer.bindMemory(to: Float.self, capacity: capacity * QwenMel.nMels)
        pointer.initialize(repeating: 0, count: capacity * QwenMel.nMels)
        mel.withUnsafeBufferPointer { pointer.update(from: $0.baseAddress!, count: mel.count) }
        let count = try MLMultiArray(shape: [1], dataType: .int32)
        count[0] = NSNumber(value: frames)
        let output = try model.prediction(
            from: MLDictionaryFeatureProvider(dictionary: ["mel": input, "n_frames": count])
        )
        guard let array = output.featureValue(for: "embedding")?.multiArrayValue,
              array.count == dimension else {
            throw QwenANEError.invalid("the speaker encoder returned the wrong embedding width")
        }
        return (0..<dimension).map { array[$0].floatValue }
    }

    private struct WAVChunk {
        let offset: Int
        let length: Int
    }

    private static func pcm16Mono24k(_ data: Data) throws -> [Float] {
        guard data.count >= 44,
              String(data: data[0..<4], encoding: .ascii) == "RIFF",
              String(data: data[8..<12], encoding: .ascii) == "WAVE" else {
            throw QwenANEError.invalid("the reference is not a RIFF/WAVE file")
        }
        var cursor = 12
        var format: (audio: UInt16, channels: UInt16, rate: UInt32, bits: UInt16)?
        var payload: WAVChunk?
        while cursor + 8 <= data.count {
            let id = String(data: data[cursor..<(cursor + 4)], encoding: .ascii) ?? ""
            let size = Int(readU32(data, cursor + 4))
            let start = cursor + 8
            guard start + size <= data.count else { throw QwenANEError.invalid("the WAV has a truncated chunk") }
            if id == "fmt ", size >= 16 {
                format = (readU16(data, start), readU16(data, start + 2), readU32(data, start + 4), readU16(data, start + 14))
            } else if id == "data" {
                payload = WAVChunk(offset: start, length: size)
            }
            cursor = start + size + (size & 1)
        }
        guard let format, format.audio == 1, format.channels == 1,
              format.rate == UInt32(sampleRate), format.bits == 16, let payload else {
            throw QwenANEError.invalid("the evaluation reference must be mono 24 kHz PCM16")
        }
        return (stride(from: payload.offset, to: payload.offset + payload.length - 1, by: 2)).map {
            Float(Int16(bitPattern: readU16(data, $0))) / 32_768
        }
    }

    private static func readU16(_ data: Data, _ offset: Int) -> UInt16 {
        UInt16(data[offset]) | UInt16(data[offset + 1]) << 8
    }

    private static func readU32(_ data: Data, _ offset: Int) -> UInt32 {
        UInt32(data[offset]) | UInt32(data[offset + 1]) << 8 |
            UInt32(data[offset + 2]) << 16 | UInt32(data[offset + 3]) << 24
    }

    private static func cached(
        directory: URL,
        sourceSHA: String,
        transcript: String,
        modelRevision: String,
        speakerDimension: Int
    ) -> QwenVoiceFiles? {
        let metadata = directory.appendingPathComponent("voice.json")
        guard let data = try? Data(contentsOf: metadata),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              json["source_sha256"] as? String == sourceSHA,
              json["ref_text"] as? String == transcript,
              json["model_revision"] as? String == modelRevision,
              json["cache_version"] as? Int == cacheVersion,
              let voice = try? QwenVoiceFiles(directory: directory),
              voice.spkEmbedding.count == speakerDimension else { return nil }
        return voice
    }

    private static func write(
        _ voice: QwenVoiceFiles,
        sourceSHA: String,
        transcript: String,
        modelRevision: String,
        to directory: URL
    ) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let frames = voice.refCodes[0].count
        let codes = voice.refCodes.flatMap { $0 }.map(Int32.init)
        try npy(codes, descr: "<i4", shape: "(1, 16, \(frames))")
            .write(to: directory.appendingPathComponent("ref_codes.npy"), options: .atomic)
        try npy(voice.spkEmbedding, descr: "<f4", shape: "(\(voice.spkEmbedding.count),)")
            .write(to: directory.appendingPathComponent("spk_embed.npy"), options: .atomic)
        let metadata: [String: Any] = [
            "ref_text": transcript,
            "source_sha256": sourceSHA,
            "model_revision": modelRevision,
            "cache_version": cacheVersion,
        ]
        try JSONSerialization.data(withJSONObject: metadata, options: [.prettyPrinted, .sortedKeys])
            .write(to: directory.appendingPathComponent("voice.json"), options: .atomic)
    }

    private static func npy<T>(_ values: [T], descr: String, shape: String) -> Data {
        var header = "{'descr': '\(descr)', 'fortran_order': False, 'shape': \(shape), }"
        let length = 10 + header.utf8.count + 1
        header += String(repeating: " ", count: (64 - length % 64) % 64) + "\n"
        var data = Data([0x93]) + Data("NUMPY".utf8) + Data([1, 0])
        let count = UInt16(header.utf8.count)
        data.append(contentsOf: [UInt8(count & 0xff), UInt8(count >> 8)])
        data.append(Data(header.utf8))
        values.withUnsafeBytes { data.append(contentsOf: $0) }
        return data
    }
}
