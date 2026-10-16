import XCTest
@testable import Qwen3TTS

final class StreamingCachePolicyTests: XCTestCase {
    func testCompactionRequiresBothBoundaryAndOverflow() {
        XCTAssertFalse(shouldCompactKVCache(step: 14, currentLength: 192, maxWindow: 192))
        XCTAssertFalse(shouldCompactKVCache(step: 13, currentLength: 193, maxWindow: 192))
        XCTAssertTrue(shouldCompactKVCache(step: 14, currentLength: 193, maxWindow: 192))
    }

    func testLowLatencyEmissionRejoinsStableCadenceExactly() {
        XCTAssertEqual(streamingFramesToNextEmission(emittedFrames: 0), 2)
        XCTAssertEqual(streamingFramesToNextEmission(emittedFrames: 2), 3)
        XCTAssertEqual(streamingFramesToNextEmission(emittedFrames: 5), 5)
        XCTAssertEqual(streamingFramesToNextEmission(emittedFrames: 45), 3)
        XCTAssertEqual(streamingFramesToNextEmission(emittedFrames: 48), 12)
    }
}
