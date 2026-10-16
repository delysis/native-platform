import XCTest
@testable import Qwen3TTS

final class PersonaAdapterManifestTests: XCTestCase {
    func testContentIdentityAndMetadataValidation() throws {
        var manifest = fixture(artifactID: "pending")
        manifest = fixture(artifactID: manifest.expectedArtifactID())
        XCTAssertNoThrow(try manifest.validateMetadata())
    }

    func testRejectsWrongArtifactIdentity() {
        let manifest = fixture(artifactID: "sha256:" + String(repeating: "0", count: 64))
        XCTAssertThrowsError(try manifest.validateMetadata()) { error in
            XCTAssertEqual(error as? PersonaAdapterError, .artifactIdentityMismatch)
        }
    }

    func testRejectsUnexpectedTargetModule() {
        let pair = PersonaAdapterManifest.TensorPair(
            module: "talker.model.layers.0.norm",
            aKey: "norm.lora_A.weight",
            bKey: "norm.lora_B.weight",
            aShape: [16, 2048],
            bShape: [2048, 16],
            dtype: "bfloat16"
        )
        var manifest = fixture(artifactID: "pending", tensors: [pair])
        manifest = fixture(artifactID: manifest.expectedArtifactID(), tensors: [pair])
        XCTAssertThrowsError(try manifest.validateMetadata())
    }

    func testPersonalProjectPackageDoesNotRequireRightsPaperwork() {
        var manifest = fixture(artifactID: "pending", rightsRecordSHA256: nil)
        manifest = fixture(artifactID: manifest.expectedArtifactID(), rightsRecordSHA256: nil)
        XCTAssertNoThrow(try manifest.validateMetadata())
    }

    private func fixture(
        artifactID: String,
        tensors: [PersonaAdapterManifest.TensorPair]? = nil,
        rightsRecordSHA256: String? = String(repeating: "e", count: 64)
    ) -> PersonaAdapterManifest {
        let pair = PersonaAdapterManifest.TensorPair(
            module: "talker.model.layers.0.self_attn.q_proj",
            aKey: "base_model.model.talker.model.layers.0.self_attn.q_proj.lora_A.weight",
            bKey: "base_model.model.talker.model.layers.0.self_attn.q_proj.lora_B.weight",
            aShape: [16, 2048],
            bShape: [2048, 16],
            dtype: "bfloat16"
        )
        return PersonaAdapterManifest(
            schema: PersonaAdapterManifest.currentSchema,
            artifactID: artifactID,
            personaID: "authorized-pilot",
            baseModel: .init(
                modelID: "Qwen/Qwen3-TTS-12Hz-1.7B-Base",
                revision: "pinned-revision",
                weightsSHA256: String(repeating: "a", count: 64)
            ),
            adapter: .init(
                file: "adapter_model.safetensors",
                sha256: String(repeating: "b", count: 64),
                rank: 16,
                alpha: 32,
                inferenceScale: 0.3,
                tensors: tensors ?? [pair]
            ),
            speakerEmbedding: .init(
                file: "speaker_embedding.safetensors",
                sha256: String(repeating: "c", count: 64),
                tensorKey: "speaker_embedding",
                dimensions: 1024,
                dtype: "bfloat16"
            ),
            trainingEvidence: .init(
                datasetManifestSHA256: String(repeating: "d", count: 64),
                rightsRecordSHA256: rightsRecordSHA256,
                trainerID: "instavar-qwen3-tts-lora",
                trainerRevision: "079db4ba844037f970b488dc96b32e5c7490642f",
                configSHA256: String(repeating: "f", count: 64)
            )
        )
    }
}
