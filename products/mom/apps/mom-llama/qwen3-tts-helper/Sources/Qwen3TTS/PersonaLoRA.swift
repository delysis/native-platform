import Foundation
import MLX
import MLXNN

/// A resident base projection plus immutable, preloaded persona deltas.
/// PEFT stores A as [rank, input] and B as [output, rank].
final class PersonaLoRALinear: Linear {
    private struct Delta {
        let a: MLXArray
        let b: MLXArray
        let scale: Float
    }

    private let base: Linear
    private var deltas: [String: Delta] = [:]
    private var activePersonaID: String?

    init(base: Linear) {
        self.base = base
        super.init(weight: base.weight, bias: base.bias)
    }

    override var shape: (Int, Int) { base.shape }

    func install(personaID: String, a: MLXArray, b: MLXArray, scale: Float) throws {
        let (output, input) = base.shape
        guard a.ndim == 2, b.ndim == 2,
              a.shape[1] == input,
              b.shape[0] == output,
              a.shape[0] == b.shape[1],
              scale.isFinite, scale > 0 else {
            throw PersonaAdapterError.tensorShapeMismatch(personaID)
        }
        deltas[personaID] = Delta(a: a, b: b, scale: scale)
    }

    func activate(_ personaID: String?) throws {
        if let personaID, deltas[personaID] == nil {
            throw PersonaAdapterError.personaNotLoaded(personaID)
        }
        activePersonaID = personaID
    }

    override func callAsFunction(_ x: MLXArray) -> MLXArray {
        let output = base(x)
        guard let personaID = activePersonaID, let delta = deltas[personaID] else {
            return output
        }
        return output + matmul(matmul(x, delta.a.T), delta.b.T) * delta.scale
    }
}

/// Owns the exact binding between portable manifest paths and this Swift model.
/// The helper's resident request loop is serial; activation is deliberately
/// allowed only at that between-request boundary.
final class PersonaAdapterController {
    private let wrappers: [String: PersonaLoRALinear]
    private let adapters: [String: LoadedPersonaAdapter]
    private(set) var activePersonaID: String?

    init(model: Qwen3Talker, bank: PersonaAdapterBank) throws {
        let personaIDs = bank.personaIDs
        guard !personaIDs.isEmpty else {
            throw PersonaAdapterError.invalidManifest("adapter bank is empty")
        }
        var loaded: [String: LoadedPersonaAdapter] = [:]
        for personaID in personaIDs {
            guard let adapter = bank.adapter(personaID: personaID) else {
                throw PersonaAdapterError.personaNotLoaded(personaID)
            }
            loaded[personaID] = adapter
        }

        let moduleSets = loaded.values.map { Set($0.tensorPairs.map(\.module)) }
        guard let requiredModules = moduleSets.first,
              moduleSets.dropFirst().allSatisfy({ $0 == requiredModules }) else {
            throw PersonaAdapterError.moduleSetMismatch
        }

        var installed: [String: PersonaLoRALinear] = [:]
        func bind(_ path: String, _ current: Linear, _ replace: (Linear) -> Void) throws {
            guard requiredModules.contains(path) else { return }
            let wrapper = PersonaLoRALinear(base: current)
            for personaID in personaIDs {
                guard let adapter = loaded[personaID],
                      let pair = adapter.tensorPairs.first(where: { $0.module == path }) else {
                    throw PersonaAdapterError.moduleSetMismatch
                }
                let scale = adapter.manifest.adapter.inferenceScale
                    * adapter.manifest.adapter.alpha
                    / Float(adapter.manifest.adapter.rank)
                try wrapper.install(personaID: personaID, a: pair.a, b: pair.b, scale: scale)
            }
            replace(wrapper)
            installed[path] = wrapper
        }

        for index in model.layers.indices {
            let layer = model.layers[index]
            let prefix = "talker.layers.\(index)"
            try bind("\(prefix).self_attn.q_proj", layer.self_attn.q_proj) { layer.self_attn.q_proj = $0 }
            try bind("\(prefix).self_attn.k_proj", layer.self_attn.k_proj) { layer.self_attn.k_proj = $0 }
            try bind("\(prefix).self_attn.v_proj", layer.self_attn.v_proj) { layer.self_attn.v_proj = $0 }
            try bind("\(prefix).self_attn.o_proj", layer.self_attn.o_proj) { layer.self_attn.o_proj = $0 }
            try bind("\(prefix).mlp.gate_proj", layer.mlp.gate_proj) { layer.mlp.gate_proj = $0 }
            try bind("\(prefix).mlp.up_proj", layer.mlp.up_proj) { layer.mlp.up_proj = $0 }
            try bind("\(prefix).mlp.down_proj", layer.mlp.down_proj) { layer.mlp.down_proj = $0 }
        }

        for index in model.code_predictor.layers.indices {
            let layer = model.code_predictor.layers[index]
            let prefix = "talker.code_predictor.layers.\(index)"
            try bind("\(prefix).self_attn.q_proj", layer.self_attn.q_proj) { layer.self_attn.q_proj = $0 }
            try bind("\(prefix).self_attn.k_proj", layer.self_attn.k_proj) { layer.self_attn.k_proj = $0 }
            try bind("\(prefix).self_attn.v_proj", layer.self_attn.v_proj) { layer.self_attn.v_proj = $0 }
            try bind("\(prefix).self_attn.o_proj", layer.self_attn.o_proj) { layer.self_attn.o_proj = $0 }
            try bind("\(prefix).mlp.gate_proj", layer.mlp.gate_proj) { layer.mlp.gate_proj = $0 }
            try bind("\(prefix).mlp.up_proj", layer.mlp.up_proj) { layer.mlp.up_proj = $0 }
            try bind("\(prefix).mlp.down_proj", layer.mlp.down_proj) { layer.mlp.down_proj = $0 }
        }

        guard Set(installed.keys) == requiredModules else {
            let unresolved = requiredModules.subtracting(installed.keys).sorted()
            throw PersonaAdapterError.unresolvedModules(unresolved)
        }
        wrappers = installed
        adapters = loaded
    }

    func activate(_ personaID: String?) throws -> MLXArray? {
        if let personaID, adapters[personaID] == nil {
            throw PersonaAdapterError.personaNotLoaded(personaID)
        }
        for wrapper in wrappers.values {
            try wrapper.activate(personaID)
        }
        activePersonaID = personaID
        return personaID.flatMap { adapters[$0]?.speakerEmbedding }
    }
}
