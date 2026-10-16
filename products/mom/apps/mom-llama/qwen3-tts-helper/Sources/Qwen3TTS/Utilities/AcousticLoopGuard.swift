import Foundation

/// Stops exact codec cycles before a failed EOS decision becomes an audible
/// repeated syllable. Equality covers every codebook, so ordinary recurring
/// pitch or phonemes do not trip the guard.
enum AcousticLoopGuard {
    static func wouldEnterExactCycle(
        history: [[Int32]],
        candidate: [Int32],
        maxPeriod: Int = 12,
        repetitions: Int = 3
    ) -> Bool {
        guard repetitions >= 2, !candidate.isEmpty else { return false }
        let frames = history + [candidate]
        let largestPeriod = min(maxPeriod, frames.count / repetitions)
        guard largestPeriod > 0 else { return false }

        for period in 1...largestPeriod {
            let suffixStart = frames.count - period
            var equal = true
            for repetition in 1..<repetitions {
                let comparisonStart = suffixStart - repetition * period
                for offset in 0..<period where
                    frames[suffixStart + offset] != frames[comparisonStart + offset]
                {
                    equal = false
                    break
                }
                if !equal { break }
            }
            if equal { return true }
        }
        return false
    }
}
