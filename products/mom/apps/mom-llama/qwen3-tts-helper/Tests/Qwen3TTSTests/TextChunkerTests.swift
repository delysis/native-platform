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
