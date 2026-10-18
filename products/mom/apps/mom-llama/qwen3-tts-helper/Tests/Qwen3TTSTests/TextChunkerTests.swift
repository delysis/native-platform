import XCTest
@testable import Qwen3TTS

final class TextChunkerTests: XCTestCase {
    func testSpokenChunksResetAtFormattedSectionAndRemoveMarkup() {
        let text = """
        I've reviewed the transcript. This is a fascinating, almost surreal clinical sequence.

        ## 1. Core Formulation

        **Safety** begins with careful attention to the person in the room and their immediate needs.
        """

        XCTAssertEqual(
            TextChunker.spokenChunks(text),
            [
                "I've reviewed the transcript. This is a fascinating, almost surreal clinical sequence.",
                "Core Formulation. Safety begins with careful attention to the person in the room and their immediate needs."
            ]
        )
    }

    func testSpokenChunksBoundEachGeneration() {
        let words = (1...70).map { "word\($0)" }.joined(separator: " ")
        let chunks = TextChunker.spokenChunks(words, maxWords: 24)

        XCTAssertGreaterThan(chunks.count, 1)
        XCTAssertTrue(chunks.allSatisfy { $0.split(whereSeparator: { $0.isWhitespace }).count <= 24 })
    }

    func testShortSpeechGetsAProportionalTokenEstimate() {
        XCTAssertEqual(TextChunker.estimateTokens(for: "I hear you."), 15)
        XCTAssertEqual(TextChunker.estimateTokens(for: "Hello."), 12)
    }

    func testGenerationStopReportDistinguishesEOSFromBudgetExhaustion() {
        var capped = TTSGenerationStopTracker(maxTokens: 162)
        for _ in 0..<162 { capped.recordGeneratedFrame() }
        XCTAssertEqual(capped.report.stopReason, .maxTokens)
        XCTAssertEqual(capped.report.generatedCodecFrames, 162)
        XCTAssertEqual(capped.report.maxTokens, 162)

        var eos = TTSGenerationStopTracker(maxTokens: 162)
        for _ in 0..<41 { eos.recordGeneratedFrame() }
        eos.stop(.eos)
        XCTAssertEqual(eos.report.stopReason, .eos)
        XCTAssertEqual(eos.report.generatedCodecFrames, 41)
        XCTAssertEqual(eos.report.maxTokens, 162)

        for reason in [TTSGenerationStopReason.padLimit, .exactCycle, .cancelled] {
            var stopped = TTSGenerationStopTracker(maxTokens: 162)
            for _ in 0..<7 { stopped.recordGeneratedFrame() }
            stopped.stop(reason)
            XCTAssertEqual(stopped.report.stopReason, reason)
            XCTAssertEqual(stopped.report.generatedCodecFrames, 7)
        }
    }

    func testExactCodecCycleIsStoppedWithoutRejectingOrdinaryRecurrence() {
        let a: [Int32] = [1, 10, 100]
        let b: [Int32] = [2, 20, 200]
        XCTAssertTrue(AcousticLoopGuard.wouldEnterExactCycle(
            history: [a, b, a, b, a],
            candidate: b
        ))
        XCTAssertFalse(AcousticLoopGuard.wouldEnterExactCycle(
            history: [a, b, a, b, [1, 10, 101]],
            candidate: b
        ))
    }
}
