import Foundation
import Network
import UserNotifications
import os

// Violet Shield on the iPhone companion (docs/violet-shield-rfc.md, docs/violet-shield-poc.md).
//
// - `sentinel.violet` is answered by this relay itself (next to `time.violet`):
//   VioletOS's Sentinel streams syscall telemetry here and receives signed Shields.
// - The SOCKS5 relay is the second, independent ground truth: every connection
//   VioletOS makes passes through it, so a connection the Sentinel never reported
//   is itself evidence of tampering.
// - Tier 0: deterministic rules. Tier 1: EmbeddingGemma 2 + hybrid head + per-app
//   novelty, on the phone. Tier 2: containment, signed with the phone's key.

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
    var contained: Bool
    var note: String
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
    var blocked: [String] = []
    var frozen: [String] = []
    var autoContain = true
    var decisions: [ShieldDecision] = []
    var lastLatencyMs: Double = 0
}

@MainActor
final class ShieldUI: ObservableObject {
    static let shared = ShieldUI()
    @Published var snap = ShieldSnapshot()
}

final class Shield: NSObject, UNUserNotificationCenterDelegate {
    static let shared = Shield()

    let queue = DispatchQueue(label: "com.immortal.violet.shield")
    private let modelQueue = DispatchQueue(label: "com.immortal.violet.shield.model", qos: .userInitiated)
    private var model: ShieldModel?
    private let signer = ShieldSigner()
    private let novelty = AppNoveltyIndex()
    private let feedback = AlertFeedbackStore()
    private var tracks: [UInt32: AppTrack] = [:]
    private var link: SentinelLink?
    private var linkUpSince: Date?
    private var snap = ShieldSnapshot()
    private var pending: [UInt64: String] = [:]
    private var scoring = Set<UInt32>()
    private weak var relay: RelayServer?
    private var flaggedFlows = Set<Int>()

    // SOCKS flows, touched from the relay queue under `lock`.
    private let lock = NSLock()
    private var blockedTargets = Set<String>()
    private var blockedHosts = Set<String>()
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

    var freezeTTL: UInt32 {
        let v = UserDefaults.standard.integer(forKey: "shieldFreezeTTL")
        return v > 0 ? UInt32(min(v, 3600)) : 600
    }

    // MARK: - Start

    func start(relay: RelayServer) {
        self.relay = relay
        snap.signer = signer.keyDescription
        snap.autoContain = !UserDefaults.standard.bool(forKey: "shieldNotifyOnly")
        UNUserNotificationCenter.current().delegate = self
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in }
        let release = UNNotificationAction(identifier: "RELEASE", title: "Release")
        let benign = UNNotificationAction(identifier: "BENIGN", title: "Allow & Learn")
        let threat = UNNotificationAction(identifier: "THREAT", title: "Confirmed Threat", options: [.destructive])
        UNUserNotificationCenter.current().setNotificationCategories([
            UNNotificationCategory(identifier: "VIOLET_SHIELD_ALERT", actions: [threat, benign, release], intentIdentifiers: []),
        ])
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

    func isBlocked(host: String, port: UInt16) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return blockedHosts.contains(host.lowercased()) || blockedTargets.contains("\(host.lowercased()):\(port)")
    }

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
            self.link?.close()
            let l = SentinelLink(conn: conn, queue: self.queue, shield: self)
            self.link = l
            self.linkUpSince = Date()
            self.snap.link = "linked"
            self.snap.linkUp = true
            l.start()
            self.publish()
        }
    }

    // MARK: - Sentinel frames (shield queue)

    fileprivate func linkClosed(_ l: SentinelLink, _ why: String) {
        guard link === l else { return }
        link = nil
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
            pushBaseline()
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
                snap.chain = String(f[2].prefix(16))
                snap.heartbeat = "up \(Int(f[0]).map { $0 / 1000 } ?? 0)s · \(f[4]) rules · \(f[5]) frozen · chain #\(f[1]) · \(f[3]) dropped"
            }
        case 0x04:
            let f = text.split(separator: " ")
            if f.count >= 2, let seq = UInt64(f[1]) {
                let what = pending.removeValue(forKey: seq) ?? "shield"
                if f[0] == "ok" {
                    snap.shieldsAccepted += 1
                    note("VioletOS verified and applied \(what)")
                } else {
                    snap.shieldsRejected += 1
                    note("VioletOS rejected \(what): \(f.dropFirst(2).joined(separator: " "))")
                }
            }
        default:
            break
        }
        publish()
    }

    private var baselineIds: [UInt32] = []
    private var baselineAt = Date.distantPast

    private func pushBaseline() {
        if baselineIds.isEmpty { baselineIds = ShieldSigner.baseline.map { _ in signer.newShieldId() } }
        for (id, rule) in zip(baselineIds, ShieldSigner.baseline) {
            let payload = ShieldSigner.rulePayload(rule.0, rule.1, rule.2, writeOnly: rule.3, crossAppOnly: rule.4)
            send(op: .install, pid: 0, ttl: 3600, payload: payload, what: "baseline: freeze writes to \(rule.2)\(rule.4 ? " of other apps" : "")", shieldId: id)
        }
        baselineAt = Date()
    }

    private func note(_ s: String) {
        shieldLog.info("\(s, privacy: .public)")
        if let i = snap.decisions.indices.first {
            snap.decisions[i].note = s
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
        case "FROZEN": t.frozen = true
        case "THAWED": t.frozen = false; t.contained = false
        case "SOCK_CONN":
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
        if link != nil, now.timeIntervalSince(baselineAt) > 3000 { pushBaseline() }
        checkUnreportedFlows(now)
        for t in tracks.values where !t.exited && !t.frozen && !scoring.contains(t.pid) {
            guard let since = t.dirtySince else { continue }
            let quiet = (t.records.last.map { now.timeIntervalSince($0.arrived) } ?? 1) >= 0.3
            // Score when the app pauses, or every second while it keeps going.
            if quiet || now.timeIntervalSince(since) >= 1.0 {
                t.dirtySince = nil
                evaluate(t)
            }
        }
        var frozen: [String] = []
        for t in tracks.values where t.frozen && !t.exited { frozen.append("\(t.name) (pid \(t.pid))") }
        snap.frozen = frozen
        publish()
    }

    private func socksSeen(_ now: Date) -> Set<String> {
        lock.lock(); defer { lock.unlock() }
        return Set(flows.values.filter { now.timeIntervalSince($0.started) < 180 }.map(\.target))
    }

    private func evaluate(_ t: AppTrack) {
        let now = Date()
        let window = t.window(socksSeen: socksSeen(now), unreported: [])
        // Tier 0: rules no window text can talk its way past.
        if let rule = tier0(t, window) {
            let d = ShieldDecision(time: now, pid: t.pid, app: t.name, tier: "tier0", verdict: "exploit_or_persistence", pMalicious: 1,
                                   action: "freeze_app", tags: [("persistence_write", 1)], novelty: 0, tokens: 0, latencyMs: 0,
                                   hosts: t.newTargets(in: window), window: window.canonicalText(), contained: false, note: rule)
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
                self.queue.async { self.scoring.remove(t.pid) }
                return
            }
            self.queue.async {
                let dist = self.novelty.minCosineDistance(key: key, embedding: e.embedding)
                let side = window.sideFeatures(minCosineDistance: dist)
                self.modelQueue.async {
                    let out = try? model.score(embedding: e.embedding, side: side)
                    let ms = (CFAbsoluteTimeGetCurrent() - t0) * 1000
                    self.queue.async {
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
                                               tokens: e.tokens, latencyMs: ms, hosts: Array(Set(hosts)).sorted(), window: text,
                                               contained: false, note: "")
                        shieldLog.info("scored \(t.name, privacy: .public) pid \(t.pid): \(d.verdict, privacy: .public) p_mal=\(pMal) \(d.action, privacy: .public) \(Int(ms))ms")
                        if out.verdict[0] >= 0.9 {
                            // Confidently benign: it becomes this app's history.
                            self.novelty.recordBenign(key: key, embedding: e.embedding)
                            for h in window.events where h.kind == "SOCK_CONN" && !t.knownHosts.contains(h.detail) {
                                t.knownHosts.append(h.detail)
                            }
                        }
                        self.decide(t, d)
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

    private func decide(_ t: AppTrack, _ d0: ShieldDecision) {
        var d = d0
        let malicious = d.pMalicious >= 0.9 && (d.action == "freeze_app" || d.action == "block_host")
        t.lastDecision = d
        if malicious && !t.contained {
            if snap.autoContain {
                contain(t, &d)
            } else {
                d.note = "notify-only mode: nothing blocked"
            }
            alert(d)
        } else if d.pMalicious >= 0.5 {
            d.note = "watching"
        }
        snap.decisions.insert(d, at: 0)
        if snap.decisions.count > 30 { snap.decisions.removeLast() }
        if malicious { feedback.recordPending(d) }
        publish()
    }

    /// Block in the relay at once, then ask VioletOS (signed) to freeze the app and deny its hosts.
    private func contain(_ t: AppTrack, _ d: inout ShieldDecision) {
        t.contained = true
        d.contained = true
        let hosts = d.hosts
        // New connections are refused at once. A live upload is cut a moment after the
        // freeze goes out, so VioletOS stops the app inside its send rather than on an error.
        let freezing = d.action == "freeze_app" && signer.canSign && link != nil
        block(hosts, cutAfter: freezing ? 1.0 : 0)
        var notes: [String] = []
        if !hosts.isEmpty { notes.append("relay blocked \(hosts.joined(separator: ", "))") }
        guard signer.canSign, link != nil else {
            d.note = notes.joined(separator: "; ") + (link == nil ? "; VioletOS offline" : "; no signing key")
            return
        }
        let ttl = freezeTTL
        if d.action == "freeze_app" {
            send(op: .freeze, pid: t.pid, ttl: ttl, payload: Data(), what: "freeze of \(t.name) (pid \(t.pid)) for \(ttl)s")
            notes.append("signed FREEZE_APP pid \(t.pid) ttl \(ttl)s")
        }
        for h in hosts {
            let host = h.split(separator: ":").dropLast().joined(separator: ":")
            let payload = ShieldSigner.rulePayload(.host, .deny, host.isEmpty ? h : host)
            send(op: .install, pid: 0, ttl: ttl, payload: payload, what: "DENY \(host) for \(ttl)s")
            notes.append("signed DENY \(host)")
        }
        d.note = notes.joined(separator: "; ")
    }

    private func send(op: ShieldSigner.Op, pid: UInt32, ttl: UInt32, payload: Data, what: String, shieldId: UInt32? = nil) {
        guard let link else { return }
        do {
            let (env, seq) = try signer.envelope(op: op, shieldId: shieldId ?? signer.newShieldId(), ttlSecs: ttl, pid: pid, payload: payload)
            pending[seq] = what
            link.send(kind: 0x10, payload: env)
            snap.shieldsSent += 1
        } catch {
            shieldLog.error("sign failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func block(_ targets: [String], cutAfter delay: Double = 0) {
        lock.lock()
        for t in targets {
            blockedTargets.insert(t.lowercased())
            if let host = t.split(separator: ":").first { blockedHosts.insert(String(host).lowercased()) }
        }
        snap.blocked = Array(blockedHosts).sorted()
        lock.unlock()
        let hosts = targets.compactMap { $0.split(separator: ":").first.map { String($0).lowercased() } }
        relay?.queue.asyncAfter(deadline: .now() + delay) { self.relay?.abortSessions(toHosts: Set(hosts)) }
    }

    /// The relay saw a connection VioletOS never reported: telemetry is missing or tampered.
    private func checkUnreportedFlows(_ now: Date) {
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
        guard !unreported.isEmpty else { return }
        let ghost = tracks[0] ?? AppTrack(pid: 0, name: "unattributed", sha8: "00000000")
        tracks[0] = ghost
        let window = ghost.window(socksSeen: [], unreported: unreported.map { SocksOnlyFlow(target: $0.target, tx: $0.up, rx: $0.down) })
        let hosts = unreported.map(\.target)
        var d = ShieldDecision(time: now, pid: 0, app: "unattributed", tier: "relay", verdict: "exfil_or_c2", pMalicious: 1,
                               action: "block_host", tags: [("socks_sentinel_mismatch", 1)], novelty: 0, tokens: 0, latencyMs: 0,
                               hosts: hosts, window: window.canonicalText(), contained: true,
                               note: "relay saw a connection the Sentinel never reported")
        block(hosts)
        if signer.canSign, link != nil {
            for h in hosts {
                let host = h.split(separator: ":").first.map(String.init) ?? h
                send(op: .install, pid: 0, ttl: freezeTTL, payload: ShieldSigner.rulePayload(.host, .deny, host), what: "DENY \(host)")
            }
            d.note += "; signed DENY " + hosts.joined(separator: ", ")
        }
        alert(d)
        snap.decisions.insert(d, at: 0)
        publish()
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

    // MARK: - User actions

    func release(_ d: ShieldDecision) {
        queue.async {
            self.lock.lock()
            for h in d.hosts {
                self.blockedTargets.remove(h.lowercased())
                if let host = h.split(separator: ":").first { self.blockedHosts.remove(String(host).lowercased()) }
            }
            self.snap.blocked = Array(self.blockedHosts).sorted()
            self.lock.unlock()
            if d.pid != 0 {
                self.send(op: .unfreeze, pid: d.pid, ttl: 0, payload: Data(), what: "release of \(d.app)")
                self.tracks[d.pid]?.contained = false
            }
            self.publish()
        }
    }

    func label(_ d: ShieldDecision, benign: Bool) {
        queue.async {
            self.feedback.label(d, as: benign ? "benign_allow" : "confirmed_threat")
            if benign { self.release(d) }
        }
    }

    func setAutoContain(_ on: Bool) {
        queue.async {
            self.snap.autoContain = on
            UserDefaults.standard.set(!on, forKey: "shieldNotifyOnly")
            self.publish()
        }
    }

    private func alert(_ d: ShieldDecision) {
        let content = UNMutableNotificationContent()
        content.title = d.contained ? "Violet Shield contained \(d.app)" : "Violet Shield flagged \(d.app)"
        content.body = "\(d.verdict.replacingOccurrences(of: "_", with: " ")) · p=\(String(format: "%.2f", d.pMalicious)) · "
            + (d.hosts.isEmpty ? d.action : d.hosts.joined(separator: ", "))
        content.categoryIdentifier = "VIOLET_SHIELD_ALERT"
        content.userInfo = ["decision": d.id.uuidString]
        content.sound = .default
        UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: d.id.uuidString, content: content, trigger: nil))
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .sound, .list])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let id = response.notification.request.content.userInfo["decision"] as? String
        queue.async {
            if let d = self.snap.decisions.first(where: { $0.id.uuidString == id }) {
                switch response.actionIdentifier {
                case "RELEASE": self.release(d)
                case "BENIGN": self.label(d, benign: true)
                case "THREAT": self.label(d, benign: false)
                default: break
                }
            }
        }
        completionHandler()
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

    private func writeStatus(_ s: ShieldSnapshot) {
        let decisions: [[String: Any]] = s.decisions.prefix(30).map { d in
            ["time": ISO8601DateFormatter().string(from: d.time), "pid": d.pid, "app": d.app, "tier": d.tier, "verdict": d.verdict,
             "p_malicious": d.pMalicious, "action": d.action, "tags": d.tags.map(\.0), "novelty": d.novelty, "tokens": d.tokens,
             "latency_ms": Int(d.latencyMs), "hosts": d.hosts, "contained": d.contained, "note": d.note, "window": d.window]
        }
        let obj: [String: Any] = ["model": s.modelState, "compute_units": s.computeUnits, "signer": s.signer, "link": s.link, "link_up": s.linkUp,
                                  "boot_id": s.bootId, "chain": s.chain, "heartbeat": s.heartbeat, "events": s.events,
                                  "windows_scored": s.windowsScored, "shields_sent": s.shieldsSent, "shields_accepted": s.shieldsAccepted,
                                  "shields_rejected": s.shieldsRejected, "blocked": s.blocked, "frozen": s.frozen, "auto_contain": s.autoContain,
                                  "last_latency_ms": Int(s.lastLatencyMs), "decisions": decisions]
        let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("shield-status.json")
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
    private let conn: NWConnection
    private let queue: DispatchQueue
    private weak var shield: Shield?
    private var buf = Data()
    private var closed = false

    init(conn: NWConnection, queue: DispatchQueue, shield: Shield) {
        self.conn = conn
        self.queue = queue
        self.shield = shield
    }

    func start() {
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
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            guard let self else { return }
            self.queue.async {
                if let data { self.buf.append(data); self.drain() }
                if done || error != nil { self.finish(error.map { "\($0)" } ?? "VioletOS closed the link"); return }
                self.receive()
            }
        }
    }

    private func drain() {
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
        conn.send(content: f, completion: .contentProcessed { _ in })
    }

    func close() {
        closed = true
        conn.cancel()
    }

    private func finish(_ why: String) {
        guard !closed else { return }
        closed = true
        conn.cancel()
        shield?.linkClosed(self, why)
    }
}

// MARK: - Active learning

/// Alerts the user labels, kept as `Documents/shield-feedback.jsonl` for the Mac trainer.
final class AlertFeedbackStore {
    private var pending: [UUID: ShieldDecision] = [:]
    private let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("shield-feedback.jsonl")

    func recordPending(_ d: ShieldDecision) { pending[d.id] = d }

    func label(_ d: ShieldDecision, as label: String) {
        let entry: [String: Any] = ["timestamp": ISO8601DateFormatter().string(from: Date()), "canonicalWindow": d.window,
                                    "modelVerdict": d.verdict, "pMalicious": d.pMalicious, "userLabel": label]
        guard var line = try? JSONSerialization.data(withJSONObject: entry) else { return }
        line.append(0x0a)
        if let h = try? FileHandle(forWritingTo: url) {
            h.seekToEndOfFile(); h.write(line); try? h.close()
        } else {
            try? line.write(to: url)
        }
        pending.removeValue(forKey: d.id)
    }
}
