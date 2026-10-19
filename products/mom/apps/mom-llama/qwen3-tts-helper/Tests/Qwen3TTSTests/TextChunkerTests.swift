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

    func testBoundedSpokenChunksPreserveShortNormalizedUtteranceExactly() {
        let text = "## Brief heading\n\nA short paragraph has **formatting** and  repeated spaces."
        let normalized = TextChunker.spokenChunks(text, maxWords: .max).joined(separator: " ")

        XCTAssertEqual(TextChunker.boundedSpokenChunks(text, maxWords: 64), [normalized])
    }

    func testBoundedSpokenChunksConserveLongTextAtWordBoundaries() {
        let body = (1...150).map { index -> String in
            let ending = index.isMultiple(of: 15) ? "." : ""
            return "word\(index)\(ending)"
        }.joined(separator: " ")
        let text = "Opening  phrase\twith spacing. " + body
        let normalized = TextChunker.spokenChunks(text, maxWords: .max).joined(separator: " ")
        let chunks = TextChunker.boundedSpokenChunks(text, maxWords: 64)

        XCTAssertGreaterThan(chunks.count, 1)
        XCTAssertTrue(chunks.allSatisfy {
            $0.split(whereSeparator: { $0.isWhitespace }).count <= 64
        })
        XCTAssertEqual(
            chunks.joined(separator: " ").split(whereSeparator: { $0.isWhitespace }).map(String.init),
            normalized.split(whereSeparator: { $0.isWhitespace }).map(String.init)
        )
    }

    func testBoundedSpokenChunksDoNotBreakAtAbbreviations() {
        let opening = "The report from the earlier review confirms that Dr. Smith used U.S. data to complete the analysis."
        let remaining = (1...75).map { "finding\($0)" }.joined(separator: " ")
        let chunks = TextChunker.boundedSpokenChunks("\(opening) \(remaining)", maxWords: 64)

        XCTAssertEqual(chunks.first, opening)
        XCTAssertTrue(chunks.first?.contains("Dr. Smith") == true)
        XCTAssertTrue(chunks.first?.contains("U.S. data") == true)
        XCTAssertTrue(chunks.allSatisfy {
            $0.split(whereSeparator: { $0.isWhitespace }).count <= 64
        })
    }

    func testOriginallyLongInputKeepsEachCompleteSentenceSeparate() {
        let sentences = (1...4).map { sentence in
            ((1...20).map { "word\(sentence)_\($0)" } + ["complete\(sentence)."])
                .joined(separator: " ")
        }
        let text = sentences.joined(separator: " ")
        XCTAssertEqual(text.split(whereSeparator: { $0.isWhitespace }).count, 84)

        XCTAssertEqual(TextChunker.boundedSpokenChunks(text, maxWords: 64), sentences)
    }

    func testSentenceEndingBeforeClosingQuoteIsRecognized() {
        let first = "A complete sentence is quoted here."
        let text = "“\(first)” " + (1...70).map { "word\($0)" }.joined(separator: " ")

        XCTAssertEqual(TextChunker.boundedSpokenChunks(text, maxWords: 64).first, "“\(first)”")
    }

    func testOversizedSentenceUsesLatestClauseAfterSixteenWords() {
        let prefix = (1...20).map { "clause\($0)" }.joined(separator: " ")
        let suffix = (1...55).map { "continuation\($0)" }.joined(separator: " ")
        let text = "\(prefix), \(suffix)."
        let chunks = TextChunker.boundedSpokenChunks(text, maxWords: 64)

        XCTAssertEqual(chunks.first, "\(prefix),")
        XCTAssertTrue(chunks.allSatisfy {
            $0.split(whereSeparator: { $0.isWhitespace }).count <= 64
        })
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
