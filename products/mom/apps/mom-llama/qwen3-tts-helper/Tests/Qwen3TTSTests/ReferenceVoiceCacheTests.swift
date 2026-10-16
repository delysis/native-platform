import CryptoKit
import Foundation
@testable import Qwen3TTS
import XCTest

final class ReferenceVoiceCacheTests: XCTestCase {
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory
            .appendingPathComponent("reference-voice-cache-tests-(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    func testExactReferenceUsesMemoryThenDiskWithoutRecomputation() throws {
        let model = try modelDirectory(config: "model-a")
        let audio = Data("exact admitted reference".utf8)
        let reference = try write(audio, named: "voice.wav")
        let digest = sha256(audio)
        var computations = 0
        let compute: (Data) throws -> CachedReferenceVoice = { _ in
            computations += 1
            return self.voice(marker: Float(computations))
        }

        var cache = ReferenceVoiceCache(modelPath: model, cacheRoot: root, capacity: 2)
        let first = try cache.voice(path: reference, personaID: "gabor_mate", expectedDigest: digest, compute: compute)
        let second = try cache.voice(path: reference, personaID: "gabor_mate", expectedDigest: digest, compute: compute)

        XCTAssertEqual(first.source, .computed)
        XCTAssertEqual(second.source, .memory)
        XCTAssertEqual(computations, 1)
        XCTAssertGreaterThanOrEqual(first.timings.computeMicros, 0)
        XCTAssertEqual(second.timings.sourceReadMicros, 0)
        XCTAssertEqual(second.timings.sourceHashMicros, 0)
        XCTAssertEqual(second.timings.computeMicros, 0)

        var restarted = ReferenceVoiceCache(modelPath: model, cacheRoot: root, capacity: 2)
        let third = try restarted.voice(path: reference, personaID: "gabor_mate", expectedDigest: digest, compute: compute)
        XCTAssertEqual(third.source, .disk)
        XCTAssertEqual(third.voice, first.voice)
        XCTAssertEqual(computations, 1)
        XCTAssertEqual(third.timings.sourceReadMicros, 0)
        XCTAssertEqual(third.timings.computeMicros, 0)
    }

    func testChangedReferenceBytesHaveDistinctIdentityAndCannotReuseOldArtifact() throws {
        let model = try modelDirectory(config: "model-a")
        let original = Data("reference version one".utf8)
        let changed = Data("reference version two".utf8)
        let reference = try write(original, named: "voice.wav")
        var computations = 0
        let compute: (Data) throws -> CachedReferenceVoice = { data in
            computations += 1
            return self.voice(marker: Float(data.count))
        }

        var cache = ReferenceVoiceCache(modelPath: model, cacheRoot: root)
        let first = try cache.voice(path: reference, personaID: "gabor_mate", expectedDigest: sha256(original), compute: compute)
        try changed.write(to: reference, options: .atomic)
        let second = try cache.voice(path: reference, personaID: "gabor_mate", expectedDigest: sha256(changed), compute: compute)

        XCTAssertEqual(first.source, .computed)
        XCTAssertEqual(second.source, .computed)
        XCTAssertEqual(computations, 2)

        var isolated = ReferenceVoiceCache(
            modelPath: model,
            cacheRoot: root.appendingPathComponent("isolated", isDirectory: true)
        )
        XCTAssertThrowsError(
            try isolated.voice(path: reference, personaID: "gabor_mate", expectedDigest: sha256(original), compute: compute)
        ) { error in
            XCTAssertEqual(error as? ReferenceVoiceCacheError, .referenceDigestMismatch)
        }
        XCTAssertEqual(computations, 2)
    }

    func testChangedModelConfigurationCannotReusePriorDiskArtifact() throws {
        let model = try modelDirectory(config: "model-a")
        let audio = Data("same reference".utf8)
        let reference = try write(audio, named: "voice.wav")
        let digest = sha256(audio)
        var computations = 0
        let compute: (Data) throws -> CachedReferenceVoice = { _ in
            computations += 1
            return self.voice(marker: Float(computations))
        }

        var firstCache = ReferenceVoiceCache(modelPath: model, cacheRoot: root)
        _ = try firstCache.voice(path: reference, personaID: "gabor_mate", expectedDigest: digest, compute: compute)
        try Data("model-b".utf8).write(
            to: model.appendingPathComponent("config.json"),
            options: .atomic
        )
        var changedModelCache = ReferenceVoiceCache(modelPath: model, cacheRoot: root)
        let changed = try changedModelCache.voice(
            path: reference,
            personaID: "gabor_mate",
            expectedDigest: digest,
            compute: compute
        )

        XCTAssertEqual(changed.source, .computed)
        XCTAssertEqual(computations, 2)
    }

    func testPersonaIdentityIsPartOfCacheIdentityAndMissingIdentityFailsClosed() throws {
        let model = try modelDirectory(config: "model-a")
        let audio = Data("shared bytes are not shared authority".utf8)
        let reference = try write(audio, named: "voice.wav")
        let digest = sha256(audio)
        var computations = 0
        let compute: (Data) throws -> CachedReferenceVoice = { _ in
            computations += 1
            return self.voice(marker: Float(computations))
        }
        var cache = ReferenceVoiceCache(modelPath: model, cacheRoot: root)

        let first = try cache.voice(
            path: reference,
            personaID: "gabor_mate",
            expectedDigest: digest,
            compute: compute
        )
        let second = try cache.voice(
            path: reference,
            personaID: "janina_fisher",
            expectedDigest: digest,
            compute: compute
        )

        XCTAssertEqual(first.source, .computed)
        XCTAssertEqual(second.source, .computed)
        XCTAssertEqual(computations, 2)
        XCTAssertThrowsError(
            try cache.voice(path: reference, personaID: "", expectedDigest: digest, compute: compute)
        ) { error in
            XCTAssertEqual(error as? ReferenceVoiceCacheError, .invalidPersonaID)
        }
        XCTAssertEqual(computations, 2)
    }

    private func voice(marker: Float) -> CachedReferenceVoice {
        CachedReferenceVoice(
            speakerEmbedding: [marker, marker + 1],
            decoderContext: [[Int32(marker), Int32(marker) + 1]]
        )
    }

    private func modelDirectory(config: String) throws -> URL {
        let model = root.appendingPathComponent("model", isDirectory: true)
        let files: [String: String] = [
            "config.json": config,
            "generation_config.json": "generation",
            "model.safetensors.index.json": "index",
            "speech_tokenizer/config.json": "speech-config",
            "speech_tokenizer/configuration.json": "speech-configuration",
            ".cache/huggingface/download/model.safetensors.metadata": "model-digest",
            ".cache/huggingface/download/speech_tokenizer/model.safetensors.metadata": "speech-digest",
        ]
        for (relative, contents) in files {
            let path = model.appendingPathComponent(relative)
            try FileManager.default.createDirectory(
                at: path.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try Data(contents.utf8).write(to: path)
        }
        return model
    }

    private func write(_ data: Data, named name: String) throws -> URL {
        let path = root.appendingPathComponent(name)
        try data.write(to: path)
        return path
    }

    private func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}
