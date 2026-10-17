import Foundation

/// Joins repeated decodes of the same timeline. Each input includes the
/// recomputed pending tail before its new samples; adjacent, nonoverlapping
/// inputs would replace new audio and shorten speech at every boundary.
struct StreamingPCMOverlap {
    let overlapSamples: Int
    private(set) var pendingTail: [Float] = []

    func requiredSampleCount(newSampleCount: Int) -> Int {
        newSampleCount + pendingTail.count
    }

    mutating func append(_ samples: [Float], final: Bool) -> [Float] {
        guard !samples.isEmpty else { return [] }
        var joined = samples
        let overlap = min(overlapSamples, pendingTail.count, joined.count)
        if overlap > 0 {
            for index in 0..<overlap {
                let position = Double(index) / Double(max(overlap - 1, 1))
                let fadeIn = Float(0.5 * (1 - cos(Double.pi * position)))
                joined[index] = pendingTail[pendingTail.count - overlap + index]
                    * (1 - fadeIn) + joined[index] * fadeIn
            }
        } else if pendingTail.isEmpty {
            let fadeLength = min(overlapSamples, joined.count)
            for index in 0..<fadeLength {
                let position = Double(index) / Double(max(fadeLength - 1, 1))
                joined[index] *= Float(0.5 * (1 - cos(Double.pi * position)))
            }
        }
        if final {
            let fadeLength = min(overlapSamples, joined.count)
            for index in 0..<fadeLength {
                let position = Double(index) / Double(max(fadeLength - 1, 1))
                joined[joined.count - fadeLength + index]
                    *= Float(0.5 * (1 + cos(Double.pi * position)))
            }
            pendingTail = []
            return joined
        }
        guard joined.count > overlapSamples else {
            pendingTail = joined
            return []
        }
        pendingTail = Array(joined.suffix(overlapSamples))
        return Array(joined.dropLast(overlapSamples))
    }
    mutating func finish() -> [Float] {
        var samples = pendingTail
        pendingTail = []
        let fadeLength = min(overlapSamples, samples.count)
        for index in 0..<fadeLength {
            let position = Double(index) / Double(max(fadeLength - 1, 1))
            samples[samples.count - fadeLength + index]
                *= Float(0.5 * (1 + cos(Double.pi * position)))
        }
        return samples
    }
}
