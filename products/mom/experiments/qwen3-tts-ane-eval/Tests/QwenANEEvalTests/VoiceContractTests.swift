import XCTest
@testable import QwenANE

final class VoiceContractTests: XCTestCase {
    func testPreparedVoiceRequiresEveryCodecGroup() throws {
        XCTAssertThrowsError(try QwenVoiceFiles.validate(refCodes: [[1]]))
        XCTAssertNoThrow(try QwenVoiceFiles.validate(refCodes: Array(repeating: [1, 2], count: 16)))
    }

    func testPreparedVoiceRejectsOutOfRangeCodes() throws {
        var codes = Array(repeating: [1, 2], count: 16)
        codes[7][1] = QwenVoiceFiles.codebookSize
        XCTAssertThrowsError(try QwenVoiceFiles.validate(refCodes: codes))
    }

    func testComputeControlIsExplicit() {
        XCTAssertEqual(QwenComputeTarget.neuralEnginePreferred.rawValue, "neuralEnginePreferred")
        XCTAssertEqual(QwenComputeTarget.cpuOnly.rawValue, "cpuOnly")
    }
}
