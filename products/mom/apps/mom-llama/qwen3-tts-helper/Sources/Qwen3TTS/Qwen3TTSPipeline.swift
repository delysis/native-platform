import Foundation
import MLX
import MLXNN

private actor StreamingBatchGate {
    private var permit = false
    private var waiter: CheckedContinuation<Void, Never>?

    func wait() async {
        if permit {
            permit = false
            return
        }
        await withCheckedContinuation { continuation in
            precondition(waiter == nil, "only one streaming producer may wait")
            waiter = continuation
        }
    }

    func signal() {
        if let waiter {
            self.waiter = nil
            waiter.resume()
        } else {
            permit = true
        }
    }
}

/// Emit two codec frames first: enough for about 139 ms of PCM after retaining
/// the crossfade tail, without waiting for the five-frame steady cadence.
/// Rejoin that cadence at frame five and preserve the stable decoder windows.
func streamingFramesToNextEmission(emittedFrames: Int) -> Int {
    if emittedFrames == 0 { return 2 }
    if emittedFrames < 5 { return 5 - emittedFrames }
    if emittedFrames < 48 { return min(5, 48 - emittedFrames) }
    return 12
}

/// A chunk of generated audio for streaming playback.
public struct AudioChunk: Sendable {
    /// Audio samples (Float, 24kHz)
    public let samples: [Float]
    /// Range of tokens in this chunk
    public let tokenRange: Range<Int>
    /// Whether this is the final chunk
    public let isFinal: Bool
    /// Time from the streaming task starting until its first codec-code batch
    /// became materialized on the GPU. Present only on the first PCM chunk.
    public let firstCodeGenerationMicros: UInt64?
    /// Whether the resident speaker/role KV prefix was already materialized.
    /// Present only on the first PCM chunk.
    public let preparedPrefixCacheHit: Bool?
    /// Wall time spent decoding the first emitted codec window into PCM.
    /// Present only on the first PCM chunk.
    public let firstDecodeMicros: UInt64?

    public init(
        samples: [Float],
        tokenRange: Range<Int>,
        isFinal: Bool,
        firstCodeGenerationMicros: UInt64? = nil,
        preparedPrefixCacheHit: Bool? = nil,
        firstDecodeMicros: UInt64? = nil
    ) {
        self.samples = samples
        self.tokenRange = tokenRange
        self.isFinal = isFinal
        self.firstCodeGenerationMicros = firstCodeGenerationMicros
        self.preparedPrefixCacheHit = preparedPrefixCacheHit
        self.firstDecodeMicros = firstDecodeMicros
    }
}

/// Configuration for the Qwen3 TTS pipeline.
public struct Qwen3TTSPipelineConfiguration: Sendable {
    /// Whether to apply mixed quantization at runtime (for non-pre-quantized models).
    /// Pre-quantized models (those with `quantization` in config.json) ignore this.
    public var applyRuntimeQuantization: Bool

    /// Default generation temperature (0.0-1.0). Higher = more varied.
    public var defaultTemperature: Float

    /// Maximum tokens per generation chunk (~12 tokens = 1 second at 12Hz).
    public var defaultMaxTokens: Int

    /// Default chunk size for streaming (frames per yield).
    public var defaultStreamingChunkSize: Int

    /// Number of samples to crossfade between text chunks (default 480 = 20ms at 24kHz).
    public var crossfadeSamples: Int

    public init(
        applyRuntimeQuantization: Bool = true,
        defaultTemperature: Float = 0.85,
        defaultMaxTokens: Int = 2400,
        defaultStreamingChunkSize: Int = 12,
        crossfadeSamples: Int = 480
    ) {
        self.applyRuntimeQuantization = applyRuntimeQuantization
        self.defaultTemperature = defaultTemperature
        self.defaultMaxTokens = defaultMaxTokens
        self.defaultStreamingChunkSize = defaultStreamingChunkSize
        self.crossfadeSamples = crossfadeSamples
    }

    public static let `default` = Qwen3TTSPipelineConfiguration()
}

/// High-level pipeline for Qwen3 text-to-speech generation.
///
/// Usage:
/// ```swift
/// let pipeline = try Qwen3TTSPipeline(modelPath: modelURL)
/// let samples = pipeline.generate(text: "Hello world", speaker: "Aiden")
/// ```
public final class Qwen3TTSPipeline: @unchecked Sendable {
    /// Audio sample rate in Hz.
    public static let sampleRate: Int = 24000

    private let model: Qwen3Talker
    private let tokenizer: Qwen3Tokenizer
    private let decoder: AudioDecoder
    private let speakerEncoder: SpeakerEncoder?
    private let audioEncoder: Qwen3TTSAudioEncoder?
    private let config: Qwen3TTSConfig
    private let device: Device
    private let pipelineConfig: Qwen3TTSPipelineConfiguration
    private var personaAdapterController: PersonaAdapterController?

    /// Available built-in speaker names (from the model's spk_id map).
    public var availableSpeakers: [String] {
        config.spk_id.keys.sorted()
    }

    /// Whether voice cloning via speaker embeddings is supported.
    public var supportsVoiceCloning: Bool {
        speakerEncoder?.isWeightsLoaded ?? false
    }

    /// Whether ICL (in-context learning) audio encoding is available.
    public var supportsICL: Bool {
        audioEncoder != nil
    }

    /// The raw model type from config (nil for base, "voice_design", "custom_voice").
    public var modelType: String? {
        config.tts_model_type
    }

    /// Whether this model supports VoiceDesign (generating voices from text descriptions).
    public var supportsVoiceDesign: Bool {
        config.tts_model_type == "voice_design"
    }

    /// Whether this model supports CustomVoice (named speakers with instruct/style control).
    public var supportsCustomVoice: Bool {
        config.tts_model_type == "custom_voice"
    }

    /// Load a Qwen3 TTS model from a local directory.
    ///
    /// The directory must contain:
    /// - `config.json` — model configuration
    /// - `model.safetensors` — model weights
    /// - `tokenizer.json` — BPE tokenizer
    /// - `speech_tokenizer/` — vocoder subdirectory with its own config.json and model.safetensors
    ///
    /// - Parameters:
    ///   - modelPath: Path to the model directory
    ///   - configuration: Pipeline configuration options
    /// - Throws: If any required file is missing or loading fails
    public init(modelPath: URL, configuration: Qwen3TTSPipelineConfiguration = .default) throws {
        self.pipelineConfig = configuration

        let device = DeviceSelector.resolveDevice()
        self.device = device

        // Load in the resolved device context
        let result: (Qwen3Talker, Qwen3Tokenizer, AudioDecoder, SpeakerEncoder?, Qwen3TTSAudioEncoder?, Qwen3TTSConfig) = try Device.withDefaultDevice(device) {
            // Parse config
            let configURL = modelPath.appendingPathComponent("config.json")
            guard FileManager.default.fileExists(atPath: configURL.path) else {
                throw Qwen3TTSError.fileNotFound("config.json")
            }
            let configData = try Data(contentsOf: configURL)
            let modelConfig = try JSONDecoder().decode(Qwen3TTSConfig.self, from: configData)

            // Load tokenizer
            let tokenizer = Qwen3Tokenizer(modelPath: modelPath)

            // Load model weights (always on CPU first)
            let weightsURL = modelPath.appendingPathComponent("model.safetensors")
            guard FileManager.default.fileExists(atPath: weightsURL.path) else {
                throw Qwen3TTSError.fileNotFound("model.safetensors")
            }
            var weights = try MLX.loadArrays(url: weightsURL, stream: .cpu)

            let model = Qwen3Talker(config: modelConfig)

            // Evaluate random init weights, then load actual weights
            eval(model.parameters())
            DeviceSelector.synchronizeIfNeeded(device: device)
            Memory.clearCache()

            try model.load(weights: weights)
            DeviceSelector.synchronizeIfNeeded(device: device)
            Memory.clearCache()

            // Load speaker encoder if weights are present
            var speakerEncoder: SpeakerEncoder? = nil
            let hasSpeakerEncoderWeights = weights.keys.contains { $0.hasPrefix("speaker_encoder.") }
            if hasSpeakerEncoderWeights {
                let spkEncoder = SpeakerEncoder()
                eval(spkEncoder.parameters())
                Memory.clearCache()

                spkEncoder.load(weights: weights)
                DeviceSelector.synchronizeIfNeeded(device: device)
                Memory.clearCache()

                if spkEncoder.isWeightsLoaded {
                    speakerEncoder = spkEncoder
                }
            }

            // Materialize all weights in GPU memory before freeing source
            eval(model.parameters())
            if let spkEnc = speakerEncoder {
                eval(spkEnc.parameters())
            }
            DeviceSelector.synchronizeIfNeeded(device: device)

            // Free original weights
            weights = [:]
            Memory.clearCache()

            // Apply runtime quantization if needed
            if modelConfig.quantization == nil && configuration.applyRuntimeQuantization {
                Qwen3TTSPipeline.applyMixedQuantization(to: model)
                DeviceSelector.synchronizeIfNeeded(device: device)
                Memory.clearCache()
            }

            // Load speech tokenizer (vocoder)
            let speechTokenizerURL = modelPath.appendingPathComponent("speech_tokenizer")
            let speechConfigCandidates = [
                speechTokenizerURL.appendingPathComponent("config.json"),
                speechTokenizerURL.appendingPathComponent("configuration.json"),
                speechTokenizerURL.appendingPathComponent("speech_tokenizer_config.json")
            ]
            guard let speechConfigURL = speechConfigCandidates.first(where: { FileManager.default.fileExists(atPath: $0.path) }) else {
                throw Qwen3TTSError.fileNotFound("speech_tokenizer/config.json")
            }
            let audioConfigData = try Data(contentsOf: speechConfigURL)
            let audioConfig = try JSONDecoder().decode(AudioDecoderConfig.self, from: audioConfigData)

            let speechWeightsURL = speechTokenizerURL.appendingPathComponent("model.safetensors")
            let decoder = AudioDecoder(config: audioConfig)
            let mlxLoaded = decoder.loadMLXDecoder(configURL: speechConfigURL, weightsURL: speechWeightsURL)
            guard mlxLoaded else {
                throw Qwen3TTSError.decoderLoadFailed
            }

            // Load audio encoder for ICL (optional)
            var audioEncoder: Qwen3TTSAudioEncoder? = nil
            do {
                let encoder = Qwen3TTSAudioEncoder(
                    config: audioConfig.encoder_config ?? Qwen3TTSTokenizerEncoderConfig(),
                    validNumQuantizers: audioConfig.encoder_valid_num_quantizers ?? 16
                )
                try encoder.loadWeights(from: speechWeightsURL)
                audioEncoder = encoder
            } catch {
                if modelConfig.tts_model_type == nil || modelConfig.tts_model_type == "base" {
                    throw Qwen3TTSError.audioEncoderLoadFailed(error.localizedDescription)
                }
            }

            DeviceSelector.synchronizeIfNeeded(device: device)
            Memory.clearCache()

            return (model, tokenizer, decoder, speakerEncoder, audioEncoder, modelConfig)
        }

        self.model = result.0
        self.tokenizer = result.1
        self.decoder = result.2
        self.speakerEncoder = result.3
        self.audioEncoder = result.4
        self.config = result.5
    }

    // MARK: - Simple Generation

    /// Generate speech from text using a built-in speaker name.
    ///
    /// - Parameters:
    ///   - text: Text to synthesize
    ///   - speaker: Speaker name (e.g., "Aiden", "Serena"). Case-insensitive.
    ///   - temperature: Generation temperature (default from configuration)
    ///   - maxTokens: Maximum tokens to generate (default from configuration)
    /// - Returns: Audio samples at 24kHz
    public func generate(
        text: String,
        speaker: String,
        temperature: Float? = nil,
        maxTokens: Int? = nil
    ) -> [Float] {
        let temp = temperature ?? pipelineConfig.defaultTemperature
        let tokens = maxTokens ?? pipelineConfig.defaultMaxTokens

        return Device.withDefaultDevice(device) {
            defer {
                model.clearGenerationCache()
                decoder.clearCompiledCache()
                DeviceSelector.synchronizeIfNeeded(device: device)
                Memory.clearCache()
            }
            return model.generate(
                prompt: speaker,
                text: text,
                tokenizer: tokenizer,
                decoder: decoder,
                temperature: temp,
                maxTokens: tokens
            )
        }
    }

    /// Generate speech from text using a speaker embedding (voice cloning).
    ///
    /// - Parameters:
    ///   - text: Text to synthesize
    ///   - speakerEmbedding: 1024-dimensional speaker embedding
    ///   - temperature: Generation temperature (default from configuration)
    ///   - maxTokens: Maximum tokens to generate (default from configuration)
    /// - Returns: Audio samples at 24kHz
    public func generate(
        text: String,
        speakerEmbedding: [Float],
        temperature: Float? = nil,
        maxTokens: Int? = nil
    ) -> [Float] {
        let temp = temperature ?? pipelineConfig.defaultTemperature
        let tokens = maxTokens ?? pipelineConfig.defaultMaxTokens

        return Device.withDefaultDevice(device) {
            defer {
                model.clearGenerationCache()
                decoder.clearCompiledCache()
                DeviceSelector.synchronizeIfNeeded(device: device)
                Memory.clearCache()
            }
            let embed = MLXArray(speakerEmbedding)
            return model.generate(
                prompt: "",
                text: text,
                speakerEmbedding: embed,
                tokenizer: tokenizer,
                decoder: decoder,
                temperature: temp,
                maxTokens: tokens
            )
        }
    }

    // MARK: - Streaming Generation

    /// Stream audio chunks for low-latency playback.
    ///
    /// Uses buffer-and-batch approach: accumulates codes until a decode chunk is ready,
    /// then decodes and yields audio samples.
    ///
    /// - Parameters:
    ///   - text: Text to synthesize
    ///   - speaker: Speaker name (case-insensitive)
    ///   - speakerEmbedding: Optional speaker embedding for voice cloning (overrides speaker name)
    ///   - temperature: Generation temperature
    ///   - maxTokens: Maximum tokens to generate
    ///   - chunkSize: Frames per streaming yield
    /// - Returns: AsyncThrowingStream of AudioChunk
    public func generateStream(
        text: String,
        speaker: String = "",
        speakerEmbedding: [Float]? = nil,
        referenceTranscript: String? = nil,
        referenceAudioCodes: [[Int32]]? = nil,
        temperature: Float? = nil,
        maxTokens: Int? = nil,
        chunkSize: Int? = nil,
        preparedPrefixKey: String? = nil,
        isCancelled: (@Sendable () -> Bool)? = nil
    ) -> AsyncThrowingStream<AudioChunk, Error> {
        _generateStreamImpl(
            text: text,
            speaker: speaker,
            instruct: nil,
            speakerEmbedding: speakerEmbedding,
            referenceTranscript: referenceTranscript,
            referenceAudioCodes: referenceAudioCodes,
            temperature: temperature,
            maxTokens: maxTokens,
            chunkSize: chunkSize,
            preparedPrefixKey: preparedPrefixKey,
            isCancelled: isCancelled
        )
    }

    // MARK: - VoiceDesign Generation

    /// Generate speech using a voice description (VoiceDesign model only).
    ///
    /// The model generates a voice matching the text description, without requiring a speaker ID.
    /// Requires a model with `tts_model_type == "voice_design"`.
    ///
    /// - Parameters:
    ///   - text: Text to synthesize
    ///   - voiceDescription: Natural language description of the desired voice (e.g., "deep male voice with British accent")
    ///   - temperature: Generation temperature
    ///   - maxTokens: Maximum tokens to generate
    /// - Returns: Audio samples at 24kHz
    public func generateVoiceDesign(
        text: String,
        voiceDescription: String,
        temperature: Float? = nil,
        maxTokens: Int? = nil
    ) -> [Float] {
        let temp = temperature ?? pipelineConfig.defaultTemperature
        let tokens = maxTokens ?? pipelineConfig.defaultMaxTokens

        return Device.withDefaultDevice(device) {
            defer {
                model.clearGenerationCache()
                decoder.clearCompiledCache()
                DeviceSelector.synchronizeIfNeeded(device: device)
                Memory.clearCache()
            }
            return model.generate(
                prompt: "",
                text: text,
                instruct: voiceDescription,
                tokenizer: tokenizer,
                decoder: decoder,
                temperature: temp,
                maxTokens: tokens
            )
        }
    }

    /// Stream audio using a voice description (VoiceDesign model only).
    ///
    /// - Parameters:
    ///   - text: Text to synthesize
    ///   - voiceDescription: Natural language description of the desired voice
    ///   - temperature: Generation temperature
    ///   - maxTokens: Maximum tokens to generate
    ///   - chunkSize: Frames per streaming yield
    /// - Returns: AsyncThrowingStream of AudioChunk
    public func generateStreamVoiceDesign(
        text: String,
        voiceDescription: String,
        temperature: Float? = nil,
        maxTokens: Int? = nil,
        chunkSize: Int? = nil
    ) -> AsyncThrowingStream<AudioChunk, Error> {
        _generateStreamImpl(
            text: text,
            speaker: "",
            instruct: voiceDescription,
            speakerEmbedding: nil,
            referenceTranscript: nil,
            referenceAudioCodes: nil,
            temperature: temperature,
            maxTokens: maxTokens,
            chunkSize: chunkSize,
            preparedPrefixKey: nil
        )
    }

    // MARK: - CustomVoice Generation

    /// Generate speech with a named speaker and style/emotion instruct (CustomVoice model only).
    ///
    /// Combines a built-in speaker identity with an instruct prompt for style control.
    /// Requires a model with `tts_model_type == "custom_voice"`.
    ///
    /// - Parameters:
    ///   - text: Text to synthesize
    ///   - speaker: Speaker name (e.g., "Vivian")
    ///   - instruct: Style/emotion instruction (e.g., "Say it angrily")
    ///   - temperature: Generation temperature
    ///   - maxTokens: Maximum tokens to generate
    /// - Returns: Audio samples at 24kHz
    public func generateCustomVoice(
        text: String,
        speaker: String,
        instruct: String,
        temperature: Float? = nil,
        maxTokens: Int? = nil
    ) -> [Float] {
        let temp = temperature ?? pipelineConfig.defaultTemperature
        let tokens = maxTokens ?? pipelineConfig.defaultMaxTokens

        return Device.withDefaultDevice(device) {
            defer {
                model.clearGenerationCache()
                decoder.clearCompiledCache()
                DeviceSelector.synchronizeIfNeeded(device: device)
                Memory.clearCache()
            }
            return model.generate(
                prompt: speaker,
                text: text,
                instruct: instruct,
                tokenizer: tokenizer,
                decoder: decoder,
                temperature: temp,
                maxTokens: tokens
            )
        }
    }

    /// Stream audio with a named speaker and style/emotion instruct (CustomVoice model only).
    ///
    /// - Parameters:
    ///   - text: Text to synthesize
    ///   - speaker: Speaker name
    ///   - instruct: Style/emotion instruction
    ///   - temperature: Generation temperature
    ///   - maxTokens: Maximum tokens to generate
    ///   - chunkSize: Frames per streaming yield
    /// - Returns: AsyncThrowingStream of AudioChunk
    public func generateStreamCustomVoice(
        text: String,
        speaker: String,
        instruct: String,
        temperature: Float? = nil,
        maxTokens: Int? = nil,
        chunkSize: Int? = nil
    ) -> AsyncThrowingStream<AudioChunk, Error> {
        _generateStreamImpl(
            text: text,
            speaker: speaker,
            instruct: instruct,
            speakerEmbedding: nil,
            referenceTranscript: nil,
            referenceAudioCodes: nil,
            temperature: temperature,
            maxTokens: maxTokens,
            chunkSize: chunkSize,
            preparedPrefixKey: nil
        )
    }

    // MARK: - Streaming Implementation

    private func _generateStreamImpl(
        text: String,
        speaker: String,
        instruct: String?,
        speakerEmbedding: [Float]?,
        referenceTranscript: String?,
        referenceAudioCodes: [[Int32]]?,
        temperature: Float?,
        maxTokens: Int?,
        chunkSize: Int?,
        preparedPrefixKey: String?,
        isCancelled: (@Sendable () -> Bool)? = nil
    ) -> AsyncThrowingStream<AudioChunk, Error> {
        let temp = temperature ?? pipelineConfig.defaultTemperature
        let tokens = maxTokens ?? pipelineConfig.defaultMaxTokens
        let chunk = chunkSize ?? pipelineConfig.defaultStreamingChunkSize
        let numCodeGroups = config.code_predictor_config.num_code_groups

        let capturedModel = model
        let capturedTokenizer = tokenizer
        let capturedDecoder = decoder
        let capturedDevice = device
        let batchGate = StreamingBatchGate()

        return AsyncThrowingStream { continuation in
            Task {
                do {
                    let streamingStarted = DispatchTime.now().uptimeNanoseconds
                    let preparedPrefixCacheHit = preparedPrefixKey.map {
                        capturedModel.hasPreparedPrefix(for: $0)
                    }
                    let speakerEmbed = speakerEmbedding.map { MLXArray($0) }
                    let codeStream = Device.withDefaultDevice(capturedDevice) {
                        capturedModel.generateStream(
                            prompt: speaker,
                            text: text,
                            instruct: instruct,
                            speakerEmbedding: speakerEmbed,
                            referenceTranscript: referenceTranscript,
                            referenceAudioCodes: referenceAudioCodes,
                            tokenizer: capturedTokenizer,
                            temperature: temp,
                            maxTokens: tokens,
                            chunkSize: chunk,
                            adaptiveChunking: true,
                            preparedPrefixKey: preparedPrefixKey,
                            isCancelled: isCancelled,
                            afterYield: { await batchGate.wait() }
                        )
                    }

                    // Two-phase decode: establish intelligible audio quickly,
                    // then use a larger rolling window for stable sustained
                    // speech. Every boundary retains and crossfades 21 ms;
                    // concatenating independently decoded tiny windows caused
                    // the severe metallic/garbled output heard in the app.
                    let firstDecodeWindow = 32
                    let steadyDecodeWindow = 80
                    let firstPhaseFrames = 48
                    let samplesPerFrame = 1920
                    let overlapSamples = 512

                    var generatedCodes: [[Int32]] = []
                    var emittedFrames = 0
                    var pcmOverlap = StreamingPCMOverlap(overlapSamples: overlapSamples)
                    var firstCodeGenerationMicros: UInt64?
                    var firstDecodeMicros: UInt64?
                    var emittedFirstPcmMetrics = false

                    func cleanSamples(_ samples: [Float]) -> [Float] {
                        samples.map { value in
                            if value.isNaN || value.isInfinite { return 0 }
                            return max(-1, min(1, value))
                        }
                    }

                    func decodeWindow(end: Int, newFrames: Int, windowSize: Int) -> [Float] {
                        guard end > 0, newFrames > 0 else { return [] }
                        let start = max(0, end - windowSize)
                        // Speaker/reference conditioning belongs to the talker.
                        // Prepending unrelated reference codes here leaks their
                        // waveform into the causal decoder's new-audio onset.
                        let decodeInput = Array(generatedCodes[start..<end])
                        let allSamples = Device.withDefaultDevice(capturedDevice) { () -> [Float] in
                            defer { DeviceSelector.synchronizeIfNeeded(device: capturedDevice) }
                            let flatCodes: [Int32] = decodeInput.flatMap { $0 }
                            let codesArray = MLXArray(flatCodes).reshaped([1, decodeInput.count, numCodeGroups])
                            let audio = capturedDecoder.mlxDecode(codes: codesArray)
                            let flatAudio = audio.reshaped([-1])
                            eval(flatAudio)
                            return flatAudio.asArray(Float.self)
                        }
                        // Re-decode the withheld tail too: the crossfade must blend
                        // two versions of the same samples, never adjacent audio.
                        let wanted = min(allSamples.count, pcmOverlap.requiredSampleCount(
                            newSampleCount: newFrames * samplesPerFrame
                        ))
                        return wanted > 0 ? cleanSamples(Array(allSamples.suffix(wanted))) : []
                    }

                    var requestCancelled = false
                    for try await codes in codeStream {
                        if Task.isCancelled || isCancelled?() == true {
                            requestCancelled = true
                            await batchGate.signal()
                            break
                        }
                        if codes.isEmpty {
                            await batchGate.signal()
                            continue
                        }

                        let validCodes = codes.filter { frame in
                            guard let firstCode = frame.first else { return false }
                            return firstCode >= 0 && firstCode < 2048
                        }
                        if validCodes.isEmpty {
                            await batchGate.signal()
                            continue
                        }

                        generatedCodes.append(contentsOf: validCodes)
                        if firstCodeGenerationMicros == nil {
                            firstCodeGenerationMicros =
                                (DispatchTime.now().uptimeNanoseconds - streamingStarted) / 1_000
                        }

                        while true {
                            if Task.isCancelled || isCancelled?() == true {
                                requestCancelled = true
                                break
                            }
                            let firstPhase = emittedFrames < firstPhaseFrames
                            let emitFrames = streamingFramesToNextEmission(
                                emittedFrames: emittedFrames
                            )
                            guard generatedCodes.count - emittedFrames >= emitFrames else { break }
                            let end = emittedFrames + emitFrames
                            let window = firstPhase ? firstDecodeWindow : steadyDecodeWindow
                            let decodeStarted = DispatchTime.now().uptimeNanoseconds
                            let decoded = decodeWindow(end: end, newFrames: emitFrames, windowSize: window)
                            if Task.isCancelled || isCancelled?() == true {
                                requestCancelled = true
                                break
                            }
                            if firstDecodeMicros == nil {
                                firstDecodeMicros =
                                    (DispatchTime.now().uptimeNanoseconds - decodeStarted) / 1_000
                            }
                            let samples = pcmOverlap.append(decoded, final: false)
                            let tokenRange = emittedFrames..<end
                            emittedFrames = end
                            if !samples.isEmpty {
                                continuation.yield(AudioChunk(
                                    samples: samples,
                                    tokenRange: tokenRange,
                                    isFinal: false,
                                    firstCodeGenerationMicros: emittedFirstPcmMetrics
                                        ? nil : firstCodeGenerationMicros,
                                    preparedPrefixCacheHit: emittedFirstPcmMetrics
                                        ? nil : preparedPrefixCacheHit,
                                    firstDecodeMicros: emittedFirstPcmMetrics
                                        ? nil : firstDecodeMicros
                                ))
                                emittedFirstPcmMetrics = true
                            }
                        }
                        await batchGate.signal()
                        if requestCancelled { break }
                    }

                    if requestCancelled || Task.isCancelled || isCancelled?() == true {
                        capturedModel.clearGenerationCache()
                        continuation.finish()
                        return
                    }

                    let remaining = generatedCodes.count - emittedFrames
                    var finalSamples: [Float]
                    if remaining > 0 {
                        let decodeStarted = DispatchTime.now().uptimeNanoseconds
                        let decoded = decodeWindow(
                            end: generatedCodes.count,
                            newFrames: remaining,
                            windowSize: steadyDecodeWindow
                        )
                        if firstDecodeMicros == nil {
                            firstDecodeMicros =
                                (DispatchTime.now().uptimeNanoseconds - decodeStarted) / 1_000
                        }
                        finalSamples = pcmOverlap.append(decoded, final: true)
                    } else {
                        finalSamples = pcmOverlap.finish()
                    }
                    if !finalSamples.isEmpty {
                        continuation.yield(AudioChunk(
                            samples: finalSamples,
                            tokenRange: emittedFrames..<generatedCodes.count,
                            isFinal: true,
                            firstCodeGenerationMicros: emittedFirstPcmMetrics
                                ? nil : firstCodeGenerationMicros,
                            preparedPrefixCacheHit: emittedFirstPcmMetrics
                                ? nil : preparedPrefixCacheHit,
                            firstDecodeMicros: emittedFirstPcmMetrics ? nil : firstDecodeMicros
                        ))
                        emittedFirstPcmMetrics = true
                    }

                    continuation.yield(AudioChunk(
                        samples: [],
                        tokenRange: generatedCodes.count..<generatedCodes.count,
                        isFinal: true,
                        firstCodeGenerationMicros: nil,
                        preparedPrefixCacheHit: nil,
                        firstDecodeMicros: nil
                    ))

                    capturedModel.clearGenerationCache()

                    continuation.finish()
                } catch {
                    capturedModel.clearGenerationCache()
                    continuation.finish(throwing: error)
                }
            }
        }
    }

    // MARK: - File Output (Memory-Efficient)

    /// Generate speech and write directly to a WAV file.
    ///
    /// This method is memory-efficient for long text: it chunks the text at natural boundaries,
    /// generates each chunk independently, and writes incrementally to disk.
    ///
    /// - Parameters:
    ///   - text: Text to synthesize (any length)
    ///   - speaker: Speaker name
    ///   - speakerEmbedding: Optional speaker embedding for voice cloning
    ///   - referenceTranscript: Optional transcript for ICL voice cloning
    ///   - referenceAudioCodes: Optional pre-encoded reference audio for ICL
    ///   - outputURL: File URL to write the WAV to
    ///   - temperature: Generation temperature
    ///   - onProgress: Optional progress callback (0.0 to 1.0)
    /// - Returns: Total number of samples written
    /// - Throws: If writing fails
    public func generateToFile(
        text: String,
        speaker: String = "",
        instruct: String? = nil,
        speakerEmbedding: [Float]? = nil,
        referenceTranscript: String? = nil,
        referenceAudioCodes: [[Int32]]? = nil,
        outputURL: URL,
        temperature: Float? = nil,
        onProgress: ((Float) -> Void)? = nil
    ) async throws -> Int {
        let temp = temperature ?? pipelineConfig.defaultTemperature
        let numCodeGroups = config.code_predictor_config.num_code_groups

        let textChunks = TextChunker.chunk(text, maxWords: TextChunker.defaultMaxWords)
        guard !textChunks.isEmpty else { return 0 }

        let embeddingData = speakerEmbedding

        return try await Task.detached(priority: .userInitiated) { [self] in
            try Device.withDefaultDevice(self.device) {
                let writer = try StreamingWAVWriter(to: outputURL)
                let speakerEmbed: MLXArray? = embeddingData.map { MLXArray($0) }

                for (chunkIndex, textChunk) in textChunks.enumerated() {
                    if Task.isCancelled {
                        _ = writer.finalize()
                        return writer.sampleCount
                    }

                    let progress = Float(chunkIndex) / Float(textChunks.count)
                    onProgress?(progress)

                    self.model.clearGenerationCache()

                    var codes: [[Int32]] = []
                    autoreleasepool {
                        codes = self.model.generateCodes(
                            prompt: speaker,
                            text: textChunk,
                            instruct: instruct,
                            speakerEmbedding: speakerEmbed,
                            referenceTranscript: referenceTranscript,
                            referenceAudioCodes: referenceAudioCodes,
                            tokenizer: self.tokenizer,
                            temperature: temp,
                            maxTokens: 600
                        )
                    }

                    Stream.defaultStream(.gpu).synchronize()
                    Memory.clearCache()

                    guard !codes.isEmpty else { continue }

                    // Decode in small batches
                    let samplesPerFrame = 1920
                    let decodeChunkSize = 16
                    let leftContextSize = 8

                    var chunkSamples: [Float] = []
                    chunkSamples.reserveCapacity(codes.count * samplesPerFrame)
                    var decodeLeftContext: [[Int32]] = []
                    var pos = 0

                    while pos < codes.count {
                        autoreleasepool {
                            let endPos = min(pos + decodeChunkSize, codes.count)
                            let batchCodes = decodeLeftContext + Array(codes[pos..<endPos])

                            let flatCodes: [Int32] = batchCodes.flatMap { $0 }
                            let codesArray = MLXArray(flatCodes).reshaped([1, batchCodes.count, numCodeGroups])
                            let audio = self.decoder.mlxDecode(codes: codesArray)
                            let flatAudio = audio.reshaped([-1])
                            eval(flatAudio)
                            var batchSamples = flatAudio.asArray(Float.self)

                            let contextSamples = decodeLeftContext.count * samplesPerFrame
                            if contextSamples > 0 && batchSamples.count > contextSamples {
                                batchSamples = Array(batchSamples.dropFirst(contextSamples))
                            }

                            for sample in batchSamples {
                                if sample.isNaN || sample.isInfinite {
                                    chunkSamples.append(0.0)
                                } else {
                                    chunkSamples.append(max(-1.0, min(1.0, sample)))
                                }
                            }

                            decodeLeftContext = Array(codes[max(0, endPos - leftContextSize)..<endPos])
                            pos = endPos
                        }

                        Stream.defaultStream(.gpu).synchronize()
                        Memory.clearCache()
                    }

                    guard !chunkSamples.isEmpty else { continue }
                    try writer.write(samples: chunkSamples)
                    chunkSamples = []

                    self.model.clearGenerationCache()
                    self.decoder.clearCompiledCache()
                    Stream.defaultStream(.gpu).synchronize()
                    Memory.clearCache()
                }

                onProgress?(1.0)
                let result = writer.finalize()
                return result.sampleCount
            }
        }.value
    }

    // MARK: - Batch Generation (Silent)

    /// Generate speech for text of any length, returning all samples at once.
    ///
    /// Uses text chunking and crossfading for seamless output.
    /// For very long text, prefer `generateToFile` for lower memory usage.
    ///
    /// - Parameters:
    ///   - text: Text to synthesize (any length)
    ///   - speaker: Speaker name
    ///   - speakerEmbedding: Optional speaker embedding for voice cloning
    ///   - referenceTranscript: Optional transcript for ICL
    ///   - temperature: Generation temperature
    ///   - onProgress: Optional progress callback (0.0 to 1.0)
    /// - Returns: Audio samples at 24kHz
    public func generateBatch(
        text: String,
        speaker: String = "",
        instruct: String? = nil,
        speakerEmbedding: [Float]? = nil,
        referenceTranscript: String? = nil,
        temperature: Float? = nil,
        onProgress: ((Float) -> Void)? = nil
    ) async -> [Float] {
        let temp = temperature ?? pipelineConfig.defaultTemperature
        let crossfade = pipelineConfig.crossfadeSamples
        let numCodeGroups = config.code_predictor_config.num_code_groups

        let textChunks = TextChunker.chunk(text, maxWords: TextChunker.defaultMaxWords)
        guard !textChunks.isEmpty else { return [] }

        // Short text: single generation
        if textChunks.count == 1 {
            onProgress?(0.0)
            let samples = generate(text: textChunks[0], speaker: speaker, temperature: temp)
            onProgress?(1.0)
            return samples
        }

        let embeddingData = speakerEmbedding

        return await Task.detached(priority: .userInitiated) { [self] () -> [Float] in
            Device.withDefaultDevice(self.device) {
                var allSamples: [Float] = []
                var previousTail: [Float] = []
                let speakerEmbed: MLXArray? = embeddingData.map { MLXArray($0) }

                for (chunkIndex, textChunk) in textChunks.enumerated() {
                    if Task.isCancelled { return allSamples }

                    let isLastChunk = chunkIndex == textChunks.count - 1
                    let progress = Float(chunkIndex) / Float(textChunks.count)
                    onProgress?(progress)

                    let codes = self.model.generateCodes(
                        prompt: speaker,
                        text: textChunk,
                        instruct: instruct,
                        speakerEmbedding: speakerEmbed,
                        referenceTranscript: referenceTranscript,
                        tokenizer: self.tokenizer,
                        temperature: temp,
                        maxTokens: 600
                    )

                    guard !codes.isEmpty else { continue }

                    // Decode in batches
                    let samplesPerFrame = 1920
                    let decodeChunkSize = 24
                    let leftContextSize = 8

                    var chunkSamples: [Float] = []
                    var decodeLeftContext: [[Int32]] = []
                    var pos = 0

                    while pos < codes.count {
                        let endPos = min(pos + decodeChunkSize, codes.count)
                        let batchCodes = decodeLeftContext + Array(codes[pos..<endPos])

                        let flatCodes: [Int32] = batchCodes.flatMap { $0 }
                        let codesArray = MLXArray(flatCodes).reshaped([1, batchCodes.count, numCodeGroups])
                        let audio = self.decoder.mlxDecode(codes: codesArray)
                        let flatAudio = audio.reshaped([-1])
                        eval(flatAudio)
                        var batchSamples = flatAudio.asArray(Float.self)

                        let contextSamples = decodeLeftContext.count * samplesPerFrame
                        if contextSamples > 0 && batchSamples.count > contextSamples {
                            batchSamples = Array(batchSamples.dropFirst(contextSamples))
                        }

                        for sample in batchSamples {
                            if sample.isNaN || sample.isInfinite {
                                chunkSamples.append(0.0)
                            } else {
                                chunkSamples.append(max(-1.0, min(1.0, sample)))
                            }
                        }

                        decodeLeftContext = Array(codes[max(0, endPos - leftContextSize)..<endPos])
                        pos = endPos

                        DeviceSelector.synchronizeIfNeeded(device: self.device)
                        Memory.clearCache()
                    }

                    guard !chunkSamples.isEmpty else { continue }

                    // Crossfade with previous chunk
                    if !previousTail.isEmpty && crossfade > 0 {
                        let fadeLength = min(crossfade, previousTail.count, chunkSamples.count)
                        var crossfaded: [Float] = []
                        for i in 0..<fadeLength {
                            let fadeOut = Float(fadeLength - i) / Float(fadeLength)
                            let fadeIn = Float(i) / Float(fadeLength)
                            crossfaded.append(previousTail[i] * fadeOut + chunkSamples[i] * fadeIn)
                        }
                        allSamples.append(contentsOf: crossfaded)
                        chunkSamples = Array(chunkSamples.dropFirst(fadeLength))
                    }

                    if isLastChunk {
                        allSamples.append(contentsOf: chunkSamples)
                    } else if chunkSamples.count > crossfade {
                        allSamples.append(contentsOf: chunkSamples.dropLast(crossfade))
                        previousTail = Array(chunkSamples.suffix(crossfade))
                    } else {
                        previousTail = chunkSamples
                    }

                    DeviceSelector.synchronizeIfNeeded(device: self.device)
                    Memory.clearCache()
                }

                onProgress?(1.0)
                return allSamples
            }
        }.value
    }

    // MARK: - Voice Cloning

    /// Extract a speaker embedding from audio samples.
    ///
    /// - Parameter audioSamples: Raw audio samples (any sample rate, but 16kHz preferred)
    /// - Returns: 1024-dimensional speaker embedding, or nil if speaker encoder is not loaded
    public func extractSpeakerEmbedding(audioSamples: [Float]) -> [Float]? {
        guard let spkEncoder = speakerEncoder, spkEncoder.isWeightsLoaded else {
            return nil
        }

        return Device.withDefaultDevice(device) {
            defer { Memory.clearCache() }
            let audioArray = MLXArray(audioSamples)
            let embedding = spkEncoder.extractEmbedding(audio: audioArray)
            eval(embedding)
            return embedding.asArray(Float.self)
        }
    }

    /// Encode reference audio for ICL (in-context learning) voice cloning.
    ///
    /// - Parameter audioSamples: Audio samples at 24kHz
    /// - Returns: Audio codes as [[Int32]] with shape [num_quantizers, time], or nil
    public func encodeReferenceAudio(audioSamples: [Float]) -> [[Int32]]? {
        guard let encoder = audioEncoder else { return nil }

        return Device.withDefaultDevice(device) {
            defer { Memory.clearCache() }

            let audioArray = MLXArray(audioSamples).expandedDimensions(axis: 0)
            let codes = encoder.encode(audioArray)
            eval(codes)

            let numQuantizers = codes.shape[1]
            let timeFrames = codes.shape[2]

            var result: [[Int32]] = []
            for q in 0..<numQuantizers {
                let quantizerCodes = codes[0, q, 0..<timeFrames]
                eval(quantizerCodes)
                result.append(quantizerCodes.asArray(Int32.self))
            }
            return result
        }
    }

    // MARK: - Memory Management

    /// Clear cached state from the model and decoder.
    /// Call this between generations if memory accumulates.
    public func clearCache() {
        model.clearGenerationCache()
        model.clearPreparedPrefixCache()
        decoder.clearCompiledCache()
        DeviceSelector.synchronizeIfNeeded(device: device)
        Memory.clearCache()
    }

    /// Install a validated resident adapter bank. This mutates the projection
    /// graph once; call after model construction and before any generation.
    public func installPersonaAdapters(_ bank: PersonaAdapterBank) throws {
        guard personaAdapterController == nil else {
            throw PersonaAdapterError.invalidManifest("persona adapters are already installed")
        }
        personaAdapterController = try PersonaAdapterController(model: model, bank: bank)
        clearCache()
    }

    /// Switch adapters only between generations. The resident helper enforces
    /// that boundary by consuming one complete request at a time.
    public func selectPersonaAdapter(_ personaID: String?) throws -> [Float]? {
        guard let controller = personaAdapterController else {
            if personaID == nil { return nil }
            throw PersonaAdapterError.personaNotLoaded(personaID!)
        }
        let embedding = try controller.activate(personaID)
        clearCache()
        return embedding?.asArray(Float.self)
    }

    // MARK: - Private

    /// Apply mixed 4-6 bit quantization for non-pre-quantized models.
    private static func applyMixedQuantization(to model: Module) {
        quantize(model: model) { path, module in
            guard module is Quantizable else { return nil }

            let pathLower = path.lowercased()
            let use6Bit = pathLower.contains("embed") ||
                          pathLower.contains("qproj") ||
                          pathLower.contains("kproj") ||
                          pathLower.contains("vproj") ||
                          pathLower.contains("q_proj") ||
                          pathLower.contains("k_proj") ||
                          pathLower.contains("v_proj") ||
                          pathLower.contains("lm_head") ||
                          pathLower.contains("codec_head")

            return use6Bit
                ? (groupSize: 64, bits: 6, mode: .affine)
                : (groupSize: 64, bits: 4, mode: .affine)
        }
    }
}

// MARK: - Errors

public enum Qwen3TTSError: LocalizedError {
    case fileNotFound(String)
    case decoderLoadFailed
    case audioEncoderLoadFailed(String)
    case modelNotLoaded

    public var errorDescription: String? {
        switch self {
        case .fileNotFound(let file):
            return "Required file not found: \(file)"
        case .decoderLoadFailed:
            return "Failed to load MLX audio decoder"
        case .audioEncoderLoadFailed(let detail):
            return "Failed to load MLX audio encoder: \(detail)"
        case .modelNotLoaded:
            return "Model is not loaded"
        }
    }
}

// MARK: - WAV Utility

extension Qwen3TTSPipeline {
    /// Convert raw WAV data to float samples (assumes 16-bit PCM).
    public static func wavToFloatSamples(data: Data) -> [Float] {
        guard data.count > 44 else { return [] }
        let sampleData = data.dropFirst(44)
        var samples: [Float] = []
        samples.reserveCapacity(sampleData.count / 2)

        for i in stride(from: 0, to: sampleData.count - 1, by: 2) {
            let low = UInt16(sampleData[sampleData.startIndex + i])
            let high = UInt16(sampleData[sampleData.startIndex + i + 1])
            let int16 = Int16(bitPattern: low | (high << 8))
            samples.append(Float(int16) / 32767.0)
        }

        return samples
    }
}
