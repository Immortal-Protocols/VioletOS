import CoreML
import CryptoKit
import Foundation
import UserNotifications

// RFC skeleton: Local AI threat checker & Secure Enclave Shield signer for the
// iPhone companion app (`companion/ios/`).
//
// Hooks into `Socks5Server.swift` in two ways:
//   1. Works TODAY (Phase 1): observes every outbound `(host, port, bytesUp, bytesDown)`
//      connection and can immediately reject blocked targets with SOCKS5 `0x02 notAllowed`.
//   2. Pairs with PR #14 (Phase 3+): serves the virtual `sentinel.violet` endpoint
//      right next to `time.violet` (`Socks5Server.swift:256`) to receive `Sentinel`
//      syscall telemetry and push P-256 signed `VSH1` Shields back to VioletOS.

// MARK: - Canonical Wire & Decision Schema (shared with tools/shield-trainer/schema.py)

enum ThreatVerdict: Int, Codable {
    case benign = 0
    case suspicious = 1
    case exfilOrC2 = 2
    case exploitOrPersistence = 3
}

enum ShieldAction: Int, Codable {
    case allow = 0
    case logDeep = 1
    case blockHost = 2
    case freezeApp = 3
}

struct TelemetryRecord: Codable {
    let timestampUs: UInt64
    let appId: UInt32
    let appPath: String
    let binarySha256Prefix: String
    let isBackground: Bool
    let isNewHash: Bool
    let devPushRecent: Bool
    let userIdleSec: UInt32
    let kind: String          // OPEN_R, OPEN_W, SOCK_CONN, SOCK_TX, MEM_WX, EXEC, ...
    let detail: String        // path, host:port, or byte count
    let crossApp: Bool
}

struct SocksFlowObservation: Codable {
    let timestamp: Date
    let host: String
    let port: UInt16
    let bytesUp: UInt64
    let bytesDown: UInt64
}

struct DecisionOutput: Codable {
    let verdict: ThreatVerdict
    let verdictProbs: [Float]          // calibrated 4-class softmax
    let recommendedAction: ShieldAction
    let threatTags: [String: Float]    // multi-label sigmoid scores
    let latencyMs: Double
}

// MARK: - Canonical `violet-log-v1` Window Formatter + SOCKS5 Cross-Checker

struct CanonicalWindowFormatter {
    /// Serializes a sliding event window + ground-truth SOCKS5 correlation into the
    /// exact compact token format used by `tools/shield-trainer/schema.py` (~200–400 tokens).
    static func format(
        appPath: String,
        shaPrefix: String,
        isNewHash: Bool,
        isBackground: Bool,
        userIdleSec: UInt32,
        devPushRecent: Bool,
        knownHosts: [String],
        events: [TelemetryRecord],
        socksFlows: [SocksFlowObservation]
    ) -> String {
        var lines: [String] = []
        lines.append(
            "APP=\(appPath) SHA=\(shaPrefix) NEW_HASH=\(isNewHash ? 1 : 0) " +
            "BG=\(isBackground ? 1 : 0) USER_IDLE=\(userIdleSec)s DEV_PUSH=\(devPushRecent ? 1 : 0)"
        )
        lines.append("KNOWN_HOSTS=\(knownHosts.prefix(8).joined(separator: ","))")

        let baseTs = events.first?.timestampUs ?? 0
        let seenHosts = Set(socksFlows.map { "\($0.host):\($0.port)" })

        for ev in events.suffix(48) {
            let dtMs = (ev.timestampUs >= baseTs) ? (ev.timestampUs - baseTs) / 1000 : 0
            var suffix = ev.crossApp ? " CROSS_APP=1" : ""
            if ev.kind == "SOCK_CONN" || ev.kind == "SOCK_TX" {
                let matched = seenHosts.contains(ev.detail) ? 1 : 0
                suffix += " SOCKS5_SEEN=\(matched)"
            }
            lines.append("[+\(dtMs)ms] \(ev.kind) \(ev.detail)\(suffix)")
        }

        // Cross-check: surface any SOCKS5 connection that Sentinel never reported.
        let reportedTargets = Set(events.filter { $0.kind == "SOCK_CONN" }.map(\.detail))
        for flow in socksFlows.suffix(12) {
            let target = "\(flow.host):\(flow.port)"
            if !reportedTargets.contains(target) {
                lines.append("[SOCKS5_ONLY] UNREPORTED_CONN \(target) TX=\(flow.bytesUp)B RX=\(flow.bytesDown)B")
            }
        }
        return lines.joined(separator: "\n")
    }
}

// MARK: - Tier 0 (Deterministic) + Tier 1 (Local CoreML ANE Decision Head)

final class LocalShieldChecker {
    /// Compiled CoreML student model (e.g., ModernBERT-base ~150M or Qwen3-0.6B with
    /// merged teacher-distilled LoRA + 3-head linear/MLP decision head).
    /// Runs on Apple Neural Engine (`computeUnits = .all`) in ~10–25 ms, ~100–250 MB RAM,
    /// staying inside iOS background Jetsam limits with zero cloud API calls.
    private var model: MLModel?
    private var blockedHosts: Set<String> = []
    private let signer = SecureEnclaveShieldSigner()
    private let feedbackStore = AlertFeedbackStore()

    func loadModel(at compiledURL: URL) throws {
        let cfg = MLModelConfiguration()
        cfg.computeUnits = .all // prefer Apple Neural Engine (ANE) for minimal battery/RAM
        self.model = try MLModel(contentsOf: compiledURL, configuration: cfg)
    }

    /// Synchronous hook called by `Socks5Session.connect(host:port:)` (Phase 1 ready).
    func shouldBlockSocksTarget(host: String, port: UInt16) -> Bool {
        blockedHosts.contains(host) || blockedHosts.contains("\(host):\(port)")
    }

    /// Tier 0 deterministic invariants that always override ML (immune to prompt injection).
    func checkDeterministicInvariants(_ ev: TelemetryRecord) -> ShieldAction? {
        if ev.kind == "OPEN_W" && (ev.detail.hasPrefix("/bin/") || ev.detail.hasPrefix("/apps/launcher/")) {
            return .freezeApp
        }
        return nil
    }

    /// Tier 1 single-forward-pass evaluation (prefill only, no autoregressive decoding).
    func scoreWindow(_ canonicalText: String) -> DecisionOutput {
        let start = CFAbsoluteTimeGetCurrent()
        // RFC skeleton: tokenize `canonicalText` -> input_ids / attention_mask ->
        // `model?.prediction(...)` -> calibrated `verdict_probs`, `action_probs`, `tag_probs`.
        let _ = (model, canonicalText)
        let elapsedMs = (CFAbsoluteTimeGetCurrent() - start) * 1000.0
        return DecisionOutput(
            verdict: .benign,
            verdictProbs: [0.99, 0.005, 0.003, 0.002],
            recommendedAction: .allow,
            threatTags: [:],
            latencyMs: elapsedMs
        )
    }

    /// Tier 2 enforcement: immediately block in SOCKS5, sign a TTL-bounded `VSH1` Shield
    /// using the iPhone's Secure Enclave P-256 key, send over `sentinel.violet`, and notify user.
    func handleFlaggedWindow(
        canonicalWindow: String,
        appId: UInt32,
        offendingHost: String?,
        decision: DecisionOutput,
        sendShield: (Data) -> Void
    ) {
        if decision.recommendedAction == .blockHost || decision.recommendedAction == .freezeApp {
            if let host = offendingHost { blockedHosts.insert(host) }
            if let signedEnvelope = try? signer.signTemporaryFreezeShield(appId: appId, ttlSecs: 600) {
                sendShield(signedEnvelope)
            }
        }
        feedbackStore.recordPendingAlert(window: canonicalWindow, decision: decision)
        postAlertNotification(appId: appId, host: offendingHost, decision: decision)
    }

    private func postAlertNotification(appId: UInt32, host: String?, decision: DecisionOutput) {
        let content = UNMutableNotificationContent()
        content.title = "Violet Shield flagged App \(appId)"
        content.body = "Verdict: \(decision.verdict) · Action: \(decision.recommendedAction)"
            + (host.map { " · Host: \($0)" } ?? "")
        content.categoryIdentifier = "VIOLET_SHIELD_ALERT" // [Inspect] [Kill] [Allow & Learn]
        let req = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(req)
    }
}

// MARK: - Hardware-Backed P-256 Shield Signer (Matches BearSSL in supervisor.c)

final class SecureEnclaveShieldSigner {
    private let keyTag = "com.immortal.violet.shield.p256"

    /// Produces a `VSH1` binary envelope signed with Apple Secure Enclave P-256 ECDSA
    /// (ASN.1 DER), verified directly by `services/sentinel` using BearSSL.
    func signTemporaryFreezeShield(appId: UInt32, ttlSecs: UInt32) throws -> Data {
        // RFC skeleton: load or create `SecureEnclave.P256.Signing.PrivateKey`,
        // serialize `SignedShieldHeader` + `FreezeApp(appId)` payload, and sign.
        let _ = (keyTag, appId, ttlSecs)
        return Data()
    }
}

// MARK: - Active Learning Feedback Store (Synced Periodically to Mac Trainer)

struct LabeledFeedback: Codable {
    let timestamp: Date
    let canonicalWindow: String
    let modelVerdict: ThreatVerdict
    let userLabel: String // "benign_allow" | "confirmed_threat"
}

final class AlertFeedbackStore {
    private var pending: [String: (String, DecisionOutput)] = [:]

    func recordPendingAlert(window: String, decision: DecisionOutput) {
        pending[UUID().uuidString] = (window, decision)
    }

    /// Called when the user taps [Benign — Allow & Learn] or [Confirmed Threat] on an alert.
    /// Saved to `Documents/shield-feedback.jsonl` for periodic Mac LoRA retraining.
    func recordUserLabel(alertId: String, label: String) {
        guard let (window, decision) = pending.removeValue(forKey: alertId) else { return }
        let entry = LabeledFeedback(
            timestamp: Date(),
            canonicalWindow: window,
            modelVerdict: decision.verdict,
            userLabel: label
        )
        let _ = entry // append JSONL line to Documents/shield-feedback.jsonl
    }
}
