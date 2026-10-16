import MLX
import MLXNN
import XCTest
@testable import Qwen3TTS

final class PersonaLoRATests: XCTestCase {
    func testAppliesPEFTLayoutWithAlphaOverRankScaleAndCanDeactivate() throws {
        let base = Linear(weight: MLXArray([Float(1), 0, 0, 1]).reshaped([2, 2]))
        let wrapped = PersonaLoRALinear(base: base)
        // A selects input 0. B maps it to both outputs. The supplied scale is
        // already the runtime alpha/r multiplier computed by the controller.
        try wrapped.install(
            personaID: "gabor-mate",
            a: MLXArray([Float(1), 0]).reshaped([1, 2]),
            b: MLXArray([Float(2), 3]).reshaped([2, 1]),
            scale: 0.5
        )
        let input = MLXArray([Float(1), 2]).reshaped([1, 2])

        try wrapped.activate("gabor-mate")
        let adapted = wrapped(input)
        eval(adapted)
        XCTAssertEqual(adapted.asArray(Float.self), [2, 3.5])

        try wrapped.activate(nil)
        let baseOnly = wrapped(input)
        eval(baseOnly)
        XCTAssertEqual(baseOnly.asArray(Float.self), [1, 2])
    }

    func testRejectsUnknownPersonaWithoutChangingActiveAdapter() throws {
        let base = Linear(weight: MLXArray([Float(1)]).reshaped([1, 1]))
        let wrapped = PersonaLoRALinear(base: base)
        try wrapped.install(
            personaID: "gabor-mate",
            a: MLXArray([Float(1)]).reshaped([1, 1]),
            b: MLXArray([Float(2)]).reshaped([1, 1]),
            scale: 1
        )
        try wrapped.activate("gabor-mate")

        XCTAssertThrowsError(try wrapped.activate("bessel-van-der-kolk"))
        let output = wrapped(MLXArray([Float(1)]).reshaped([1, 1]))
        eval(output)
        XCTAssertEqual(output.asArray(Float.self), [3])
    }
}
