import CryptoKit
import Foundation

/// Immutable conditioning derived from one admitted reference recording.
/// The cache stores the expensive speaker embedding and audio-code context,
/// never source audio or mutable path identity.
public struct CachedReferenceVoice: Codable, Equatable, Sendable {
    public let speakerEmbedding: [Float]
    public let decoderContext: [[Int32]]

    public init(speakerEmbedding: [Float], decoderContext: [[Int32]]) {
        self.speakerEmbedding = speakerEmbedding
        self.decoderContext = decoderContext
    }
}

public enum ReferenceVoiceCacheSource: String, Codable, Sendable {
    case memory
    case disk
    case computed
}

/// Helper-local timings. Every field is an exclusive stage except `totalMicros`.
public struct ReferenceVoiceCacheTimings: Equatable, Sendable {
    public let identityMicros: Int
    public let memoryLookupMicros: Int
    public let diskLookupMicros: Int
    public let sourceReadMicros: Int
    public let sourceHashMicros: Int
    public let computeMicros: Int
    public let persistMicros: Int
    public let totalMicros: Int
}

public struct ReferenceVoiceCacheResult: Sendable {
    public let voice: CachedReferenceVoice
    public let source: ReferenceVoiceCacheSource
    public let timings: ReferenceVoiceCacheTimings
}

/// Content-addressed cache for the reference-dependent part of Qwen3-TTS.
/// The key binds the exact admitted WAV digest to the model configuration and
/// published model artifact identities. A path is used only on a cache miss.
public struct ReferenceVoiceCache {
    private static let schema = "mom-qwen-reference-voice-v2"
    private static let identityFiles = [
        "config.json",
        "generation_config.json",
        "model.safetensors.index.json",
        "speech_tokenizer/config.json",
        "speech_tokenizer/configuration.json",
        ".cache/huggingface/download/model.safetensors.metadata",
        ".cache/huggingface/download/speech_tokenizer/model.safetensors.metadata",
    ]

    private struct Envelope: Codable {
        let schema: String
        let modelFingerprint: String
        let personaID: String
        let referenceSHA256: String
        let voice: CachedReferenceVoice
    }

    private var values: [String: CachedReferenceVoice] = [:]
    private var recency: [String] = []
    private let modelFingerprint: String
    private let cacheRoot: URL
    private let capacity: Int

    public init(modelPath: URL, cacheRoot: URL? = nil, capacity: Int = 64) {
        precondition(capacity > 0)
        modelFingerprint = Self.fingerprint(modelPath: modelPath)
        let base = cacheRoot
            ?? FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        self.cacheRoot = base
            .appendingPathComponent("com.delysis.mom-llama", isDirectory: true)
            .appendingPathComponent("qwen-reference-voices", isDirectory: true)
        self.capacity = capacity
    }

    public mutating func voice(
        path: URL,
        personaID: String,
        expectedDigest: String,
        compute: (Data) throws -> CachedReferenceVoice
    ) throws -> ReferenceVoiceCacheResult {
        let totalStart = DispatchTime.now().uptimeNanoseconds
        var identityMicros = 0
        var memoryLookupMicros = 0
        var diskLookupMicros = 0
        var sourceReadMicros = 0
        var sourceHashMicros = 0
        var computeMicros = 0
        var persistMicros = 0

        let identityStart = DispatchTime.now().uptimeNanoseconds
        guard Self.validPersonaID(personaID) else {
            throw ReferenceVoiceCacheError.invalidPersonaID
        }
        let referenceDigest = try Self.normalizedDigest(expectedDigest)
        let key = Self.key(
            modelFingerprint: modelFingerprint,
            personaID: personaID,
            referenceDigest: referenceDigest
        )
        identityMicros = Self.elapsedMicros(since: identityStart)

        let memoryStart = DispatchTime.now().uptimeNanoseconds
        if let cached = values[key] {
            touch(key)
            memoryLookupMicros = Self.elapsedMicros(since: memoryStart)
            return result(
                voice: cached,
                source: .memory,
                totalStart: totalStart,
                identity: identityMicros,
                memory: memoryLookupMicros,
                disk: 0,
                read: 0,
                hash: 0,
                compute: 0,
                persist: 0
            )
        }
        memoryLookupMicros = Self.elapsedMicros(since: memoryStart)

        let artifact = cacheRoot.appendingPathComponent(key).appendingPathExtension("plist")
        let diskStart = DispatchTime.now().uptimeNanoseconds
        if let bytes = try? Data(contentsOf: artifact, options: [.mappedIfSafe]),
           let envelope = try? PropertyListDecoder().decode(Envelope.self, from: bytes),
           envelope.schema == Self.schema,
           envelope.modelFingerprint == modelFingerprint,
           envelope.personaID == personaID,
           envelope.referenceSHA256 == referenceDigest,
           Self.valid(envelope.voice) {
            diskLookupMicros = Self.elapsedMicros(since: diskStart)
            insert(envelope.voice, for: key)
            return result(
                voice: envelope.voice,
                source: .disk,
                totalStart: totalStart,
                identity: identityMicros,
                memory: memoryLookupMicros,
                disk: diskLookupMicros,
                read: 0,
                hash: 0,
                compute: 0,
                persist: 0
            )
        }
        diskLookupMicros = Self.elapsedMicros(since: diskStart)

        let readStart = DispatchTime.now().uptimeNanoseconds
        let data = try Data(contentsOf: path, options: [.mappedIfSafe])
        sourceReadMicros = Self.elapsedMicros(since: readStart)

        let hashStart = DispatchTime.now().uptimeNanoseconds
        let actualDigest = Self.sha256(data)
        sourceHashMicros = Self.elapsedMicros(since: hashStart)
        guard actualDigest == referenceDigest else {
            throw ReferenceVoiceCacheError.referenceDigestMismatch
        }

        let computeStart = DispatchTime.now().uptimeNanoseconds
        let encoded = try compute(data)
        computeMicros = Self.elapsedMicros(since: computeStart)
        guard Self.valid(encoded) else {
            throw ReferenceVoiceCacheError.invalidComputedVoice
        }

        let persistStart = DispatchTime.now().uptimeNanoseconds
        let envelope = Envelope(
            schema: Self.schema,
            modelFingerprint: modelFingerprint,
            personaID: personaID,
            referenceSHA256: referenceDigest,
            voice: encoded
        )
        if let bytes = try? Self.encode(envelope) {
            try? FileManager.default.createDirectory(at: cacheRoot, withIntermediateDirectories: true)
            try? bytes.write(to: artifact, options: [.atomic])
        }
        persistMicros = Self.elapsedMicros(since: persistStart)
        insert(encoded, for: key)
        return result(
            voice: encoded,
            source: .computed,
            totalStart: totalStart,
            identity: identityMicros,
            memory: memoryLookupMicros,
            disk: diskLookupMicros,
            read: sourceReadMicros,
            hash: sourceHashMicros,
            compute: computeMicros,
            persist: persistMicros
        )
    }

    private func result(
        voice: CachedReferenceVoice,
        source: ReferenceVoiceCacheSource,
        totalStart: UInt64,
        identity: Int,
        memory: Int,
        disk: Int,
        read: Int,
        hash: Int,
        compute: Int,
        persist: Int
    ) -> ReferenceVoiceCacheResult {
        ReferenceVoiceCacheResult(
            voice: voice,
            source: source,
            timings: ReferenceVoiceCacheTimings(
                identityMicros: identity,
                memoryLookupMicros: memory,
                diskLookupMicros: disk,
                sourceReadMicros: read,
                sourceHashMicros: hash,
                computeMicros: compute,
                persistMicros: persist,
                totalMicros: Self.elapsedMicros(since: totalStart)
            )
        )
    }

    private mutating func insert(_ voice: CachedReferenceVoice, for key: String) {
        values[key] = voice
        touch(key)
        while recency.count > capacity {
            values.removeValue(forKey: recency.removeFirst())
        }
    }

    private mutating func touch(_ key: String) {
        recency.removeAll { $0 == key }
        recency.append(key)
    }

    private static func valid(_ voice: CachedReferenceVoice) -> Bool {
        !voice.speakerEmbedding.isEmpty
            && voice.speakerEmbedding.count <= 16_384
            && !voice.decoderContext.isEmpty
            && voice.decoderContext.count <= 64
            && voice.decoderContext.allSatisfy { !$0.isEmpty && $0.count <= 24_000 }
    }

    private static func normalizedDigest(_ value: String) throws -> String {
        let normalized = value.lowercased()
        guard normalized.utf8.count == 64,
              normalized.utf8.allSatisfy({ byte in
                  (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
              }) else {
            throw ReferenceVoiceCacheError.invalidReferenceDigest
        }
        return normalized
    }

    private static func validPersonaID(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.allSatisfy { byte in
            (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 122) || byte == 95
        }
    }

    private static func key(
        modelFingerprint: String,
        personaID: String,
        referenceDigest: String
    ) -> String {
        sha256(
            Data("\(Self.schema)\u{1f}\(modelFingerprint)\u{1f}\(personaID)\u{1f}\(referenceDigest)".utf8)
        )
    }

    private static func encode(_ envelope: Envelope) throws -> Data {
        let encoder = PropertyListEncoder()
        encoder.outputFormat = .binary
        return try encoder.encode(envelope)
    }

    private static func fingerprint(modelPath: URL) -> String {
        var identity = Data(Self.schema.utf8)
        for relative in identityFiles {
            identity.append(Data(relative.utf8))
            let file = modelPath.appendingPathComponent(relative)
            if let bytes = try? Data(contentsOf: file, options: [.mappedIfSafe]) {
                identity.append(Data(sha256(bytes).utf8))
            } else {
                identity.append(Data("missing".utf8))
            }
        }
        return sha256(identity)
    }

    private static func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private static func elapsedMicros(since started: UInt64) -> Int {
        Int((DispatchTime.now().uptimeNanoseconds - started) / 1_000)
    }
}

public enum ReferenceVoiceCacheError: LocalizedError, Equatable {
    case invalidPersonaID
    case invalidReferenceDigest
    case referenceDigestMismatch
    case invalidComputedVoice

    public var errorDescription: String? {
        switch self {
        case .invalidPersonaID: "Consultant voice identity was absent or malformed"
        case .invalidReferenceDigest: "Reference digest was invalid"
        case .referenceDigestMismatch: "Reference audio changed after admission"
        case .invalidComputedVoice: "Reference conditioning was empty or malformed"
        }
    }
}
