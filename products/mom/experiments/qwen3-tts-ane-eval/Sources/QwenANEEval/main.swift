import CoreML
import Foundation
import QwenANE

private let runtimeRevision = "60e0ddaf8797e4c33043427a6c7712833f4d82d3"
private let modelRevision = "551e82a5618b4804a51559bc508fece40d63b6a7"

struct Arguments {
    let models: URL
    let reference: URL
    let transcript: String
    let text: String
    let cache: URL
    let output: URL
    let receipt: URL
    let cpuControlFrames: Int
    let chunkFrames: [Int]

    static func parse(_ raw: [String]) throws -> Self {
        var values: [String: String] = [:]
        var index = 1
        while index < raw.count {
            guard raw[index].hasPrefix("--"), index + 1 < raw.count else {
                throw EvalError.usage("expected --name value pairs")
            }
            values[String(raw[index].dropFirst(2))] = raw[index + 1]
            index += 2
        }
        func required(_ key: String) throws -> String {
            guard let value = values[key], !value.isEmpty else { throw EvalError.usage("missing --\(key)") }
            return value
        }
        let chunkFrames = (values["chunk-frames"] ?? "4,8,12")
            .split(separator: ",")
            .compactMap { Int($0) }
        guard !chunkFrames.isEmpty, chunkFrames.allSatisfy({ (1...12).contains($0) }) else {
            throw EvalError.usage("--chunk-frames must be a comma-separated list in 1...12")
        }
        return try Self(
            models: URL(fileURLWithPath: required("models")),
            reference: URL(fileURLWithPath: required("reference")),
            transcript: required("transcript"),
            text: required("text"),
            cache: URL(fileURLWithPath: required("cache")),
            output: URL(fileURLWithPath: required("output")),
            receipt: URL(fileURLWithPath: required("receipt")),
            cpuControlFrames: Int(values["cpu-control-frames"] ?? "0") ?? 0,
            chunkFrames: chunkFrames
        )
    }
}

enum EvalError: Error, LocalizedError {
    case usage(String)
    case qualification(String)

    var errorDescription: String? {
        switch self {
        case .usage(let message):
            "\(message). Usage: qwen3-tts-ane-eval --models DIR --reference WAV --transcript TEXT --text TEXT --cache DIR --output WAV --receipt JSON [--cpu-control-frames N]"
        case .qualification(let message): message
        }
    }
}

struct StageReceipt: Codable {
    let loadMs: Double
    let voicePrepMs: Double
    let voicePrepCacheHit: Bool
    let warmMs: Double
    let firstPCMMs: Double
    let renderMs: Double
    let audioMs: Double
    let rtf: Double
    let promptMs: Double
    let prefillMs: Double
    let generationLoopMs: Double
    let vocoderBusyMs: Double
    let vocoderWaitMs: Double
    let chunks: [Int]
    let frames: Int
    let stopReason: String
    let prefixRowsReused: Int
    let chunkSchedule: [Int]
}

struct CancellationReceipt: Codable {
    let requestedAfterMs: Double
    let acknowledgedAfterRequestMs: Double
    let totalMs: Double
    let stopReason: String
    let returnedSamples: Int
}

struct CPUControlReceipt: Codable {
    let frames: Int
    let loadMs: Double?
    let renderMs: Double?
    let aneToCPUSpeedup: Double?
    let error: String?
}

struct ComputePlanReceipt: Codable {
    let model: String
    let function: String?
    let wholeModelFallback: Bool
    let requestedUnits: String
    let operations: Int
    let neuralEnginePreferred: Int
    let cpuPreferred: Int
    let gpuPreferred: Int
    let neuralEngineSupported: Int
    let cpuSupported: Int
    let gpuSupported: Int
    let qualifies: Bool
}

struct EvaluationReceipt: Codable {
    let schemaVersion: Int
    let createdAt: String
    let hardware: String
    let operatingSystem: String
    let runtimeRevision: String
    let modelRevision: String
    let modelsDirectory: String
    let referencePath: String
    let outputPath: String
    let synthesisPlacement: String
    let voicePreparationPlacement: String
    let computePlans: [ComputePlanReceipt]
    let stage: StageReceipt
    let cancellation: CancellationReceipt
    let cpuControl: CPUControlReceipt?
    let qualificationPassed: Bool
}

private final class StreamMeter: @unchecked Sendable {
    private let lock = NSLock()
    private let started: Date
    private var first: Date?
    private var sizes: [Int] = []

    init(started: Date) { self.started = started }

    func record(_ samples: [Float]) {
        lock.lock()
        if first == nil { first = Date() }
        sizes.append(samples.count)
        lock.unlock()
    }

    func snapshot() -> (firstMs: Double, sizes: [Int]) {
        lock.lock(); defer { lock.unlock() }
        return (first.map { $0.timeIntervalSince(started) * 1000 } ?? .infinity, sizes)
    }
}

@main
struct QwenANEEvaluation {
    static func main() async throws {
        let arguments = try Arguments.parse(CommandLine.arguments)
        try FileManager.default.createDirectory(
            at: arguments.receipt.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try FileManager.default.createDirectory(
            at: arguments.output.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )

        emit(stage: "compute_plan", details: ["status": "started"])
        let plans = try await auditRequiredPlans(models: arguments.models)
        let planQualified = plans.allSatisfy(\.qualifies)
        emit(stage: "compute_plan", details: [
            "status": planQualified ? "qualified" : "failed",
            "models": plans.count,
        ])

        let loadStarted = Date()
        let engine = try QwenANEEngine(modelsDirectory: arguments.models)
        engine.options.sampling.temperature = 1.0
        engine.options.chunkFrames = arguments.chunkFrames
        let loadMs = Date().timeIntervalSince(loadStarted) * 1000
        emit(stage: "model_load", details: ["ms": loadMs])

        let prepStarted = Date()
        let prepared = try QwenStandaloneVoicePrep.prepared(
            referenceWAV: Data(contentsOf: arguments.reference),
            transcript: arguments.transcript,
            cacheDirectory: arguments.cache,
            modelsDirectory: arguments.models,
            modelRevision: modelRevision,
            speakerDimension: engine.speakerDimension
        )
        let prepMs = Date().timeIntervalSince(prepStarted) * 1000
        emit(stage: "voice_prep", details: ["ms": prepMs, "cache_hit": prepared.cacheHit])

        let warmStarted = Date()
        _ = try engine.warm(voice: prepared.voice)
        let warmMs = Date().timeIntervalSince(warmStarted) * 1000
        emit(stage: "warm", details: ["ms": warmMs])

        let renderStarted = Date()
        let meter = StreamMeter(started: renderStarted)
        let render = try engine.render(
            text: arguments.text,
            voice: prepared.voice,
            seed: 7,
            chunkFrames: arguments.chunkFrames,
            onAudio: { meter.record($0) }
        )
        let renderMs = Date().timeIntervalSince(renderStarted) * 1000
        let stream = meter.snapshot()
        guard stream.firstMs.isFinite, !render.samples.isEmpty else {
            throw EvalError.qualification("the ANE render produced no playable PCM")
        }
        try writeWAV(samples: render.samples, sampleRate: render.sampleRate, to: arguments.output)
        let audioMs = render.audioSeconds * 1000
        let stage = StageReceipt(
            loadMs: loadMs,
            voicePrepMs: prepMs,
            voicePrepCacheHit: prepared.cacheHit,
            warmMs: warmMs,
            firstPCMMs: stream.firstMs,
            renderMs: renderMs,
            audioMs: audioMs,
            rtf: renderMs / audioMs,
            promptMs: render.timings.prompt * 1000,
            prefillMs: render.timings.prefill * 1000,
            generationLoopMs: render.timings.loop * 1000,
            vocoderBusyMs: render.timings.vocoder * 1000,
            vocoderWaitMs: render.timings.vocoderWait * 1000,
            chunks: stream.sizes,
            frames: render.frames,
            stopReason: render.stopReason.rawValue,
            prefixRowsReused: render.prefixRowsReused,
            chunkSchedule: arguments.chunkFrames
        )
        emit(stage: "render", details: [
            "first_pcm_ms": stream.firstMs,
            "render_ms": renderMs,
            "audio_ms": audioMs,
            "rtf": stage.rtf,
        ])

        let cancelStarted = Date()
        let cancellationDelay = 0.15
        let cancelled = try engine.render(
            text: "This deliberately long reply measures how quickly an in-flight speculative answer can be belayed when the caller keeps speaking.",
            voice: prepared.voice,
            seed: 8,
            chunkFrames: arguments.chunkFrames,
            cancelled: { Date().timeIntervalSince(cancelStarted) >= cancellationDelay }
        )
        let cancelTotal = Date().timeIntervalSince(cancelStarted)
        let cancellation = CancellationReceipt(
            requestedAfterMs: cancellationDelay * 1000,
            acknowledgedAfterRequestMs: max(0, cancelTotal - cancellationDelay) * 1000,
            totalMs: cancelTotal * 1000,
            stopReason: cancelled.stopReason.rawValue,
            returnedSamples: cancelled.samples.count
        )
        emit(stage: "cancel", details: [
            "ack_ms": cancellation.acknowledgedAfterRequestMs,
            "stop": cancellation.stopReason,
        ])

        var cpuControl: CPUControlReceipt?
        if arguments.cpuControlFrames > 0 {
            do {
                let cpuLoadStarted = Date()
                let cpu = try QwenANEEngine(modelsDirectory: arguments.models, computeTarget: .cpuOnly)
                cpu.options.sampling.temperature = 1.0
                let cpuLoadMs = Date().timeIntervalSince(cpuLoadStarted) * 1000
                let cpuRenderStarted = Date()
                _ = try cpu.render(
                    text: arguments.text,
                    voice: prepared.voice,
                    seed: 7,
                    maxFrames: arguments.cpuControlFrames,
                    chunkFrames: arguments.chunkFrames
                )
                let cpuRenderMs = Date().timeIntervalSince(cpuRenderStarted) * 1000
                let comparableANEMs = renderMs * Double(arguments.cpuControlFrames) / Double(max(1, render.frames))
                cpuControl = CPUControlReceipt(
                    frames: arguments.cpuControlFrames,
                    loadMs: cpuLoadMs,
                    renderMs: cpuRenderMs,
                    aneToCPUSpeedup: cpuRenderMs / comparableANEMs,
                    error: nil
                )
            } catch {
                cpuControl = CPUControlReceipt(
                    frames: arguments.cpuControlFrames,
                    loadMs: nil,
                    renderMs: nil,
                    aneToCPUSpeedup: nil,
                    error: String(describing: error)
                )
            }
        }

        let cancellationQualified = cancelled.stopReason == .cancelled && cancelled.samples.isEmpty
        let qualified = planQualified && cancellationQualified && stage.firstPCMMs.isFinite
        let receipt = EvaluationReceipt(
            schemaVersion: 1,
            createdAt: ISO8601DateFormatter().string(from: Date()),
            hardware: hardwareName(),
            operatingSystem: ProcessInfo.processInfo.operatingSystemVersionString,
            runtimeRevision: runtimeRevision,
            modelRevision: modelRevision,
            modelsDirectory: arguments.models.path,
            referencePath: arguments.reference.path,
            outputPath: arguments.output.path,
            synthesisPlacement: "Core ML cpuAndNeuralEngine; compute-plan audited; GPU excluded",
            voicePreparationPlacement: "Core ML cpuOnly (upstream-required one-time preparation)",
            computePlans: plans,
            stage: stage,
            cancellation: cancellation,
            cpuControl: cpuControl,
            qualificationPassed: qualified
        )
        let encoded = try JSONEncoder.pretty.encode(receipt)
        try encoded.write(to: arguments.receipt, options: .atomic)
        FileHandle.standardOutput.write(encoded)
        FileHandle.standardOutput.write(Data("\n".utf8))
        guard qualified else { throw EvalError.qualification("ANE evaluation did not qualify") }
    }
}

private extension JSONEncoder {
    static var pretty: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return encoder
    }
}

private func auditRequiredPlans(models: URL) async throws -> [ComputePlanReceipt] {
    let coreML = models.appendingPathComponent("coreml")
    let requests: [(String, String?)] = [
        ("talker0", "decode"), ("talker0", "prefill"),
        ("talker1", "decode"), ("talker1", "prefill"),
        ("cp_ane", nil), ("upMall", "w12"), ("upMall", "w20"),
    ]
    var receipts: [ComputePlanReceipt] = []
    for (name, function) in requests {
        let url = coreML.appendingPathComponent("\(name).mlmodelc")
        let configuration = MLModelConfiguration()
        configuration.computeUnits = .cpuAndNeuralEngine
        if let function { configuration.functionName = function }
        let plan: MLComputePlan
        let wholeModelFallback: Bool
        do {
            plan = try await MLComputePlan.load(contentsOf: url, configuration: configuration)
            wholeModelFallback = false
        } catch where function != nil {
            // Core ML's runtime accepts `functionName` for these compiled
            // multifunction bundles, while MLComputePlan can classify the
            // compiled artifact as a container and reject it. The whole-model
            // plan still exposes every operation and its preferred device.
            configuration.functionName = nil
            plan = try await MLComputePlan.load(contentsOf: url, configuration: configuration)
            wholeModelFallback = true
        }
        var operations = 0
        var preferred = ["neural_engine": 0, "cpu": 0, "gpu": 0]
        var supported = ["neural_engine": 0, "cpu": 0, "gpu": 0]
        func inspect(_ block: MLModelStructure.Program.Block) {
            for operation in block.operations {
                operations += 1
                if let usage = plan.deviceUsage(for: operation) {
                    preferred[deviceName(usage.preferred), default: 0] += 1
                    for device in usage.supported { supported[deviceName(device), default: 0] += 1 }
                }
                operation.blocks.forEach(inspect)
            }
        }
        switch plan.modelStructure {
        case .program(let program): program.functions.values.forEach { inspect($0.block) }
        case .neuralNetwork(let network):
            for layer in network.layers {
                operations += 1
                if let usage = plan.deviceUsage(for: layer) {
                    preferred[deviceName(usage.preferred), default: 0] += 1
                    for device in usage.supported { supported[deviceName(device), default: 0] += 1 }
                }
            }
        default: break
        }
        let ne = preferred["neural_engine", default: 0]
        receipts.append(ComputePlanReceipt(
            model: name,
            function: function,
            wholeModelFallback: wholeModelFallback,
            requestedUnits: "cpuAndNeuralEngine",
            operations: operations,
            neuralEnginePreferred: ne,
            cpuPreferred: preferred["cpu", default: 0],
            gpuPreferred: preferred["gpu", default: 0],
            neuralEngineSupported: supported["neural_engine", default: 0],
            cpuSupported: supported["cpu", default: 0],
            gpuSupported: supported["gpu", default: 0],
            qualifies: operations > 0 && ne > 0 && preferred["gpu", default: 0] == 0
        ))
    }
    return receipts
}

private func deviceName(_ device: MLComputeDevice) -> String {
    switch device {
    case .cpu: "cpu"
    case .gpu: "gpu"
    case .neuralEngine: "neural_engine"
    @unknown default: "unknown"
    }
}

private func hardwareName() -> String {
    var size = 0
    sysctlbyname("machdep.cpu.brand_string", nil, &size, nil, 0)
    var value = [CChar](repeating: 0, count: max(1, size))
    sysctlbyname("machdep.cpu.brand_string", &value, &size, nil, 0)
    return String(cString: value)
}

private func emit(stage: String, details: [String: Any]) {
    var event = details
    event["stage"] = stage
    event["at"] = ISO8601DateFormatter().string(from: Date())
    if let data = try? JSONSerialization.data(withJSONObject: event, options: [.sortedKeys]) {
        FileHandle.standardError.write(data)
        FileHandle.standardError.write(Data("\n".utf8))
    }
}

private func writeWAV(samples: [Float], sampleRate: Int, to url: URL) throws {
    let pcm = samples.map { sample -> Int16 in
        let clamped = max(-1, min(0.9999695, sample))
        return Int16((clamped * 32_768).rounded())
    }
    let dataBytes = pcm.count * MemoryLayout<Int16>.size
    var output = Data()
    func appendASCII(_ value: String) { output.append(Data(value.utf8)) }
    func append16(_ value: UInt16) {
        output.append(UInt8(value & 0xff)); output.append(UInt8(value >> 8))
    }
    func append32(_ value: UInt32) {
        output.append(UInt8(value & 0xff)); output.append(UInt8((value >> 8) & 0xff))
        output.append(UInt8((value >> 16) & 0xff)); output.append(UInt8((value >> 24) & 0xff))
    }
    appendASCII("RIFF"); append32(UInt32(36 + dataBytes)); appendASCII("WAVE")
    appendASCII("fmt "); append32(16); append16(1); append16(1)
    append32(UInt32(sampleRate)); append32(UInt32(sampleRate * 2)); append16(2); append16(16)
    appendASCII("data"); append32(UInt32(dataBytes))
    pcm.withUnsafeBytes { output.append(contentsOf: $0) }
    try output.write(to: url, options: .atomic)
}
