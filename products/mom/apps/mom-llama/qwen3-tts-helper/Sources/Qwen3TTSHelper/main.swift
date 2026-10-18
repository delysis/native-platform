import Foundation
import MLX
import Qwen3TTS

private struct Result: Encodable {
    let samples: Int
    let sampleRate: Int
}

private struct Request: Decodable {
    let id: String
    let text: String
    let persona_id: String
    let ref_audio: String
    let ref_sha256: String
    let ref_text: String
    var warm_only: Bool? = nil
    var chunk_size: Int? = nil
    var audition_seed: UInt64? = nil
}

private struct ControlRequest: Decodable {
    let op: String?
    let id: String?
}

private enum ResidentInput {
    case request(Request)
    case invalid
}

private final class ResidentRequestQueue: @unchecked Sendable {
    private let condition = NSCondition()
    private var pending: [ResidentInput] = []
    private var closed = false

    func push(_ input: ResidentInput) {
        condition.lock()
        pending.append(input)
        condition.signal()
        condition.unlock()
    }

    func close() {
        condition.lock()
        closed = true
        condition.broadcast()
        condition.unlock()
    }

    func pop() -> ResidentInput? {
        condition.lock()
        defer { condition.unlock() }
        while pending.isEmpty && !closed { condition.wait() }
        return pending.isEmpty ? nil : pending.removeFirst()
    }
}

private final class RequestCancellationRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var active = Set<String>()
    private var cancelled = Set<String>()

    func begin(_ id: String) {
        lock.lock()
        active.insert(id)
        cancelled.remove(id)
        lock.unlock()
    }

    func cancel(_ id: String) {
        lock.lock()
        if active.contains(id) { cancelled.insert(id) }
        lock.unlock()
    }

    func contains(_ id: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled.contains(id)
    }

    func finish(_ id: String) {
        lock.lock()
        active.remove(id)
        cancelled.remove(id)
        lock.unlock()
    }
}

private struct EmbeddingRequest: Decodable {
    let id: String
    let audio: String
}

private struct WireEvent: Encodable {
    let event: String
    var id: String? = nil
    var sample_rate: Int? = nil
    var samples: Int? = nil
    var data: String? = nil
    var detail: String? = nil
    var reference_cache: String? = nil
    var reference_us: Int? = nil
    var reference_identity_us: Int? = nil
    var reference_memory_lookup_us: Int? = nil
    var reference_disk_lookup_us: Int? = nil
    var reference_source_read_us: Int? = nil
    var reference_source_hash_us: Int? = nil
    var reference_compute_us: Int? = nil
    var reference_persist_us: Int? = nil
    var text_chunk_us: Int? = nil
    var code_generation_us: Int? = nil
    var prepared_prefix_cache_hit: Bool? = nil
    var first_decode_us: Int? = nil
    var first_pcm_us: Int? = nil
    var embedding: [Float]? = nil
    var generation_segments: [GenerationSegmentReceipt]? = nil
}

private struct GenerationSegmentReceipt: Encodable {
    let segment: Int
    let stop_reason: String?
    let generated_codec_frames: Int?
    let max_tokens: Int?
}

private final class GenerationReportBox: @unchecked Sendable {
    private let lock = NSLock()
    private var report: TTSGenerationReport?

    func store(_ report: TTSGenerationReport) {
        lock.lock()
        self.report = report
        lock.unlock()
    }

    func load() -> TTSGenerationReport? {
        lock.lock()
        defer { lock.unlock() }
        return report
    }
}

private let residentProtocol = "mom-qwen3-tts-resident-v2"

@main
private enum Qwen3TTSHelperMain {
    static func main() async {
        do {
            let arguments = Array(CommandLine.arguments.dropFirst())
            if arguments == ["--protocol"] {
                print(residentProtocol)
            } else if arguments.first == "--resident", arguments.count == 2 {
                try await runResident(modelPath: URL(fileURLWithPath: arguments[1]))
            } else if arguments.first == "--embedding-resident", arguments.count == 2 {
                try runEmbeddingResident(modelPath: URL(fileURLWithPath: arguments[1]))
            } else if arguments.first == "--icl", arguments.count == 6 {
                try await runICL(
                    modelPath: URL(fileURLWithPath: arguments[1]),
                    referencePath: URL(fileURLWithPath: arguments[2]),
                    transcript: arguments[3],
                    outputPath: URL(fileURLWithPath: arguments[4]),
                    text: arguments[5]
                )
            } else if arguments.count == 4 {
                try runVoiceDesign(arguments: arguments)
            } else {
                fputs("usage: qwen3-tts-helper MODEL OUTPUT VOICE_DESCRIPTION TEXT\n", stderr)
                fputs("       qwen3-tts-helper --icl MODEL REF_AUDIO REF_TEXT OUTPUT TEXT\n", stderr)
                fputs("       qwen3-tts-helper --resident MODEL\n", stderr)
                fputs("       qwen3-tts-helper --embedding-resident MODEL\n", stderr)
                fputs("       qwen3-tts-helper --protocol\n", stderr)
                exit(2)
            }
        } catch {
            fputs("qwen3-tts synthesis failed: \(error)\n", stderr)
            exit(1)
        }
    }

    /// Offline archive worker. It deliberately exposes embeddings only through
    /// a separate protocol so diarization can never occupy or mutate the live
    /// synthesis resident used by calls.
    private static func runEmbeddingResident(modelPath: URL) throws {
        let pipeline = try Qwen3TTSPipeline(modelPath: modelPath)
        guard pipeline.supportsVoiceCloning else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 4,
                          userInfo: [NSLocalizedDescriptionKey: "Speaker encoder is unavailable"])
        }
        writeJSON(WireEvent(event: "ready"))
        while let line = readLine() {
            guard let data = line.data(using: .utf8) else { continue }
            do {
                let request = try JSONDecoder().decode(EmbeddingRequest.self, from: data)
                let wav = try Data(contentsOf: URL(fileURLWithPath: request.audio))
                let samples = try referenceSamples(data: wav)
                guard let embedding = pipeline.extractSpeakerEmbedding(audioSamples: samples),
                      !embedding.isEmpty else {
                    throw NSError(domain: "MomQwen3TTSHelper", code: 8,
                                  userInfo: [NSLocalizedDescriptionKey: "Could not extract speaker embedding"])
                }
                writeJSON(WireEvent(event: "embedding", id: request.id, embedding: embedding))
            } catch {
                writeJSON(WireEvent(event: "error", detail: String(describing: error)))
            }
        }
    }

    private static func runVoiceDesign(arguments: [String]) throws {
        let pipeline = try Qwen3TTSPipeline(modelPath: URL(fileURLWithPath: arguments[0]))
        let samples = pipeline.generateVoiceDesign(
            text: arguments[3],
            voiceDescription: arguments[2],
            temperature: 0.75,
            maxTokens: 2400
        )
        guard !samples.isEmpty else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 3,
                          userInfo: [NSLocalizedDescriptionKey: "synthesis returned no audio"])
        }
        try AudioSampleWriter.write(samples: samples, to: URL(fileURLWithPath: arguments[1]))
        writeJSON(Result(samples: samples.count, sampleRate: Qwen3TTSPipeline.sampleRate))
    }

    private static func runICL(
        modelPath: URL,
        referencePath: URL,
        transcript: String,
        outputPath: URL,
        text: String
    ) async throws {
        let pipeline = try Qwen3TTSPipeline(modelPath: modelPath)
        guard pipeline.supportsICL else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 4,
                          userInfo: [NSLocalizedDescriptionKey: "Base ICL audio encoder is unavailable"])
        }
        let codes = try referenceCodes(pipeline: pipeline, path: referencePath)
        let count = try await pipeline.generateToFile(
            text: text,
            referenceTranscript: transcript,
            referenceAudioCodes: codes,
            outputURL: outputPath
        )
        guard count > 0 else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 5,
                          userInfo: [NSLocalizedDescriptionKey: "ICL synthesis returned no audio"])
        }
        writeJSON(Result(samples: count, sampleRate: Qwen3TTSPipeline.sampleRate))
    }

    private static func runResident(modelPath: URL) async throws {
        let pipeline = try Qwen3TTSPipeline(modelPath: modelPath)
        var referenceVoiceCache = ReferenceVoiceCache(modelPath: modelPath)
        var warmedPrefixKeys = Set<String>()
        guard pipeline.supportsVoiceCloning else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 4,
                          userInfo: [NSLocalizedDescriptionKey: "Base ICL audio encoder is unavailable"])
        }
        writeJSON(WireEvent(event: "ready", sample_rate: Qwen3TTSPipeline.sampleRate))

        // Keep stdin consumption independent from the serial Metal owner.
        // A cancel command must remain readable while that owner is inside
        // MLX generation; otherwise cancellation waits behind the work it is
        // intended to stop and can wedge the resident worker.
        let queue = ResidentRequestQueue()
        let cancellations = RequestCancellationRegistry()
        Thread.detachNewThread {
            while let line = readLine() {
                guard let data = line.data(using: .utf8) else { continue }
                if let control = try? JSONDecoder().decode(ControlRequest.self, from: data),
                   control.op == "cancel", let id = control.id {
                    cancellations.cancel(id)
                    continue
                }
                if let request = try? JSONDecoder().decode(Request.self, from: data) {
                    cancellations.begin(request.id)
                    queue.push(.request(request))
                } else {
                    queue.push(.invalid)
                }
            }
            queue.close()
        }

        while let input = queue.pop() {
            guard case let .request(request) = input else {
                writeJSON(WireEvent(event: "fatal", detail: "Invalid request JSON"))
                continue
            }
            var generationSegments: [GenerationSegmentReceipt] = []
            var activeGeneration: (segment: Int, reports: GenerationReportBox)?
            do {
                let requestStarted = DispatchTime.now().uptimeNanoseconds
                let lookup = try referenceVoiceCache.voice(
                    path: URL(fileURLWithPath: request.ref_audio),
                    personaID: request.persona_id,
                    expectedDigest: request.ref_sha256
                ) { data in
                    let samples = try referenceSamples(data: data)
                    guard let speakerEmbedding = pipeline.extractSpeakerEmbedding(audioSamples: samples),
                          !speakerEmbedding.isEmpty else {
                        throw NSError(domain: "MomQwen3TTSHelper", code: 8,
                                      userInfo: [NSLocalizedDescriptionKey: "Could not extract reference speaker embedding"])
                    }
                    guard let decoderContext = pipeline.encodeReferenceAudio(audioSamples: samples),
                          !decoderContext.isEmpty else {
                        throw NSError(domain: "MomQwen3TTSHelper", code: 8,
                                      userInfo: [NSLocalizedDescriptionKey: "Could not encode reference decoder context"])
                    }
                    return CachedReferenceVoice(
                        speakerEmbedding: speakerEmbedding,
                        decoderContext: decoderContext
                    )
                }
                let voice = lookup.voice
                let preparedPrefixKey = "\(request.persona_id):\(request.ref_sha256)"
                if request.warm_only == true {
                    // Prime the talker and first vocoder emission once
                    // during background startup. Reference warmup alone left
                    // their first compilation on the user's first reply.
                    if warmedPrefixKeys.insert(preparedPrefixKey).inserted {
                        let warmStream = pipeline.generateStream(
                            text: "Ready.",
                            speakerEmbedding: voice.speakerEmbedding,
                            referenceTranscript: nil,
                            referenceAudioCodes: voice.decoderContext,
                            temperature: 0.65,
                            // One two-frame emission compiles and evaluates the
                            // talker, code predictor, and vocoder graphs. Running
                            // a conversational token budget here only occupies
                            // the single resident worker and can make an
                            // immediate foreground call wait behind startup.
                            maxTokens: 2,
                            chunkSize: 2,
                            preparedPrefixKey: preparedPrefixKey,
                            isCancelled: { cancellations.contains(request.id) }
                        )
                        for try await _ in warmStream {}
                    }
                    if cancellations.contains(request.id) {
                        writeJSON(WireEvent(event: "cancelled", id: request.id))
                        cancellations.finish(request.id)
                        continue
                    }
                    writeJSON(WireEvent(
                        event: "warmed",
                        id: request.id,
                        reference_cache: lookup.source.rawValue,
                        reference_us: lookup.timings.totalMicros,
                        reference_identity_us: lookup.timings.identityMicros,
                        reference_memory_lookup_us: lookup.timings.memoryLookupMicros,
                        reference_disk_lookup_us: lookup.timings.diskLookupMicros,
                        reference_source_read_us: lookup.timings.sourceReadMicros,
                        reference_source_hash_us: lookup.timings.sourceHashMicros,
                        reference_compute_us: lookup.timings.computeMicros,
                        reference_persist_us: lookup.timings.persistMicros
                    ))
                    cancellations.finish(request.id)
                    continue
                }
                let textChunkStarted = DispatchTime.now().uptimeNanoseconds
                // Live call/voice-note requests already arrive bounded by the
                // app scheduler and remain one acoustic segment. Read Aloud
                // can contain an entire long message, so split only above the
                // same 64-word ceiling while keeping one helper job and one
                // final terminal event for the streamed PCM.
                let speechChunks = TextChunker.boundedSpokenChunks(request.text, maxWords: 64)
                let textChunkMicros = elapsedMicros(since: textChunkStarted)
                guard !speechChunks.isEmpty else {
                    throw NSError(domain: "MomQwen3TTSHelper", code: 9,
                                  userInfo: [NSLocalizedDescriptionKey: "Synthesis text was empty"])
                }
                var firstPCM = true
                let chunkSize = min(8, max(2, request.chunk_size ?? 2))
                // Explicitly seeded audition requests make reference A/B
                // comparisons repeatable. App requests omit this field.
                if let seed = request.audition_seed { MLX.seed(seed) }
                for (segmentIndex, textChunk) in speechChunks.enumerated() {
                    // Reset generation at natural boundaries before acoustic
                    // drift can propagate. The cached speaker embedding keeps
                    // identity stable, and playback consumes earlier PCM while
                    // the following segment is generated.
                    // A large fixed floor let a missed EOS extend a tiny live
                    // phrase into seconds of repeated codec frames. Budget by
                    // text length and retain a small prosodic tail.
                    // Allow the app's bounded 64-word sentences to finish instead
                    // of exhausting the old short-phrase ceiling mid-word.
                    let maxTokens = min(384, max(24, TextChunker.estimateTokens(for: textChunk) + 12))
                    let generationReports = GenerationReportBox()
                    activeGeneration = (segmentIndex + 1, generationReports)
                    let stream = pipeline.generateStream(
                        text: textChunk,
                        speakerEmbedding: voice.speakerEmbedding,
                        referenceTranscript: nil,
                        // Identity is embedding-only without a transcript.
                        // The decoder renders generated codes only; reference
                        // waveforms must never be spliced into its onset.
                        referenceAudioCodes: voice.decoderContext,
                        // The native sampler has less numerical headroom than
                        // the reference CUDA path. This temperature preserves
                        // natural prosody without long-form instability.
                        temperature: 0.65,
                        maxTokens: maxTokens,
                        chunkSize: chunkSize,
                        preparedPrefixKey: preparedPrefixKey,
                        isCancelled: { cancellations.contains(request.id) },
                        onGenerationComplete: { generationReports.store($0) }
                    )
                    for try await chunk in stream {
                        if cancellations.contains(request.id) { break }
                        guard !chunk.samples.isEmpty else { continue }
                        let pcm = pcm16LE(chunk.samples)
                        writeJSON(WireEvent(
                            event: "audio",
                            id: request.id,
                            sample_rate: Qwen3TTSPipeline.sampleRate,
                            samples: chunk.samples.count,
                            data: pcm.base64EncodedString(),
                            reference_cache: firstPCM ? lookup.source.rawValue : nil,
                            reference_us: firstPCM ? lookup.timings.totalMicros : nil,
                            reference_identity_us: firstPCM ? lookup.timings.identityMicros : nil,
                            reference_memory_lookup_us: firstPCM ? lookup.timings.memoryLookupMicros : nil,
                            reference_disk_lookup_us: firstPCM ? lookup.timings.diskLookupMicros : nil,
                            reference_source_read_us: firstPCM ? lookup.timings.sourceReadMicros : nil,
                            reference_source_hash_us: firstPCM ? lookup.timings.sourceHashMicros : nil,
                            reference_compute_us: firstPCM ? lookup.timings.computeMicros : nil,
                            reference_persist_us: firstPCM ? lookup.timings.persistMicros : nil,
                            text_chunk_us: firstPCM ? textChunkMicros : nil,
                            code_generation_us: firstPCM
                                ? chunk.firstCodeGenerationMicros.map { clampedInt($0) } : nil,
                            prepared_prefix_cache_hit: firstPCM
                                ? chunk.preparedPrefixCacheHit : nil,
                            first_decode_us: firstPCM
                                ? chunk.firstDecodeMicros.map { clampedInt($0) } : nil,
                            first_pcm_us: firstPCM ? elapsedMicros(since: requestStarted) : nil
                        ))
                        firstPCM = false
                    }
                    let report = generationReports.load()
                    let missingReportReason = cancellations.contains(request.id) ? "cancelled" : nil
                    generationSegments.append(GenerationSegmentReceipt(
                        segment: segmentIndex + 1,
                        stop_reason: report?.stopReason.rawValue ?? missingReportReason,
                        generated_codec_frames: report?.generatedCodecFrames,
                        max_tokens: report?.maxTokens
                    ))
                    activeGeneration = nil
                    if cancellations.contains(request.id) { break }
                }
                writeJSON(WireEvent(
                    event: cancellations.contains(request.id) ? "cancelled" : "done",
                    id: request.id,
                    generation_segments: generationSegments
                ))
            } catch {
                if let activeGeneration {
                    let report = activeGeneration.reports.load()
                    generationSegments.append(GenerationSegmentReceipt(
                        segment: activeGeneration.segment,
                        stop_reason: report?.stopReason.rawValue ?? (cancellations.contains(request.id) ? "cancelled" : "error"),
                        generated_codec_frames: report?.generatedCodecFrames,
                        max_tokens: report?.maxTokens
                    ))
                }
                writeJSON(WireEvent(
                    event: cancellations.contains(request.id) ? "cancelled" : "error",
                    id: request.id,
                    detail: cancellations.contains(request.id) ? nil : String(describing: error),
                    generation_segments: generationSegments.isEmpty ? nil : generationSegments
                ))
            }
            cancellations.finish(request.id)
        }
    }

    private static func clampedInt(_ value: UInt64) -> Int {
        value > UInt64(Int.max) ? Int.max : Int(value)
    }

    private static func referenceCodes(pipeline: Qwen3TTSPipeline, path: URL) throws -> [[Int32]] {
        let data = try Data(contentsOf: path)
        return try referenceCodes(pipeline: pipeline, data: data)
    }

    fileprivate static func referenceCodes(
        pipeline: Qwen3TTSPipeline,
        data: Data
    ) throws -> [[Int32]] {
        let samples = try referenceSamples(data: data)
        guard let codes = pipeline.encodeReferenceAudio(audioSamples: samples), !codes.isEmpty else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 7,
                          userInfo: [NSLocalizedDescriptionKey: "Could not encode reference audio for ICL"])
        }
        return codes
    }

    fileprivate static func referenceSamples(data: Data) throws -> [Float] {
        guard data.count >= 44,
              data.prefix(4) == Data("RIFF".utf8),
              data.subdata(in: 8..<12) == Data("WAVE".utf8),
              data[22] == 1, data[23] == 0,
              data[24] == 0xC0, data[25] == 0x5D,
              data[34] == 16, data[35] == 0 else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 6,
                          userInfo: [NSLocalizedDescriptionKey: "Reference must be mono 24 kHz 16-bit PCM WAV"])
        }
        let samples = Qwen3TTSPipeline.wavToFloatSamples(data: data)
        guard !samples.isEmpty else {
            throw NSError(domain: "MomQwen3TTSHelper", code: 7,
                          userInfo: [NSLocalizedDescriptionKey: "Could not encode reference audio for ICL"])
        }
        return samples
    }

    private static func pcm16LE(_ samples: [Float]) -> Data {
        var data = Data(capacity: samples.count * 2)
        for sample in samples {
            let value = Int16((max(-1, min(1, sample)) * 32767).rounded()).littleEndian
            withUnsafeBytes(of: value) { data.append(contentsOf: $0) }
        }
        return data
    }

    private static func elapsedMicros(since started: UInt64) -> Int {
        let elapsed = DispatchTime.now().uptimeNanoseconds &- started
        return Int(min(elapsed / 1_000, UInt64(Int.max)))
    }

    private static func writeJSON<T: Encodable>(_ value: T) {
        guard let data = try? JSONEncoder().encode(value) else { return }
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([0x0A]))
    }
}
