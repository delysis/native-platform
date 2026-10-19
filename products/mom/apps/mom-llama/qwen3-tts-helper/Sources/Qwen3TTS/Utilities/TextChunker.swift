import Foundation

/// Smart text chunking for memory-efficient long text generation.
/// Finds natural break points to split text while maintaining prosody.
public struct TextChunker {

    /// Default maximum words per chunk
    public static let defaultMaxWords = 35

    /// Minimum words to consider for a chunk (avoid tiny fragments)
    public static let minWords = 8

    /// Prepare formatted assistant text for speech and split it into bounded
    /// generations. Keeping each generation short prevents acoustic drift;
    /// paragraph and heading boundaries provide natural places to reset the
    /// model without changing the text shown in the conversation.
    public static func spokenChunks(_ text: String, maxWords: Int = 24) -> [String] {
        let normalizedLines = text
            .replacingOccurrences(of: "\r\n", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { normalizeSpokenLine(String($0)) }

        var blocks: [String] = []
        var lines: [String] = []
        func flush() {
            let block = lines.joined(separator: " ")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !block.isEmpty { blocks.append(block) }
            lines.removeAll(keepingCapacity: true)
        }

        for line in normalizedLines {
            if line.isEmpty {
                flush()
            } else {
                lines.append(line)
            }
        }
        flush()

        // A short heading is more natural when spoken with the paragraph it
        // introduces, but it still starts a fresh model generation.
        var mergedBlocks: [String] = []
        var index = 0
        while index < blocks.count {
            let block = blocks[index]
            if block.split(whereSeparator: { $0.isWhitespace }).count < 4,
               index + 1 < blocks.count {
                let separator = block.last.map { ".!?".contains($0) } == true ? " " : ". "
                mergedBlocks.append(block + separator + blocks[index + 1])
                index += 2
            } else {
                mergedBlocks.append(block)
                index += 1
            }
        }

        return mergedBlocks.flatMap { chunk($0, maxWords: maxWords) }
    }

    /// Preserve short normalized requests as one utterance. For long requests,
    /// give each complete sentence its own generation, falling back to a
    /// sufficiently long clause or the word limit for oversized sentences.
    /// Canonicalizing whitespace keeps the split boundaries lossless.
    public static func boundedSpokenChunks(_ text: String, maxWords: Int = 64) -> [String] {
        precondition(maxWords > 0)
        let normalized = spokenChunks(text, maxWords: .max).joined(separator: " ")
        guard !normalized.isEmpty else { return [] }
        let words = normalized.split(whereSeparator: { $0.isWhitespace })
        guard words.count > maxWords else { return [normalized] }
        var remaining = words.joined(separator: " ")
        var chunks: [String] = []

        while !remaining.isEmpty {
            let next = boundedLongInputBreak(remaining, maxWords: maxWords)
            let piece = String(remaining.prefix(next)).trimmingCharacters(in: .whitespacesAndNewlines)
            if !piece.isEmpty {
                chunks.append(piece)
            }
            remaining = String(remaining.dropFirst(next))
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }

        return chunks
    }

    private static func normalizeSpokenLine(_ input: String) -> String {
        var line = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !line.isEmpty else { return "" }

        while line.first == "#" { line.removeFirst() }
        line = line.trimmingCharacters(in: .whitespaces)

        for marker in ["- ", "* ", "+ "] where line.hasPrefix(marker) {
            line.removeFirst(marker.count)
            break
        }

        let prefix = line.prefix { $0.isNumber }
        if !prefix.isEmpty {
            let suffix = line.dropFirst(prefix.count)
            if let punctuation = suffix.first,
               punctuation == "." || punctuation == ")" {
                line = String(suffix.dropFirst()).trimmingCharacters(in: .whitespaces)
            }
        }

        for marker in ["**", "__", "`"] {
            line = line.replacingOccurrences(of: marker, with: "")
        }
        return line
    }

    /// Conjunctions that indicate clause boundaries (split BEFORE these)
    private static let conjunctions = [
        " and then ", " and ", " but ", " or ", " so ", " because ",
        " when ", " while ", " although ", " however ", " therefore ",
        " meanwhile ", " afterwards ", " finally ", " then "
    ]

    /// Prepositions that often start new phrases (split BEFORE these, lower priority)
    private static let phraseStarters = [
        " in the ", " on the ", " at the ", " for the ", " with the ",
        " to the ", " from the ", " into the ", " onto the "
    ]

    /// Split text into natural chunks for TTS generation.
    /// - Parameters:
    ///   - text: The input text to chunk
    ///   - maxWords: Maximum words per chunk (default 35)
    /// - Returns: Array of text chunks
    public static func chunk(_ text: String, maxWords: Int = defaultMaxWords) -> [String] {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }

        // If short enough, return as single chunk
        let wordCount = trimmed.split(separator: " ").count
        if wordCount <= maxWords {
            return [trimmed]
        }

        var chunks: [String] = []
        var remaining = trimmed

        while !remaining.isEmpty {
            let chunk = findNaturalBreak(remaining, maxWords: maxWords)
            let trimmedChunk = chunk.trimmingCharacters(in: .whitespacesAndNewlines)

            if !trimmedChunk.isEmpty {
                chunks.append(trimmedChunk)
            }

            // Remove the chunk from remaining text
            remaining = String(remaining.dropFirst(chunk.count))
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }

        return chunks
    }

    /// Find a natural break point within the text.
    private static func findNaturalBreak(_ text: String, maxWords: Int) -> String {
        let words = text.split(separator: " ", omittingEmptySubsequences: true)

        // If text is short enough, return all of it
        if words.count <= maxWords {
            return text
        }

        // Take up to maxWords for our search window
        let windowWords = words.prefix(maxWords)
        let window = windowWords.joined(separator: " ")

        // Priority 1: Sentence endings (. ! ?)
        if let breakPoint = findSentenceEnd(in: window) {
            let chunk = String(window.prefix(breakPoint))
            if chunk.split(separator: " ").count >= minWords {
                return chunk
            }
        }

        // Priority 2: Semicolon or colon (strong clause break)
        if let lastSemi = window.lastIndex(of: ";") {
            let chunk = String(window[...lastSemi])
            if chunk.split(separator: " ").count >= minWords {
                return chunk
            }
        }
        if let lastColon = window.lastIndex(of: ":") {
            let chunk = String(window[...lastColon])
            if chunk.split(separator: " ").count >= minWords {
                return chunk
            }
        }

        // Priority 3: Last comma (clause separator)
        if let lastComma = window.lastIndex(of: ",") {
            let chunk = String(window[...lastComma])
            if chunk.split(separator: " ").count >= minWords {
                return chunk
            }
        }

        // Priority 4: Conjunctions (split BEFORE the conjunction)
        for conjunction in conjunctions {
            if let range = window.range(of: conjunction, options: [.backwards, .caseInsensitive]) {
                let chunk = String(window[..<range.lowerBound])
                if chunk.split(separator: " ").count >= minWords {
                    return chunk
                }
            }
        }

        // Priority 5: Phrase starters (lower priority, split BEFORE)
        for starter in phraseStarters {
            if let range = window.range(of: starter, options: [.backwards, .caseInsensitive]) {
                let chunk = String(window[..<range.lowerBound])
                if chunk.split(separator: " ").count >= minWords {
                    return chunk
                }
            }
        }

        // Priority 6: Hard limit at word boundary
        return window
    }

    /// Find sentence ending position in text.
    /// Returns the position after the sentence-ending punctuation.
    private static func findSentenceEnd(in text: String) -> Int? {
        var lastEnd: Int? = nil
        let minChunkLength = minWords * 4  // Rough character estimate
        let characters = Array(text)

        for (index, char) in characters.enumerated() {
            if char == "." || char == "!" || char == "?" {
                let nextIndex = text.index(text.startIndex, offsetBy: index + 1, limitedBy: text.endIndex)
                if nextIndex == text.endIndex || (nextIndex != nil && text[nextIndex!].isWhitespace) {
                    if char == ".", isAbbreviationPeriod(in: characters, at: index) {
                        continue
                    }
                    if index >= minChunkLength {
                        lastEnd = index + 1
                    }
                }
            }
        }

        return lastEnd
    }

    /// Find the first complete sentence, or the latest usable clause boundary
    /// in the word-limited window. This path is only used for originally-long
    /// requests, so short remaining tails may still be emitted separately.
    private static func boundedLongInputBreak(_ text: String, maxWords: Int) -> Int {
        let words = text.split(whereSeparator: { $0.isWhitespace })
        let window = words.prefix(maxWords).joined(separator: " ")
        if let sentenceEnd = findFirstSentenceEnd(in: window) {
            return sentenceEnd
        }
        guard words.count > maxWords else {
            return text.count
        }

        let characters = Array(window)
        var clauseEnd: Int?
        for (index, character) in characters.enumerated() {
            let candidate: Int?
            if character == ";" || character == ":" || character == "," {
                candidate = index + 1
            } else {
                candidate = nil
            }
            if let candidate,
               String(characters[..<candidate]).split(whereSeparator: { $0.isWhitespace }).count >= 16 {
                clauseEnd = candidate
            }
        }

        for conjunction in conjunctions {
            guard let range = window.range(of: conjunction, options: [.backwards, .caseInsensitive]) else {
                continue
            }
            let prefix = String(window[..<range.lowerBound])
            guard prefix.split(whereSeparator: { $0.isWhitespace }).count >= 16 else { continue }
            let offset = window.distance(from: window.startIndex, to: range.lowerBound)
            if clauseEnd == nil || offset > clauseEnd! {
                clauseEnd = offset
            }
        }

        return clauseEnd ?? window.count
    }

    private static func findFirstSentenceEnd(in text: String) -> Int? {
        let characters = Array(text)
        for (index, character) in characters.enumerated() where character == "." || character == "!" || character == "?" {
            if character == ".", isAbbreviationPeriod(in: characters, at: index) {
                continue
            }
            var end = index + 1
            while end < characters.count && "\"'’”)]}»".contains(characters[end]) {
                end += 1
            }
            if end == characters.count || characters[end].isWhitespace {
                return end
            }
        }
        return nil
    }

    private static func isAbbreviationPeriod(in characters: [Character], at characterOffset: Int) -> Bool {
        guard characterOffset < characters.count, characters[characterOffset] == "." else {
            return false
        }
        let start = (0..<characterOffset).reversed()
            .first(where: { characters[$0].isWhitespace })
            .map { $0 + 1 } ?? 0
        let token = String(characters[start...characterOffset]).lowercased()
        if ["mr.", "mrs.", "ms.", "dr.", "prof.", "sr.", "jr.", "st.", "vs.", "etc."]
            .contains(token) {
            return true
        }
        let initials = token.split(separator: ".", omittingEmptySubsequences: true)
        return initials.count > 1 && initials.allSatisfy { $0.count == 1 && $0.first?.isLetter == true }
    }

    /// Estimate the number of audio tokens needed for text.
    /// At 12Hz, approximately 12 tokens = 1 second of audio.
    /// Average speech is ~150 words per minute = 2.5 words per second.
    /// So roughly 5 tokens per word.
    public static func estimateTokens(for text: String) -> Int {
        let wordCount = text.split(separator: " ").count
        return max(12, wordCount * 5)
    }
}
