import CryptoKit
import Foundation
import MLX

/// Versioned on-disk contract for one persona LoRA. The manifest names every
/// tensor explicitly so the runtime never guesses PEFT prefixes or applies an
/// adapter to a similarly named, but incompatible, module.
public struct PersonaAdapterManifest: Codable, Equatable, Sendable {
    public static let currentSchema = "mom-qwen3-tts-persona-adapter-v1"

    public struct BaseModel: Codable, Equatable, Sendable {
        public let modelID: String
        public let revision: String
        public let weightsSHA256: String

        enum CodingKeys: String, CodingKey {
            case modelID = "model_id"
            case revision
            case weightsSHA256 = "weights_sha256"
        }
    }

    public struct TensorPair: Codable, Equatable, Sendable {
        /// Runtime module path, without a PEFT wrapper prefix.
        public let module: String
        public let aKey: String
        public let bKey: String
        /// PEFT storage shapes. A is [rank, input], B is [output, rank].
        public let aShape: [Int]
        public let bShape: [Int]
        public let dtype: String

        enum CodingKeys: String, CodingKey {
            case module
            case aKey = "a_key"
            case bKey = "b_key"
            case aShape = "a_shape"
            case bShape = "b_shape"
            case dtype
        }
    }

    public struct Adapter: Codable, Equatable, Sendable {
        public let file: String
        public let sha256: String
        public let rank: Int
        public let alpha: Float
        /// Additional evaluated runtime multiplier. Effective scale is
        /// `inference_scale * alpha / rank`.
        public let inferenceScale: Float
        public let tensors: [TensorPair]

        enum CodingKeys: String, CodingKey {
            case file, sha256, rank, alpha, tensors
            case inferenceScale = "inference_scale"
        }
    }

    public struct SpeakerEmbedding: Codable, Equatable, Sendable {
        public let file: String
        public let sha256: String
        public let tensorKey: String
        public let dimensions: Int
        public let dtype: String

        enum CodingKeys: String, CodingKey {
            case file, sha256, dimensions, dtype
            case tensorKey = "tensor_key"
        }
    }

    public struct TrainingEvidence: Codable, Equatable, Sendable {
        public let datasetManifestSHA256: String
        public let rightsRecordSHA256: String?
        public let trainerID: String
        public let trainerRevision: String
        public let configSHA256: String

        enum CodingKeys: String, CodingKey {
            case datasetManifestSHA256 = "dataset_manifest_sha256"
            case rightsRecordSHA256 = "rights_record_sha256"
            case trainerID = "trainer_id"
            case trainerRevision = "trainer_revision"
            case configSHA256 = "config_sha256"
        }
    }

    public let schema: String
    public let artifactID: String
    public let personaID: String
    public let baseModel: BaseModel
    public let adapter: Adapter
    public let speakerEmbedding: SpeakerEmbedding
    public let trainingEvidence: TrainingEvidence

    enum CodingKeys: String, CodingKey {
        case schema
        case artifactID = "artifact_id"
        case personaID = "persona_id"
        case baseModel = "base_model"
        case adapter
        case speakerEmbedding = "speaker_embedding"
        case trainingEvidence = "training_evidence"
    }

    /// Stable identity over everything that can change inference behavior.
    /// `artifact_id` itself is excluded to avoid a self-referential digest.
    public func expectedArtifactID() -> String {
        let tensorIdentity = adapter.tensors.sorted { $0.module < $1.module }.map {
            [
                $0.module, $0.aKey, $0.bKey,
                $0.aShape.map(String.init).joined(separator: ","),
                $0.bShape.map(String.init).joined(separator: ","),
                $0.dtype,
            ].joined(separator: "\u{1f}")
        }.joined(separator: "\u{1e}")
        let identity = [
            schema, personaID,
            baseModel.modelID, baseModel.revision, baseModel.weightsSHA256,
            adapter.file, adapter.sha256, String(adapter.rank), String(adapter.alpha.bitPattern),
            String(adapter.inferenceScale.bitPattern),
            tensorIdentity,
            speakerEmbedding.file, speakerEmbedding.sha256,
            speakerEmbedding.tensorKey, String(speakerEmbedding.dimensions),
            speakerEmbedding.dtype,
            trainingEvidence.datasetManifestSHA256, trainingEvidence.rightsRecordSHA256 ?? "",
            trainingEvidence.trainerID, trainingEvidence.trainerRevision,
            trainingEvidence.configSHA256,
        ].joined(separator: "\u{1d}")
        return "sha256:" + Self.sha256(Data(identity.utf8))
    }

    public func validateMetadata() throws {
        guard schema == Self.currentSchema else {
            throw PersonaAdapterError.unsupportedSchema(schema)
        }
        guard Self.validIdentifier(personaID) else {
            throw PersonaAdapterError.invalidManifest("persona_id must be lowercase ASCII, digits, or hyphens")
        }
        guard !baseModel.modelID.isEmpty, !baseModel.revision.isEmpty,
              Self.validDigest(baseModel.weightsSHA256) else {
            throw PersonaAdapterError.invalidManifest("base_model identity is incomplete")
        }
        guard Self.safeLeaf(adapter.file), Self.validDigest(adapter.sha256),
              (1...256).contains(adapter.rank), adapter.alpha.isFinite, adapter.alpha > 0,
              adapter.inferenceScale.isFinite, adapter.inferenceScale > 0,
              adapter.inferenceScale <= 2,
              !adapter.tensors.isEmpty, adapter.tensors.count <= 4096 else {
            throw PersonaAdapterError.invalidManifest("adapter metadata is invalid")
        }
        guard Self.safeLeaf(speakerEmbedding.file), Self.validDigest(speakerEmbedding.sha256),
              !speakerEmbedding.tensorKey.isEmpty,
              speakerEmbedding.dimensions == 1024,
              Self.validFloatingDType(speakerEmbedding.dtype) else {
            throw PersonaAdapterError.invalidManifest("speaker embedding metadata is invalid")
        }
        guard Self.validDigest(trainingEvidence.datasetManifestSHA256),
              trainingEvidence.rightsRecordSHA256.map(Self.validDigest) ?? true,
              Self.validDigest(trainingEvidence.configSHA256),
              !trainingEvidence.trainerID.isEmpty,
              !trainingEvidence.trainerRevision.isEmpty else {
            throw PersonaAdapterError.invalidManifest("training evidence is incomplete")
        }

        var modules = Set<String>()
        var keys = Set<String>()
        let allowedSuffixes = [
            ".q_proj", ".k_proj", ".v_proj", ".o_proj",
            ".gate_proj", ".up_proj", ".down_proj",
        ]
        for tensor in adapter.tensors {
            guard !tensor.module.isEmpty,
                  allowedSuffixes.contains(where: tensor.module.hasSuffix),
                  modules.insert(tensor.module).inserted,
                  keys.insert(tensor.aKey).inserted,
                  keys.insert(tensor.bKey).inserted,
                  tensor.aShape.count == 2, tensor.bShape.count == 2,
                  tensor.aShape.allSatisfy({ $0 > 0 }),
                  tensor.bShape.allSatisfy({ $0 > 0 }),
                  Self.validFloatingDType(tensor.dtype),
                  tensor.aShape[0] == adapter.rank,
                  tensor.bShape[1] == adapter.rank else {
                throw PersonaAdapterError.invalidManifest("invalid or duplicate LoRA tensor pair for \(tensor.module)")
            }
        }
        guard artifactID == expectedArtifactID() else {
            throw PersonaAdapterError.artifactIdentityMismatch
        }
    }

    private static func validIdentifier(_ value: String) -> Bool {
        !value.isEmpty && value.count <= 96
            && value.utf8.allSatisfy { byte in
                (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57) || byte == 45
            }
    }

    fileprivate static func validDigest(_ value: String) -> Bool {
        value.count == 64 && value.utf8.allSatisfy {
            ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102)
        }
    }

    private static func safeLeaf(_ value: String) -> Bool {
        !value.isEmpty && value != "." && value != ".."
            && !value.contains("/") && !value.contains("\\")
    }

    private static func validFloatingDType(_ value: String) -> Bool {
        value == "float16" || value == "bfloat16" || value == "float32"
    }

    fileprivate static func mlxDType(_ value: String) -> DType? {
        switch value {
        case "float16": .float16
        case "bfloat16": .bfloat16
        case "float32": .float32
        default: nil
        }
    }

    fileprivate static func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}

public struct LoadedPersonaAdapter {
    public let manifest: PersonaAdapterManifest
    /// PEFT-layout arrays. Application transposes A and B once when installing
    /// the adapter-aware linear wrappers.
    public let tensorPairs: [(module: String, a: MLXArray, b: MLXArray)]
    public let speakerEmbedding: MLXArray
}

/// Immutable resident collection. Loading is intentionally separate from
/// model mutation; a later adapter-aware Linear wrapper can select these
/// already-validated arrays by persona ID without touching disk.
public final class PersonaAdapterBank {
    public let baseModelID: String
    public let baseModelRevision: String
    public let baseWeightsSHA256: String
    private var adapters: [String: LoadedPersonaAdapter] = [:]

    public init(modelPath: URL, modelID: String, revision: String) throws {
        let weights = modelPath.appendingPathComponent("model.safetensors")
        guard FileManager.default.fileExists(atPath: weights.path) else {
            throw PersonaAdapterError.missingFile("model.safetensors")
        }
        baseModelID = modelID
        baseModelRevision = revision
        baseWeightsSHA256 = try Self.fileSHA256(weights)
    }

    public var personaIDs: [String] { adapters.keys.sorted() }

    @discardableResult
    public func load(packageDirectory: URL) throws -> LoadedPersonaAdapter {
        let manifestURL = packageDirectory.appendingPathComponent("manifest.json")
        guard FileManager.default.fileExists(atPath: manifestURL.path) else {
            throw PersonaAdapterError.missingFile("manifest.json")
        }
        let manifest = try JSONDecoder().decode(
            PersonaAdapterManifest.self,
            from: Data(contentsOf: manifestURL, options: [.mappedIfSafe])
        )
        try manifest.validateMetadata()
        guard manifest.baseModel.modelID == baseModelID,
              manifest.baseModel.revision == baseModelRevision,
              manifest.baseModel.weightsSHA256 == baseWeightsSHA256 else {
            throw PersonaAdapterError.baseModelMismatch
        }

        let adapterURL = packageDirectory.appendingPathComponent(manifest.adapter.file)
        let embeddingURL = packageDirectory.appendingPathComponent(manifest.speakerEmbedding.file)
        guard FileManager.default.fileExists(atPath: adapterURL.path) else {
            throw PersonaAdapterError.missingFile(manifest.adapter.file)
        }
        guard FileManager.default.fileExists(atPath: embeddingURL.path) else {
            throw PersonaAdapterError.missingFile(manifest.speakerEmbedding.file)
        }
        guard try Self.fileSHA256(adapterURL) == manifest.adapter.sha256,
              try Self.fileSHA256(embeddingURL) == manifest.speakerEmbedding.sha256 else {
            throw PersonaAdapterError.fileDigestMismatch
        }

        let adapterArrays = try MLX.loadArrays(url: adapterURL, stream: .cpu)
        let expectedKeys = Set(manifest.adapter.tensors.flatMap { [$0.aKey, $0.bKey] })
        guard Set(adapterArrays.keys) == expectedKeys else {
            throw PersonaAdapterError.tensorSetMismatch
        }
        let pairs = try manifest.adapter.tensors.map { pair in
            guard let a = adapterArrays[pair.aKey], let b = adapterArrays[pair.bKey],
                  let dtype = PersonaAdapterManifest.mlxDType(pair.dtype),
                  a.shape == pair.aShape, b.shape == pair.bShape,
                  a.dtype == dtype, b.dtype == dtype else {
                throw PersonaAdapterError.tensorShapeMismatch(pair.module)
            }
            return (module: pair.module, a: a, b: b)
        }

        let embeddingArrays = try MLX.loadArrays(url: embeddingURL, stream: .cpu)
        guard embeddingArrays.count == 1,
              let embedding = embeddingArrays[manifest.speakerEmbedding.tensorKey],
              let embeddingDType = PersonaAdapterManifest.mlxDType(manifest.speakerEmbedding.dtype),
              embedding.size == manifest.speakerEmbedding.dimensions,
              embedding.dtype == embeddingDType,
              embedding.shape == [manifest.speakerEmbedding.dimensions]
                || embedding.shape == [1, manifest.speakerEmbedding.dimensions] else {
            throw PersonaAdapterError.speakerEmbeddingMismatch
        }
        eval(pairs.flatMap { [$0.a, $0.b] } + [embedding])

        let loaded = LoadedPersonaAdapter(
            manifest: manifest,
            tensorPairs: pairs,
            speakerEmbedding: embedding.reshaped([manifest.speakerEmbedding.dimensions])
        )
        adapters[manifest.personaID] = loaded
        return loaded
    }

    public func adapter(personaID: String) -> LoadedPersonaAdapter? {
        adapters[personaID]
    }

    private static func fileSHA256(_ url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while true {
            let data = try handle.read(upToCount: 4 * 1024 * 1024) ?? Data()
            if data.isEmpty { break }
            hasher.update(data: data)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}

public enum PersonaAdapterError: LocalizedError, Equatable {
    case unsupportedSchema(String)
    case invalidManifest(String)
    case artifactIdentityMismatch
    case missingFile(String)
    case fileDigestMismatch
    case baseModelMismatch
    case tensorSetMismatch
    case tensorShapeMismatch(String)
    case speakerEmbeddingMismatch
    case moduleSetMismatch
    case unresolvedModules([String])
    case personaNotLoaded(String)

    public var errorDescription: String? {
        switch self {
        case .unsupportedSchema(let schema): "Unsupported persona adapter schema: \(schema)"
        case .invalidManifest(let detail): "Invalid persona adapter manifest: \(detail)"
        case .artifactIdentityMismatch: "Persona adapter artifact identity does not match its manifest"
        case .missingFile(let path): "Persona adapter file is missing: \(path)"
        case .fileDigestMismatch: "Persona adapter file digest does not match its manifest"
        case .baseModelMismatch: "Persona adapter was trained for a different base model"
        case .tensorSetMismatch: "Persona adapter tensor set does not match its manifest"
        case .tensorShapeMismatch(let module): "Persona adapter tensor shape is invalid for \(module)"
        case .speakerEmbeddingMismatch: "Persona speaker embedding is missing or malformed"
        case .moduleSetMismatch: "Persona adapters do not target the same module set"
        case .unresolvedModules(let modules): "Persona adapter modules are not present in this model: \(modules.joined(separator: ", "))"
        case .personaNotLoaded(let personaID): "Persona adapter is not loaded: \(personaID)"
        }
    }
}
