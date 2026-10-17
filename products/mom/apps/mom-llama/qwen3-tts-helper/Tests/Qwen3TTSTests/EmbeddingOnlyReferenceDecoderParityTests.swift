import Foundation
import MLX
import MLXRandom
import XCTest
@testable import Qwen3TTS

/// Opt-in real-model regression for the reference-padding onset fix.
/// Keep only generic text and synthetic codec codes in this source.
final class EmbeddingOnlyReferenceDecoderParityTests: XCTestCase {
    func testReferenceCodesDoNotChangeEmbeddingOnlyPCM() async throws {
        guard let inputPath = ProcessInfo.processInfo.environment["QWEN_ONSET_FIX_DIAGNOSTIC_JSON"] else {
            throw XCTSkip("Set QWEN_ONSET_FIX_DIAGNOSTIC_JSON to an external diagnostic input")
        }
        let input = try JSONDecoder().decode(Input.self, from: Data(contentsOf: URL(fileURLWithPath: inputPath)))
        let pipeline = try Qwen3TTSPipeline(modelPath: URL(fileURLWithPath: input.modelPath, isDirectory: true))
        let referenceData = try Data(contentsOf: URL(fileURLWithPath: input.referenceWavPath), options: [.mappedIfSafe])
        let referenceSamples = Qwen3TTSPipeline.wavToFloatSamples(data: referenceData)
        let embedding = try XCTUnwrap(
            pipeline.extractSpeakerEmbedding(audioSamples: referenceSamples),
            "The chosen model must provide a speaker encoder"
        )

        let noReference = try await render(
            pipeline: pipeline,
            speakerEmbedding: embedding,
            referenceAudioCodes: nil,
            seed: input.seed
        )
        let reference32 = try await render(
            pipeline: pipeline,
            speakerEmbedding: embedding,
            referenceAudioCodes: syntheticReference(frameCount: 32),
            seed: input.seed
        )
        let reference80 = try await render(
            pipeline: pipeline,
            speakerEmbedding: embedding,
            referenceAudioCodes: syntheticReference(frameCount: 80),
            seed: input.seed
        )

        for (label, rendered) in [
            ("no-reference", noReference),
            ("reference-32", reference32),
            ("reference-80", reference80)
        ] {
            XCTAssertFalse(rendered.samples.isEmpty, "\(label) must produce PCM")
            XCTAssertGreaterThan(rendered.summary.tokenCount, 0, "\(label) must generate codec tokens")
            XCTAssertNotNil(rendered.summary.firstPCMElapsedMicros, "\(label) must yield PCM")
            XCTAssertTrue(rendered.samples.allSatisfy(\.isFinite), "\(label) PCM must be finite")
            XCTAssertGreaterThan(rendered.summary.peak, 0, "\(label) PCM must not be silent")
        }

        let comparisons = [
            compare(noReference.samples, reference32.samples, label: "reference-32"),
            compare(noReference.samples, reference80.samples, label: "reference-80")
        ]
        let artifact = ResultArtifact(
            seed: input.seed,
            temperature: 0.65,
            maxTokens: 64,
            textWasGeneric: true,
            sampleRate: 24_000,
            noReference: noReference.summary,
            reference32: reference32.summary,
            reference80: reference80.summary,
            comparisons: comparisons
        )
        let outputURL = URL(fileURLWithPath: input.outputPath)
        try FileManager.default.createDirectory(at: outputURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(artifact).write(to: outputURL, options: .atomic)

        for comparison in comparisons {
            XCTAssertEqual(comparison.sampleCount, comparison.otherSampleCount)
            XCTAssertLessThanOrEqual(
                comparison.maxAbsoluteDifference,
                1e-6,
                "Reference codec codes must not affect embedding-only decoding"
            )
        }
    }

    private func render(
        pipeline: Qwen3TTSPipeline,
        speakerEmbedding: [Float],
        referenceAudioCodes: [[Int32]]?,
        seed: UInt64
    ) async throws -> Rendered {
        MLXRandom.seed(seed)
        let started = DispatchTime.now().uptimeNanoseconds
        let stream = pipeline.generateStream(
            text: "This short sentence tests whether reference codes alter the voice onset.",
            speakerEmbedding: speakerEmbedding,
            referenceTranscript: nil,
            referenceAudioCodes: referenceAudioCodes,
            temperature: 0.65,
            maxTokens: 64,
            chunkSize: 2,
            preparedPrefixKey: nil
        )
        var samples: [Float] = []
        var firstPCMElapsedMicros: UInt64?
        var firstCodeGenerationMicros: UInt64?
        var firstDecodeMicros: UInt64?
        var tokenCount = 0
        for try await chunk in stream {
            if !chunk.samples.isEmpty {
                samples.append(contentsOf: chunk.samples)
                if firstPCMElapsedMicros == nil {
                    firstPCMElapsedMicros = (DispatchTime.now().uptimeNanoseconds - started) / 1_000
                    firstCodeGenerationMicros = chunk.firstCodeGenerationMicros
                    firstDecodeMicros = chunk.firstDecodeMicros
                }
            }
            tokenCount = max(tokenCount, chunk.tokenRange.upperBound)
        }
        return Rendered(
            samples: samples,
            summary: CaseSummary(
                sampleCount: samples.count,
                durationMilliseconds: Double(samples.count) * 1_000 / 24_000,
                tokenCount: tokenCount,
                firstPCMElapsedMicros: firstPCMElapsedMicros,
                firstCodeGenerationMicros: firstCodeGenerationMicros,
                firstDecodeMicros: firstDecodeMicros,
                rms: rms(samples),
                peak: samples.map { abs($0) }.max() ?? 0,
                first200msRMS: rms(Array(samples.prefix(4_800)))
            )
        )
    }

    private func syntheticReference(frameCount: Int) -> [[Int32]] {
        (0..<16).map { group in
            (0..<frameCount).map { frame in
                Int32((group * 127 + frame * 31) % 2048)
            }
        }
    }

    private func compare(_ lhs: [Float], _ rhs: [Float], label: String) -> Comparison {
        let count = min(lhs.count, rhs.count)
        var maxDifference: Float = 0
        var squaredDifference = 0.0
        for index in 0..<count {
            let difference = abs(lhs[index] - rhs[index])
            maxDifference = max(maxDifference, difference)
            squaredDifference += Double(difference * difference)
        }
        return Comparison(
            label: label,
            sampleCount: lhs.count,
            otherSampleCount: rhs.count,
            comparedSampleCount: count,
            maxAbsoluteDifference: maxDifference,
            differenceRMS: count > 0 ? sqrt(squaredDifference / Double(count)) : 0
        )
    }

    private func rms(_ samples: [Float]) -> Double {
        guard !samples.isEmpty else { return 0 }
        return sqrt(samples.reduce(0.0) { $0 + Double($1) * Double($1) } / Double(samples.count))
    }
}

private struct Input: Decodable {
    let modelPath: String
    let referenceWavPath: String
    let outputPath: String
    let seed: UInt64

    enum CodingKeys: String, CodingKey {
        case modelPath = "model_path"
        case referenceWavPath = "reference_wav_path"
        case outputPath = "output_path"
        case seed
    }
}

private struct Rendered {
    let samples: [Float]
    let summary: CaseSummary
}

private struct CaseSummary: Encodable {
    let sampleCount: Int
    let durationMilliseconds: Double
    let tokenCount: Int
    let firstPCMElapsedMicros: UInt64?
    let firstCodeGenerationMicros: UInt64?
    let firstDecodeMicros: UInt64?
    let rms: Double
    let peak: Float
    let first200msRMS: Double
}

private struct Comparison: Encodable {
    let label: String
    let sampleCount: Int
    let otherSampleCount: Int
    let comparedSampleCount: Int
    let maxAbsoluteDifference: Float
    let differenceRMS: Double
}

private struct ResultArtifact: Encodable {
    let seed: UInt64
    let temperature: Float
    let maxTokens: Int
    let textWasGeneric: Bool
    let sampleRate: Int
    let noReference: CaseSummary
    let reference32: CaseSummary
    let reference80: CaseSummary
    let comparisons: [Comparison]
}
