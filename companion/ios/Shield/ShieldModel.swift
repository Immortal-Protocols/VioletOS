import CoreML
import Foundation
import Tokenizers

/// The student from tools/shield-trainer/train.py: frozen EmbeddingGemma 2
/// (text only, int8 weights, float32 compute; float16 breaks this model) and the
/// hybrid head. Everything runs on the phone; nothing leaves it.
final class ShieldModel {
    let tokenizer: Tokenizer
    private let embedder: MLModel
    private let head: MLModel
    let computeUnits: String

    struct Scores {
        let verdict: [Float]
        let action: [Float]
        let tags: [Float]
        let embedding: [Float]
        let embedMs: Double
        let headMs: Double
        let tokens: Int
    }

    static func load() async throws -> ShieldModel {
        guard let tokDir = Bundle.main.url(forResource: "ShieldTokenizer", withExtension: nil),
              let embURL = Bundle.main.url(forResource: "VioletShieldEmbedder", withExtension: "mlmodelc"),
              let headURL = Bundle.main.url(forResource: "VioletShieldHead", withExtension: "mlmodelc") else {
            throw NSError(domain: "Shield", code: 1, userInfo: [NSLocalizedDescriptionKey: "model files missing from the app bundle (run tools/shield-trainer/train.py)"])
        }
        let tok = try await AutoTokenizer.from(modelFolder: tokDir)
        let cfg = MLModelConfiguration()
        #if targetEnvironment(simulator)
        // The Simulator has no Neural Engine. `-shieldComputeUnits gpu` tries the Mac's GPU.
        let gpu = UserDefaults.standard.string(forKey: "shieldComputeUnits") == "gpu"
        cfg.computeUnits = gpu ? .cpuAndGPU : .cpuOnly
        let units = gpu ? "CPU+GPU (Simulator)" : "CPU (Simulator)"
        #else
        // Float32 compute rules out the Neural Engine (float16 only); see docs/violet-shield-poc.md.
        cfg.computeUnits = .cpuAndGPU
        let units = "CPU+GPU"
        #endif
        let emb = try MLModel(contentsOf: embURL, configuration: cfg)
        let head = try MLModel(contentsOf: headURL, configuration: cfg)
        return ShieldModel(tokenizer: tok, embedder: emb, head: head, computeUnits: units)
    }

    private init(tokenizer: Tokenizer, embedder: MLModel, head: MLModel, computeUnits: String) {
        self.tokenizer = tokenizer
        self.embedder = embedder
        self.head = head
        self.computeUnits = computeUnits
    }

    func countTokens(_ text: String) -> Int {
        tokenizer.encode(text: text).count
    }

    func tokenIds(_ windowText: String) -> [Int] {
        Array(tokenizer.encode(text: ShieldSchema.taskPrefix + windowText).prefix(ShieldSchema.seqLen))
    }

    func embed(_ windowText: String) throws -> (embedding: [Float], tokens: Int, ms: Double) {
        let t0 = CFAbsoluteTimeGetCurrent()
        let ids = tokenIds(windowText)
        let n = ShieldSchema.seqLen
        let idArr = try MLMultiArray(shape: [1, NSNumber(value: n)], dataType: .int32)
        let maskArr = try MLMultiArray(shape: [1, NSNumber(value: n)], dataType: .int32)
        for i in 0..<n {
            idArr[i] = NSNumber(value: i < ids.count ? Int32(ids[i]) : 0)  // pad id 0, padded on the right
            maskArr[i] = NSNumber(value: i < ids.count ? 1 : 0)
        }
        let input = try MLDictionaryFeatureProvider(dictionary: ["input_ids": idArr, "attention_mask": maskArr])
        let out = try embedder.prediction(from: input)
        guard let e = out.featureValue(for: "embedding")?.multiArrayValue else { throw NSError(domain: "Shield", code: 2) }
        return (Self.floats(e), ids.count, (CFAbsoluteTimeGetCurrent() - t0) * 1000)
    }

    func score(embedding: [Float], side: [Float]) throws -> (verdict: [Float], action: [Float], tags: [Float]) {
        let e = try MLMultiArray(shape: [1, 768], dataType: .float32)
        for (i, v) in embedding.enumerated() { e[i] = NSNumber(value: v) }
        let s = try MLMultiArray(shape: [1, 9], dataType: .float32)
        for (i, v) in side.enumerated() { s[i] = NSNumber(value: v) }
        let out = try head.prediction(from: MLDictionaryFeatureProvider(dictionary: ["embedding": e, "side_features": s]))
        func get(_ name: String) -> [Float] { out.featureValue(for: name)?.multiArrayValue.map(Self.floats) ?? [] }
        return (get("verdict_probs"), get("action_probs"), get("tag_probs"))
    }

    static func floats(_ a: MLMultiArray) -> [Float] {
        (0..<a.count).map { Float(truncating: a[$0]) }
    }
}

/// Per-app benign history: up to 16 Matryoshka 256-d vectors per (app, binary).
final class AppNoveltyIndex {
    private var prototypes: [String: [[Float]]] = [:]
    private let maxPrototypesPerApp = 16

    func minCosineDistance(key: String, embedding: [Float]) -> Float {
        guard let bank = prototypes[key], !bank.isEmpty else { return ShieldSchema.noHistory }
        let q = Array(embedding.prefix(256))
        return bank.map { Self.cosineDistance(q, $0) }.min() ?? ShieldSchema.noHistory
    }

    func recordBenign(key: String, embedding: [Float]) {
        var bank = prototypes[key] ?? []
        if bank.count >= maxPrototypesPerApp { bank.removeFirst() }
        bank.append(Array(embedding.prefix(256)))
        prototypes[key] = bank
    }

    func count(_ key: String) -> Int { prototypes[key]?.count ?? 0 }

    static func cosineDistance(_ a: [Float], _ b: [Float]) -> Float {
        let n = min(a.count, b.count)
        guard n > 0 else { return 1 }
        var dot: Float = 0, na: Float = 0, nb: Float = 0
        for i in 0..<n { dot += a[i] * b[i]; na += a[i] * a[i]; nb += b[i] * b[i] }
        let d = sqrt(na) * sqrt(nb)
        return d > 0 ? max(0, 1 - dot / d) : 1
    }
}
