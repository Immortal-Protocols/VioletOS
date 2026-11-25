import Foundation

// `violet-log-v1` records and windows. Every rule here mirrors
// tools/shield-trainer/schema.py and windows.py, so the model on the phone sees
// exactly the text and side features it was trained on.

enum ShieldSchema {
    static let maxEvents = 16
    static let maxSocksOnly = 6
    static let seqLen = 384
    static let taskPrefix = "task: classification | query: "
    static let windowKinds: Set<String> = ["EXEC", "OPEN_R", "OPEN_W", "SOCK_CONN", "SOCK_TX", "MEM_WX"]
    static let verdicts = ["benign", "suspicious", "exfil_or_c2", "exploit_or_persistence"]
    static let actions = ["allow", "log_deep", "block_host", "freeze_app"]
    static let tags = ["c2_beacon", "data_exfil", "persistence_write", "wx_shellcode", "cross_app_read",
                       "socks_sentinel_mismatch", "typosquat_host", "prompt_injection_attempt"]
    /// Novelty when an app has no benign history yet (train.py NO_HISTORY).
    static let noHistory: Float = 0.5
}

/// One Sentinel record: `t_ms pid app sha8 kind flags idle_s detail target`, tab separated.
struct SentinelRecord {
    let tMs: Int64
    let pid: UInt32
    let app: String
    let sha8: String
    let kind: String
    let flags: Int
    let idleS: Int
    let detail: String
    let target: String
    let arrived: Date

    init?(line: Substring, arrived: Date = Date()) {
        let p = line.split(separator: "\t", omittingEmptySubsequences: false)
        guard p.count == 9, let t = Int64(p[0]), let pid = UInt32(p[1]), let flags = Int(p[5]), let idle = Int(p[6]) else { return nil }
        tMs = t
        self.pid = pid
        app = String(p[2])
        sha8 = String(p[3])
        kind = String(p[4])
        self.flags = flags
        idleS = idle
        detail = String(p[7])
        target = String(p[8])
        self.arrived = arrived
    }

    /// A copy carrying `bytes` as its SOCK_TX total and the newer record's flags.
    func merged(with newer: SentinelRecord, bytes: UInt64) -> SentinelRecord {
        SentinelRecord(tMs: tMs, pid: pid, app: app, sha8: sha8, kind: kind, flags: newer.flags, idleS: newer.idleS,
                       detail: "\(bytes)B", target: target, arrived: newer.arrived)
    }

    private init(tMs: Int64, pid: UInt32, app: String, sha8: String, kind: String, flags: Int, idleS: Int,
                 detail: String, target: String, arrived: Date) {
        self.tMs = tMs; self.pid = pid; self.app = app; self.sha8 = sha8; self.kind = kind
        self.flags = flags; self.idleS = idleS; self.detail = detail; self.target = target; self.arrived = arrived
    }

    var txBytes: UInt64? { sendBytes(detail) }

    var background: Bool { flags & 1 != 0 }
    var crossApp: Bool { flags & 2 != 0 }
    var newHash: Bool { flags & 4 != 0 }
    var devPush: Bool { flags & 16 != 0 }
}

/// "1234B" -> 1234, like schema.py's `_bytes`.
func sendBytes(_ detail: String) -> UInt64? {
    guard detail.hasSuffix("B") else { return nil }
    let digits = detail.dropLast()
    guard !digits.isEmpty, digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
    return UInt64(digits)
}

struct WindowEvent {
    let dtMs: Int64
    let kind: String
    let detail: String
    let crossApp: Bool
    let socksSeen: Bool?
}

struct SocksOnlyFlow {
    let target: String
    let tx: UInt64
    let rx: UInt64
}

struct TelemetryWindow {
    let appPath: String
    let sha: String
    let newHash: Bool
    let background: Bool
    let userIdleSec: Int
    let devPush: Bool
    let knownHosts: [String]
    let events: [WindowEvent]
    let socksOnly: [SocksOnlyFlow]

    /// schema.py `display_events`: consecutive SOCK_TX merged into a running total, then the newest maxEvents.
    var displayEvents: [WindowEvent] {
        var merged: [WindowEvent] = []
        for ev in events {
            if ev.kind == "SOCK_TX", let prev = merged.last, prev.kind == "SOCK_TX",
               let a = sendBytes(prev.detail), let b = sendBytes(ev.detail) {
                let seen: Bool? = (prev.socksSeen == nil && ev.socksSeen == nil) ? nil : (prev.socksSeen == true && ev.socksSeen == true)
                merged[merged.count - 1] = WindowEvent(dtMs: prev.dtMs, kind: "SOCK_TX", detail: "\(a + b)B", crossApp: false, socksSeen: seen)
            } else {
                merged.append(ev)
            }
        }
        return Array(merged.suffix(ShieldSchema.maxEvents))
    }

    func canonicalText(skip: Int = 0) -> String {
        var lines = [
            "APP=\(appPath) SHA=\(sha) NEW_HASH=\(newHash ? 1 : 0) BG=\(background ? 1 : 0) USER_IDLE=\(userIdleSec)s DEV_PUSH=\(devPush ? 1 : 0)",
            "KNOWN_HOSTS=" + knownHosts.prefix(8).joined(separator: ","),
        ]
        let shown = Array(displayEvents.dropFirst(skip))
        let base = shown.first?.dtMs ?? 0
        for ev in shown {
            var suffix = ev.crossApp ? " CROSS_APP=1" : ""
            if let seen = ev.socksSeen { suffix += " SOCKS5_SEEN=\(seen ? 1 : 0)" }
            lines.append("[+\(ev.dtMs - base)ms] \(ev.kind) \(ev.detail)\(suffix)")
        }
        for f in socksOnly.suffix(ShieldSchema.maxSocksOnly) {
            lines.append("[SOCKS5_ONLY] UNREPORTED_CONN \(f.target) TX=\(f.tx)B RX=\(f.rx)B")
        }
        return lines.joined(separator: "\n")
    }

    /// Drop the oldest events until the prefixed text fits the embedder's input.
    func fitText(countTokens: (String) -> Int) -> String {
        var skip = 0
        while true {
            let text = canonicalText(skip: skip)
            if countTokens(ShieldSchema.taskPrefix + text) <= ShieldSchema.seqLen || skip >= ShieldSchema.maxEvents { return text }
            skip += 1
        }
    }

    /// `schema.py TelemetryWindow.to_side_features`, in the same order.
    func sideFeatures(minCosineDistance: Float) -> [Float] {
        let ev = displayEvents
        let flows = Array(socksOnly.suffix(ShieldSchema.maxSocksOnly))
        let crossApp = ev.filter(\.crossApp).count
        let mismatch = flows.count + ev.filter { $0.socksSeen == false }.count
        let wx = ev.filter { $0.kind == "MEM_WX" }.count
        var tx: UInt64 = flows.reduce(0) { $0 + $1.tx }
        for e in ev where e.kind == "SOCK_TX" { tx += sendBytes(e.detail) ?? 0 }
        return [
            minCosineDistance,
            background ? 1 : 0,
            newHash ? 1 : 0,
            devPush ? 1 : 0,
            Float(log1p(Double(max(0, userIdleSec)))),
            Float(crossApp),
            Float(mismatch),
            Float(wx),
            Float(log1p(Double(tx))),
        ]
    }
}

private extension Character {
    var isASCIIDigit: Bool { isASCII && isNumber }
}

/// One app as the companion sees it through the Sentinel.
final class AppTrack {
    let pid: UInt32
    var name: String
    var sha8: String
    var records: [SentinelRecord] = []
    var knownHosts: [String] = []
    var exited = false
    var dirtySince: Date?
    var lastScored: Date?
    var lastWarning = Date.distantPast

    init(pid: UInt32, name: String, sha8: String) {
        self.pid = pid
        self.name = name
        self.sha8 = sha8
    }

    var key: String { "/apps/\(name)@\(sha8)" }

    /// windows.py `add_record`: a send merges into the previous send to the same target.
    func add(_ r: SentinelRecord) {
        if r.kind == "SOCK_TX", let last = records.last, last.kind == "SOCK_TX", last.target == r.target,
           let a = last.txBytes, let b = r.txBytes {
            records[records.count - 1] = last.merged(with: r, bytes: a + b)
        } else {
            records.append(r)
        }
        if records.count > 64 { records.removeFirst(records.count - 64) }
    }

    /// `windows.py window_for`.
    func window(socksSeen: Set<String>, unreported: [SocksOnlyFlow]) -> TelemetryWindow {
        let shown = records.filter { ShieldSchema.windowKinds.contains($0.kind) }
        let base = shown.first?.tMs ?? 0
        let events = shown.map { r in
            WindowEvent(dtMs: max(0, r.tMs - base), kind: r.kind, detail: r.detail, crossApp: r.crossApp,
                        socksSeen: (r.kind == "SOCK_CONN" || r.kind == "SOCK_TX") ? socksSeen.contains(r.target) : nil)
        }
        let last = records.last
        return TelemetryWindow(appPath: "/apps/\(name)", sha: sha8, newHash: last?.newHash ?? false, background: last?.background ?? false,
                               userIdleSec: last?.idleS ?? 0, devPush: last?.devPush ?? false, knownHosts: Array(knownHosts.prefix(8)),
                               events: events, socksOnly: Array(unreported.suffix(ShieldSchema.maxSocksOnly)))
    }

    /// Hosts this window talks to that the app has not used before.
    func newTargets(in window: TelemetryWindow) -> [String] {
        var out: [String] = []
        for e in window.events where e.kind == "SOCK_CONN" && !knownHosts.contains(e.detail) && !out.contains(e.detail) {
            out.append(e.detail)
        }
        return out
    }
}
