import XCTest
@testable import Qwen3TTS

final class StreamingPCMOverlapTests: XCTestCase {
    private let samplesPerFrame = 1_920

    func testFrameBatchesPreserveContinuousSampleTimelineWithAndWithoutFinalRemainder() {
        for remainder in [0, 1_920, 137] {
            var overlap = StreamingPCMOverlap(overlapSamples: 512)
            var output: [Float] = []
            var decodedThrough = 0

            for frames in [2, 3, 5, 5] {
                let newCount = frames * samplesPerFrame
                decodedThrough += newCount
                let requiredCount = overlap.requiredSampleCount(newSampleCount: newCount)
                let decoded = decodedWindow(
                    start: decodedThrough - requiredCount,
                    count: requiredCount
                )
                let chunk = overlap.append(decoded, final: false)
                output.append(contentsOf: chunk)
            }

            if remainder > 0 {
                decodedThrough += remainder
                let requiredCount = overlap.requiredSampleCount(newSampleCount: remainder)
                let decoded = decodedWindow(
                    start: decodedThrough - requiredCount,
                    count: requiredCount
                )
                output.append(contentsOf: overlap.append(decoded, final: true))
            } else {
                output.append(contentsOf: overlap.finish())
            }

            let expectedCount = decodedThrough
            XCTAssertEqual(output.count, expectedCount, "remainder=\(remainder)")
            let continuous = decodedWindow(start: 0, count: expectedCount)
            for sampleIndex in 512..<(expectedCount - 512) {
                XCTAssertEqual(
                    output[sampleIndex], continuous[sampleIndex], accuracy: 0.000_001,
                    "timeline diverged at sample \(sampleIndex), remainder=\(remainder)"
                )
            }
        }
    }

    func testSmallDecodedBlocksEmitAllSamplesOutsideTheSinglePendingTail() {
        var overlap = StreamingPCMOverlap(overlapSamples: 512)
        var output: [Float] = []
        var sourceStart = 0
        for count in [700, 700, 700, 700] {
            let decoded = decodedWindow(
                start: sourceStart,
                count: overlap.requiredSampleCount(newSampleCount: count)
            )
            let chunk = overlap.append(decoded, final: false)
            output.append(contentsOf: chunk)
            sourceStart += chunk.count
            XCTAssertLessThanOrEqual(overlap.pendingTail.count, 512)
        }
        output.append(contentsOf: overlap.finish())
        XCTAssertEqual(output.count, 2_800)
        let continuous = decodedWindow(start: 0, count: 2_800)
        for sampleIndex in 512..<2_288 {
            XCTAssertEqual(output[sampleIndex], continuous[sampleIndex], accuracy: 0.000_001)
        }
    }

    private func decodedWindow(start: Int, count: Int) -> [Float] {
        (start..<(start + count)).map { Float($0 % 1_000) / 1_000 }
    }
}
