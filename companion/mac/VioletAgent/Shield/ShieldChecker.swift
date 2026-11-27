import Foundation
import Darwin
import Network
import os

// Violet Shield on the Mac companion (docs/violet-shield-rfc.md, docs/violet-shield-poc.md).
//
// - `sentinel.violet` is answered by this relay itself (next to `time.violet`):
//   VioletOS's Sentinel streams syscall telemetry here and receives signed warnings.
// - The SOCKS5 relay is the second, independent ground truth: every connection
//   VioletOS makes passes through it, so a connection the Sentinel never reported
//   is itself evidence of tampering.
// - Tier 0: deterministic rules. Tier 1: EmbeddingGemma 2 + hybrid head + per-app
//   novelty, on the phone. Warnings are signed before they appear on VioletOS Home.

let shieldLog = Logger(subsystem: "com.immortal.violet.relay", category: "shield")

struct ShieldDecision: Identifiable {
    let id = UUID()
    let time: Date
    let pid: UInt32
    let app: String
    let tier: String           // "tier0" | "model" | "relay"
    let verdict: String
    let pMalicious: Float
    let action: String
    let tags: [(String, Float)]
    let novelty: Float
    let tokens: Int
    let latencyMs: Double
    let hosts: [String]
    let window: String
    var warned: Bool
    var note: String
}

private struct TrainingScores {
    let input: String
    let sideFeatures: [Float]
    let verdict: [Float]
    let action: [Float]
    let tags: [Float]
}

struct ShieldSnapshot {
    var modelState = "loading"
    var computeUnits = ""
    var signer = ""
    var link = "waiting for VioletOS"
    var linkUp = false
    var bootId = ""
    var chain = ""
    var heartbeat = ""
    var events = 0
    var windowsScored = 0
    var shieldsSent = 0
    var shieldsAccepted = 0
    var shieldsRejected = 0
    var decisions: [ShieldDecision] = []
    var lastLatencyMs: Double = 0
}

@MainActor
final class ShieldUI: ObservableObject {
    static let shared = ShieldUI()
    @Published var snap = ShieldSnapshot()
}

final class Shield {
    static let shared = Shield()

    let queue = DispatchQueue(label: "com.immortal.violet.shield")
    private let modelQueue = DispatchQueue(label: "com.immortal.violet.shield.model", qos: .userInitiated)
    private var model: ShieldModel?
    private var signer: ShieldSigner!
    private let novelty = AppNoveltyIndex()
    private var dataDirectory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    private lazy var feedback = AlertFeedbackStore(directory: dataDirectory)
    private var tracks: [UInt32: AppTrack] = [:]
    private var link: SentinelLink?
    private var agentSession: UUID?
    private var lastAgentFrame: Date?
    private var verdictTransport = ""
    private var linkUpSince: Date?
    private var snap = ShieldSnapshot()
    private var proxySnapshot: RelayFlowSnapshot?
    private var proxySampleMs: Int64 = 0
    private var proxyStartMs: Int64 = 0
    private var proxyReceived = Date.distantPast
    private var proxyFlagged = Set<Int>()
    private var proxyReports: [(target: String, time: Int64)] = []
    private var telemetryThroughMs: Int64 = 0
    private var pending: [UInt64: String] = [:]
    private var scoring = Set<UInt32>()
    private var flaggedFlows = Set<Int>()

    // SOCKS flows, touched from the relay queue under `lock`.
    private let lock = NSLock()
    private var flows: [Int: FlowEntry] = [:]
    private var nextFlow = 1
    private var reported: [String: Date] = [:]

    struct FlowEntry {
        let id: Int
        let host: String
        let port: UInt16
        let started: Date
        var up: UInt64 = 0
        var down: UInt64 = 0
        var ended: Date?
        var target: String { "\(host):\(port)" }
    }

    func start(emulatorKey: URL? = nil, dataDirectory: URL? = nil) {
        if let dataDirectory { self.dataDirectory = dataDirectory }
        signer = ShieldSigner(emulatorKey: emulatorKey)
        snap.signer = signer.keyDescription
        Task.detached { [weak self] in
            do {
                let m = try await ShieldModel.load()
                self?.queue.async {
                    self?.model = m
                    self?.snap.modelState = "EmbeddingGemma 2 (270M, int8) + hybrid head"
                    self?.snap.computeUnits = m.computeUnits
                    self?.publish()
                    if UserDefaults.standard.bool(forKey: "shieldSelfTest") { self?.selfTest() }
                    self?.warmUp()
                }
            } catch {
                self?.queue.async {
                    self?.snap.modelState = "model unavailable: \(error.localizedDescription) (Tier 0 only)"
                    self?.publish()
                }
            }
        }
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 0.25, repeating: 0.25)
        t.setEventHandler { [weak self] in self?.tick() }
        t.resume()
        timer = t
        publish()
    }
    private var timer: DispatchSourceTimer?

    private func warmUp() {
        guard let model else { return }
        modelQueue.async {
            _ = try? model.embed("APP=/apps/warmup SHA=00000000 NEW_HASH=0 BG=0 USER_IDLE=0s DEV_PUSH=0\nKNOWN_HOSTS=")
        }
    }

    // MARK: - SOCKS hooks (relay queue)

    func flowStarted(host: String, port: UInt16) -> Int {
        lock.lock(); defer { lock.unlock() }
        let id = nextFlow
        nextFlow += 1
        flows[id] = FlowEntry(id: id, host: host.lowercased(), port: port, started: Date())
        if flows.count > 400, let oldest = flows.values.filter({ $0.ended != nil }).min(by: { $0.started < $1.started }) {
            flows.removeValue(forKey: oldest.id)
        }
        return id
    }

    func flowBytes(_ id: Int, up: Int, down: Int) {
        lock.lock(); defer { lock.unlock() }
        flows[id]?.up += UInt64(up)
        flows[id]?.down += UInt64(down)
    }

    func flowEnded(_ id: Int) {
        lock.lock(); defer { lock.unlock() }
        flows[id]?.ended = Date()
    }

    func attachSentinel(_ conn: NWConnection) {
        queue.async {
            guard self.agentSession == nil else {
                conn.cancel()
                return
            }
            let next = SentinelLink(conn: conn, queue: self.queue, shield: self)
            self.replaceLink(next, route: "SOCKS relay")
            next.start()
        }
    }

    /// The handset has an authenticated Agent session with this Mac. Use its
    /// Sentinel stream ahead of the emulator's local SOCKS endpoint.
    func receiveAgent(_ data: Data, session: UUID, send: @escaping (Data) -> Void) {
        queue.async {
            if self.agentSession != session {
                guard data.first == 0x01 else { return }  // new streams begin with VSL1 HELLO
                self.replaceLink(SentinelLink(queue: self.queue, shield: self, forward: send), route: "Mac Agent")
                self.agentSession = session
            }
            self.lastAgentFrame = Date()
            self.link?.feed(data)
        }
    }

    func disconnectAgent(_ session: UUID) {
        queue.async {
            guard self.agentSession == session, let link = self.link else { return }
            link.close()
            self.linkClosed(link, "Agent disconnected")
        }
    }

    private func replaceLink(_ next: SentinelLink, route: String) {
        link?.close()
        link = next
        resetProxy()
        lock.lock()
        reported.removeAll()
        lock.unlock()
        tracks.removeAll()
        scoring.removeAll()
        pending.removeAll()
        snap.bootId = ""
        snap.chain = ""
        snap.heartbeat = ""
        snap.events = 0
        snap.windowsScored = 0
        snap.shieldsSent = 0
        snap.shieldsAccepted = 0
        snap.shieldsRejected = 0
        snap.decisions.removeAll()
        linkUpSince = Date()
        snap.link = route
        verdictTransport = route
        snap.linkUp = true
        publish()
    }

    // MARK: - Sentinel frames (shield queue)

    fileprivate func linkClosed(_ l: SentinelLink, _ why: String) {
        guard link === l else { return }
        link = nil
        resetProxy()
        agentSession = nil
        lastAgentFrame = nil
        pending.removeAll()
        linkUpSince = nil
        snap.linkUp = false
        snap.link = "offline (\(why))"
        publish()
    }

    fileprivate func frame(_ kind: UInt8, _ payload: Data) {
        let text = String(decoding: payload, as: UTF8.self)
        switch kind {
        case 0x01:
            // "VSL1 boot=.. chain=.. key=.."
            for part in text.split(separator: " ") {
                if part.hasPrefix("boot=") { snap.bootId = String(part.dropFirst(5).prefix(12)) }
                if part.hasPrefix("chain=") { snap.chain = String(part.dropFirst(6).prefix(16)) }
            }
            shieldLog.info("sentinel hello: \(text, privacy: .public)")
        case 0x02:
            let now = Date()
            var n = 0
            for line in text.split(separator: "\n") {
                guard let r = SentinelRecord(line: line, arrived: now) else { continue }
                ingest(r)
                n += 1
            }
            snap.events += n
        case 0x03:
            // "uptime_ms seq chain dropped active frozen accepted rejected"
            let f = text.split(separator: " ")
            if f.count >= 8 {
                telemetryThroughMs = f.count >= 9 ? Int64(f[8]) ?? 0 : Int64(f[0]) ?? 0
                snap.chain = String(f[2].prefix(16))
                snap.heartbeat = "up \(Int(f[0]).map { $0 / 1000 } ?? 0)s · chain #\(f[1]) · \(f[3]) dropped"
            }
        case 0x05 where agentSession != nil:
            receiveProxy(payload)
        case 0x04:
            let f = text.split(separator: " ")
            if f.count >= 2, let seq = UInt64(f[1]) {
                let what = pending.removeValue(forKey: seq) ?? "shield"
                if f[0] == "ok" {
                    snap.shieldsAccepted += 1
                    shieldLog.info("VioletOS verified \(what, privacy: .public)")
                } else {
                    snap.shieldsRejected += 1
                    shieldLog.error("VioletOS rejected \(what, privacy: .public): \(f.dropFirst(2).joined(separator: " "), privacy: .public)")
                }
            }
        default:
            break
        }
        publish()
    }

    private func resetProxy() {
        proxyStartMs = 0
        proxySampleMs = 0
        proxySnapshot = nil
        proxyFlagged.removeAll()
        proxyReports.removeAll()
        telemetryThroughMs = 0
    }

    private func receiveProxy(_ payload: Data) {
        guard payload.count > 16, payload.count <= 3016,
              let snapshot = try? JSONDecoder().decode(RelayFlowSnapshot.self, from: payload.dropFirst(16)),
              ["Mac", "iPhone"].contains(snapshot.source) else { resetProxy(); return }
        let start = payload.withUnsafeBytes { Int64(littleEndian: $0.loadUnaligned(as: Int64.self)) }
        let sample = payload.withUnsafeBytes { Int64(littleEndian: $0.loadUnaligned(fromByteOffset: 8, as: Int64.self)) }
        guard start >= 0, sample >= start,
              snapshot.flows.allSatisfy({ flow in
                  flow.ageMs >= 0 && flow.endedMsAgo.map { (0...flow.ageMs).contains($0) } != false
                      && flow.target.utf8.count <= 300 && flow.id > 0
              })
        else { resetProxy(); return }
        if proxyStartMs != start {
            proxyFlagged.removeAll()
            proxyReports = proxyReports.filter { $0.time >= start }
        }
        proxyStartMs = start
        proxySampleMs = sample
        proxySnapshot = snapshot
        proxyReceived = Date()
        if snapshot.complete { proxyFlagged.formIntersection(snapshot.flows.map(\.id)) }
        proxyReports = proxyReports.filter { $0.time >= sample - 185_000 }
        // Rescore once a sample covers a previously unobserved network call.
        for t in tracks.values where !t.exited {
            if t.records.contains(where: { ($0.kind == "SOCK_CONN" || $0.kind == "SOCK_TX")
                && $0.tMs > t.proxyScoredThrough && $0.tMs <= sample - 2000 && $0.tMs >= start }) {
                t.dirtySince = t.dirtySince ?? Date()
            }
        }
    }

    private func ingest(_ r: SentinelRecord) {
        let t: AppTrack
        if let existing = tracks[r.pid] {
            t = existing
        } else {
            t = AppTrack(pid: r.pid, name: r.app, sha8: r.sha8)
            tracks[r.pid] = t
        }
        if r.app != "?" { t.name = r.app; t.sha8 = r.sha8 }
        t.add(r)
        switch r.kind {
        case "EXIT": t.exited = true
        case "SOCK_CONN":
            if agentSession != nil && !r.udp {
                proxyReports.append((r.target.lowercased(), r.tMs))
                if proxyReports.count > 4096 { proxyReports.removeFirst(1024) }
            }
            lock.lock(); reported[r.target.lowercased()] = r.arrived; lock.unlock()
            t.dirtySince = t.dirtySince ?? Date()
        default:
            if ShieldSchema.windowKinds.contains(r.kind) { t.dirtySince = t.dirtySince ?? Date() }
        }
        if tracks.count > 64, let old = tracks.values.filter({ $0.exited }).first { tracks.removeValue(forKey: old.pid) }
    }

    // MARK: - Scoring

    private func tick() {
        let now = Date()
        if let frame = lastAgentFrame, now.timeIntervalSince(frame) > 8, let link {
            link.close()
            linkClosed(link, "Mac Agent telemetry timed out")
        }
        checkUnreportedFlows(now)
        for t in tracks.values where !t.exited && !scoring.contains(t.pid) {
            guard let since = t.dirtySince else { continue }
            let quiet = (t.records.last.map { now.timeIntervalSince($0.arrived) } ?? 1) >= 0.3
            // Score when the app pauses, or every second while it keeps going.
            if quiet || now.timeIntervalSince(since) >= 1.0 {
                t.dirtySince = nil
                evaluate(t)
            }
        }
        publish()
    }

    private func socksSeen(_ now: Date) -> Set<String>? {
        // Direct Sentinel sessions use this Mac relay. Agent sessions supply a proxy snapshot.
        guard agentSession == nil else { return nil }
        lock.lock(); defer { lock.unlock() }
        return Set(flows.values.filter { now.timeIntervalSince($0.started) < 180 }.map(\.target))
    }

    private func evaluate(_ t: AppTrack) {
        let now = Date()
        let local = socksSeen(now)
        let window = t.window(unreported: []) { r in
            guard !r.udp, let host = r.target.split(separator: ":").dropLast().last,
                  !["agent-relay.violet", "agent-pair.violet", "time.violet"].contains(host) else { return nil }
            if self.agentSession == nil { return local?.contains(r.target.lowercased()) }
            guard now.timeIntervalSince(self.proxyReceived) < 5 else { return nil }
            return self.proxySnapshot?.observation(for: r.target, eventMs: r.tMs,
                                                   sessionMs: self.proxyStartMs, sampleMs: self.proxySampleMs)
        }
        t.proxyScoredThrough = proxySampleMs - 2000
        // Tier 0: rules no window text can talk its way past.
        if let rule = tier0(t, window) {
            let d = ShieldDecision(time: now, pid: t.pid, app: t.name, tier: "tier0", verdict: "exploit_or_persistence", pMalicious: 1,
                                   action: "freeze_app", tags: [("persistence_write", 1)], novelty: 0, tokens: 0, latencyMs: 0,
                                   hosts: t.newTargets(in: window), window: window.canonicalText(), warned: false, note: rule)
            decide(t, d)
            return
        }
        guard let model else { return }
        scoring.insert(t.pid)
        let key = t.key
        modelQueue.async { [weak self] in
            guard let self else { return }
            let t0 = CFAbsoluteTimeGetCurrent()
            let text = window.fitText(countTokens: model.countTokens)
            guard let e = try? model.embed(text) else {
                self.queue.async { if self.tracks[t.pid] === t { self.scoring.remove(t.pid) } }
                return
            }
            self.queue.async {
                let dist = self.novelty.minCosineDistance(key: key, embedding: e.embedding)
                let side = window.sideFeatures(minCosineDistance: dist)
                self.modelQueue.async {
                    let out = try? model.score(embedding: e.embedding, side: side)
                    let ms = (CFAbsoluteTimeGetCurrent() - t0) * 1000
                    self.queue.async {
                        guard self.tracks[t.pid] === t else { return }
                        self.scoring.remove(t.pid)
                        guard let out, out.verdict.count == 4, out.action.count == 4 else { return }
                        self.snap.windowsScored += 1
                        self.snap.lastLatencyMs = ms
                        let vi = out.verdict.indices.max { out.verdict[$0] < out.verdict[$1] }!
                        let ai = out.action.indices.max { out.action[$0] < out.action[$1] }!
                        let tags = zip(ShieldSchema.tags, out.tags).filter { $0.1 >= 0.5 }.sorted { $0.1 > $1.1 }
                        let pMal = 1 - out.verdict[0]
                        var hosts = t.newTargets(in: window)
                        if hosts.isEmpty { hosts = window.events.filter { $0.kind == "SOCK_CONN" }.map(\.detail) }
                        let d = ShieldDecision(time: Date(), pid: t.pid, app: t.name, tier: "model", verdict: ShieldSchema.verdicts[vi],
                                               pMalicious: pMal, action: ShieldSchema.actions[ai], tags: tags.map { ($0.0, $0.1) }, novelty: dist,
                                               tokens: e.tokens, latencyMs: ms, hosts: Array(Set(hosts)).sorted(), window: window.canonicalText(),
                                               warned: false, note: "")
                        shieldLog.info("scored \(t.name, privacy: .public) pid \(t.pid): \(d.verdict, privacy: .public) p_mal=\(pMal) \(d.action, privacy: .public) \(Int(ms))ms")
                        if out.verdict[0] >= 0.9 {
                            // Confidently benign: it becomes this app's history.
                            self.novelty.recordBenign(key: key, embedding: e.embedding)
                            for h in window.events where h.kind == "SOCK_CONN" && !t.knownHosts.contains(h.detail) {
                                t.knownHosts.append(h.detail)
                            }
                        }
                        self.decide(t, d, scores: TrainingScores(input: text, sideFeatures: side,
                                                                verdict: out.verdict, action: out.action, tags: out.tags))
                    }
                }
            }
        }
    }

    private func tier0(_ t: AppTrack, _ w: TelemetryWindow) -> String? {
        for e in w.events where e.kind == "OPEN_W" {
            if e.detail.hasPrefix("/bin/") || e.detail.hasPrefix("/lib/") { return "write to system file \(e.detail)" }
            if e.detail.hasPrefix("/apps/") {
                let owner = e.detail.dropFirst(6).split(separator: "/").first.map(String.init) ?? ""
                if owner != t.name { return "write into another app's code: \(e.detail)" }
            }
        }
        return nil
    }

    private func decide(_ t: AppTrack, _ d0: ShieldDecision, scores: TrainingScores? = nil) {
        var d = d0
        let malicious = d.pMalicious >= 0.9 && (d.action == "freeze_app" || d.action == "block_host")
        if malicious {
            d.warned = warn(t)
            if d.note.isEmpty { d.note = "Monitoring only" }
        } else if d.pMalicious >= 0.5 {
            d.note = "watching"
        }
        record(d, scores: scores)
    }

    private func record(_ decision: ShieldDecision, scores: TrainingScores? = nil) {
        appendVerdict(decision, scores: scores)
        snap.decisions.insert(decision, at: 0)
        if snap.decisions.count > 30 { snap.decisions.removeLast() }
        publish()
    }

    /// Repeated windows from one app share a warning for one minute.
    private func warn(_ track: AppTrack) -> Bool {
        let now = Date()
        guard let link, now.timeIntervalSince(track.lastWarning) >= 60 else { return false }
        do {
            let (envelope, seq) = try signer.warning()
            pending[seq] = "Home warning"
            link.send(kind: 0x10, payload: envelope)
            track.lastWarning = now
            snap.shieldsSent += 1
            return true
        } catch {
            shieldLog.error("warning signature failed: \(error.localizedDescription, privacy: .public)")
            return false
        }
    }

    /// The relay saw a connection VioletOS never reported: telemetry is missing or tampered.
    private func checkUnreportedFlows(_ now: Date) {
        if agentSession != nil {
            guard let snapshot = proxySnapshot, now.timeIntervalSince(proxyReceived) < 5 else { return }
            var missing: [SocksOnlyFlow] = []
            for flow in snapshot.flows where !proxyFlagged.contains(flow.id) {
                let started = proxySampleMs - Int64(flow.ageMs)
                guard started > proxyStartMs + 2000, flow.ageMs >= 5000, telemetryThroughMs >= started + 5000 else { continue }
                proxyFlagged.insert(flow.id)
                if proxyReports.contains(where: { $0.target == flow.kernelTarget && abs($0.time - started) <= 15_000 }) { continue }
                missing.append(.init(target: flow.target, tx: flow.up, rx: flow.down))
            }
            reportUnmatched(missing)
            return
        }
        guard let up = linkUpSince, now.timeIntervalSince(up) > 5 else { return }
        lock.lock()
        let candidates = flows.values.filter { f in
            !flaggedFlows.contains(f.id) && f.started > up.addingTimeInterval(2) && now.timeIntervalSince(f.started) > 3
        }
        var unreported: [FlowEntry] = []
        for f in candidates {
            flaggedFlows.insert(f.id)
            if let r = reported[f.target], abs(r.timeIntervalSince(f.started)) < 15 { continue }
            unreported.append(f)
        }
        lock.unlock()
        reportUnmatched(unreported.map { SocksOnlyFlow(target: $0.target, tx: $0.up, rx: $0.down) })
    }

    private func reportUnmatched(_ unreported: [SocksOnlyFlow]) {
        guard !unreported.isEmpty else { return }
        let ghost = tracks[0] ?? AppTrack(pid: 0, name: "unattributed", sha8: "00000000")
        tracks[0] = ghost
        let window = ghost.window(unreported: unreported) { _ in nil }
        let hosts = unreported.map(\.target)
        let d = ShieldDecision(time: Date(), pid: 0, app: "unattributed", tier: "relay", verdict: "exfil_or_c2", pMalicious: 1,
                               action: "block_host", tags: [("socks_sentinel_mismatch", 1)], novelty: 0, tokens: 0, latencyMs: 0,
                               hosts: hosts, window: window.canonicalText(), warned: warn(ghost),
                               note: "relay saw a connection the Sentinel never reported")
        record(d)
        // The model's own view of the same evidence, for the record.
        if let model {
            let text = window.fitText(countTokens: model.countTokens)
            modelQueue.async {
                guard let e = try? model.embed(text),
                      let out = try? model.score(embedding: e.embedding, side: window.sideFeatures(minCosineDistance: ShieldSchema.noHistory)) else { return }
                self.queue.async {
                    if let i = self.snap.decisions.firstIndex(where: { $0.id == d.id }) {
                        self.snap.decisions[i].note += String(format: "; model agrees: p_mal %.2f", 1 - out.verdict[0])
                        self.publish()
                    }
                }
            }
        }
    }

    func label(_ decision: ShieldDecision, benign: Bool) {
        queue.async {
            self.feedback.label(decision, as: benign ? "benign_allow" : "confirmed_threat")
        }
    }

    // MARK: - Status

    private var lastPublish = Date.distantPast
    private func publish() {
        let s = snap
        DispatchQueue.main.async { ShieldUI.shared.snap = s }
        if Date().timeIntervalSince(lastPublish) > 0.5 {
            lastPublish = Date()
            writeStatus(s)
        }
    }

    private var verdictWriteFailed = false

    // This runs on the Shield queue, where decisions arrive in order.
    private func appendVerdict(_ d: ShieldDecision, scores: TrainingScores?) {
        let url = dataDirectory.appendingPathComponent("shield-verdicts.jsonl")
        var row = decisionFields(d)
        row["schema_version"] = 1
        row["boot_id"] = snap.bootId
        row["transport"] = verdictTransport
        row["model"] = d.tier == "model" ? snap.modelState : ""
        row["tag_scores"] = Dictionary(uniqueKeysWithValues: d.tags)
        if let scores {
            row["model_input"] = scores.input
            row["side_features"] = scores.sideFeatures
            row["verdict_probs"] = scores.verdict
            row["action_probs"] = scores.action
            row["tag_probs"] = scores.tags
        }

        do {
            var line = try JSONSerialization.data(withJSONObject: row, options: [.sortedKeys])
            line.append(0x0a)
            let fd = open(url.path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK,
                          S_IRUSR | S_IWUSR)
            guard fd >= 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
            let file = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
            var info = stat()
            guard fstat(fd, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG, info.st_uid == getuid(),
                  fchmod(fd, S_IRUSR | S_IWUSR) == 0 else { throw CocoaError(.fileWriteNoPermission) }
            try file.write(contentsOf: line)
            verdictWriteFailed = false
        } catch {
            if !verdictWriteFailed {
                shieldLog.error("Could not save verdicts: \(error.localizedDescription, privacy: .public)")
                verdictWriteFailed = true
            }
        }
    }

    private func decisionFields(_ d: ShieldDecision) -> [String: Any] {
        ["id": d.id.uuidString, "time": ISO8601DateFormatter().string(from: d.time), "pid": d.pid, "app": d.app,
         "tier": d.tier, "verdict": d.verdict, "p_malicious": d.pMalicious, "action": d.action, "tags": d.tags.map(\.0),
         "novelty": d.novelty, "tokens": d.tokens, "latency_ms": Int(d.latencyMs), "hosts": d.hosts,
         "contained": false, "warned": d.warned, "note": d.note, "window": d.window]
    }

    private func writeStatus(_ s: ShieldSnapshot) {
        let decisions = s.decisions.prefix(30).map(decisionFields)
        let obj: [String: Any] = ["model": s.modelState, "compute_units": s.computeUnits, "signer": s.signer, "link": s.link, "link_up": s.linkUp,
                                  "flow_observer": agentSession == nil ? (s.linkUp ? "Mac" : "") : (Date().timeIntervalSince(proxyReceived) < 5 ? proxySnapshot?.source ?? "" : ""),
                                  "boot_id": s.bootId, "chain": s.chain, "heartbeat": s.heartbeat, "events": s.events,
                                  "windows_scored": s.windowsScored, "shields_sent": s.shieldsSent, "shields_accepted": s.shieldsAccepted,
                                  "shields_rejected": s.shieldsRejected, "auto_contain": false,
                                  "last_latency_ms": Int(s.lastLatencyMs), "decisions": decisions]
        let url = dataDirectory.appendingPathComponent("shield-status.json")
        if let data = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: url, options: .atomic)
        }
    }

    /// Compare this phone's tokenizer, embedder and head with the Mac's (fixtures.json from train.py).
    private func selfTest() {
        guard let model, let url = Bundle.main.url(forResource: "ShieldFixtures", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let fixtures = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else { return }
        modelQueue.async {
            var results: [[String: Any]] = []
            for fx in fixtures {
                let text = fx["text"] as? String ?? ""
                let want = (fx["token_ids"] as? [Int]) ?? []
                let got = model.tokenIds(text)
                let side = (fx["side"] as? [Double] ?? []).map(Float.init)
                var r: [String: Any] = ["family": fx["family"] ?? "", "tokens_match": got == want, "tokens": got.count]
                if let e = try? model.embed(text), let out = try? model.score(embedding: e.embedding, side: side) {
                    let wantE = (fx["embedding_head"] as? [Double] ?? []).map(Float.init)
                    r["embedding_max_diff"] = zip(e.embedding.prefix(8), wantE).map { abs($0 - $1) }.max() ?? -1
                    let wantV = (fx["verdict_probs"] as? [Double] ?? []).map(Float.init)
                    r["verdict_max_diff"] = zip(out.verdict, wantV).map { abs($0 - $1) }.max() ?? -1
                    r["latency_ms"] = Int(e.ms)
                }
                results.append(r)
            }
            let out = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("shield-selftest.json")
            if let d = try? JSONSerialization.data(withJSONObject: results, options: [.prettyPrinted, .sortedKeys]) { try? d.write(to: out) }
        }
    }
}

// MARK: - sentinel.violet link

/// One TCP stream from VioletOS's Sentinel, handed over by the SOCKS5 session.
/// Frames: [type u8][len u32 LE][payload].
final class SentinelLink {
    private let conn: NWConnection?
    private let forward: ((Data) -> Void)?
    private let queue: DispatchQueue
    private weak var shield: Shield?
    private var buf = Data()
    private var closed = false

    init(conn: NWConnection? = nil, queue: DispatchQueue, shield: Shield, forward: ((Data) -> Void)? = nil) {
        self.conn = conn
        self.forward = forward
        self.queue = queue
        self.shield = shield
    }

    func start() {
        guard let conn else { return }
        conn.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .failed(let e): self.queue.async { self.finish("\(e)") }
            case .cancelled: self.queue.async { self.finish("closed") }
            default: break
            }
        }
        receive()
    }

    private func receive() {
        conn?.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            guard let self else { return }
            self.queue.async {
                if let data { self.feed(data) }
                if done || error != nil { self.finish(error.map { "\($0)" } ?? "VioletOS closed the link"); return }
                self.receive()
            }
        }
    }

    func feed(_ data: Data) {
        guard !closed else { return }
        buf.append(data)
        while buf.count >= 5 {
            let b = [UInt8](buf.prefix(5))
            let len = Int(b[1]) | Int(b[2]) << 8 | Int(b[3]) << 16 | Int(b[4]) << 24
            guard len <= 1 << 20 else { finish("oversized frame"); return }
            guard buf.count >= 5 + len else { break }
            let payload = buf.subdata(in: buf.startIndex + 5 ..< buf.startIndex + 5 + len)
            buf.removeFirst(5 + len)
            shield?.frame(b[0], payload)
        }
    }

    func send(kind: UInt8, payload: Data) {
        var f = Data([kind])
        f.appendLE(UInt32(payload.count))
        f.append(payload)
        if let forward { forward(f) } else { conn?.send(content: f, completion: .contentProcessed { _ in }) }
    }

    func close() {
        closed = true
        conn?.cancel()
    }

    private func finish(_ why: String) {
        guard !closed else { return }
        closed = true
        conn?.cancel()
        shield?.linkClosed(self, why)
    }
}

/// Labels are stored locally for training feedback.
private final class AlertFeedbackStore {
    private let url: URL

    init(directory: URL) {
        url = directory.appendingPathComponent("shield-feedback.jsonl")
    }

    func label(_ d: ShieldDecision, as label: String) {
        let entry: [String: Any] = ["verdictID": d.id.uuidString, "timestamp": ISO8601DateFormatter().string(from: Date()), "canonicalWindow": d.window,
                                    "modelVerdict": d.verdict, "pMalicious": d.pMalicious, "userLabel": label]
        guard var line = try? JSONSerialization.data(withJSONObject: entry) else { return }
        line.append(0x0a)
        if let file = try? FileHandle(forWritingTo: url) {
            file.seekToEndOfFile()
            file.write(line)
            try? file.close()
        } else {
            try? line.write(to: url)
        }
    }
}
