import Foundation
import Network
import NetworkExtension
import UIKit

/// App configuration. Read from launch arguments (`-key value` lands in UserDefaults' argument domain)
/// and persisted so a later plain launch keeps the pairing secret.
enum RelayConfig {
    static let username = "violet"
    static var port: UInt16 {
        let v = UserDefaults.standard.integer(forKey: "port")
        return (1...65535).contains(v) ? UInt16(v) : 1080
    }

    static var pairingSecret: String {
        let d = UserDefaults.standard
        if let fromArgs = d.volatileDomain(forName: UserDefaults.argumentDomain)["pairingSecret"] as? String,
           !fromArgs.isEmpty {
            d.set(fromArgs, forKey: "pairingSecret")
            return fromArgs
        }
        if let env = ProcessInfo.processInfo.environment["VIOLET_PAIRING_SECRET"], !env.isEmpty { return env }
        if let code = pairingCode { return String(code.suffix(8)) }
        return d.string(forKey: "pairingSecret") ?? "violet-dev"
    }

    /// The 12-character code shown on VioletOS's Phone Internet page: the first four
    /// characters name the network, the last eight are the relay password.
    static var pairingCode: String? {
        get { UserDefaults.standard.string(forKey: "pairingCode") }
        set {
            UserDefaults.standard.set(newValue, forKey: "pairingCode")
            // A code replaces any secret set by the Mac tools.
            if newValue != nil { UserDefaults.standard.removeObject(forKey: "pairingSecret") }
        }
    }

    /// Uppercase hex only; spaces and dashes are ignored. Nil unless exactly 12 characters.
    static func normalizedCode(_ text: String) -> String? {
        let code = text.uppercased().filter { !" -".contains($0) }
        guard code.count == 12, code.allSatisfy({ "0123456789ABCDEF".contains($0) }) else { return nil }
        return code
    }

    static func ssid(forCode code: String) -> String { "VioletRelayOpen-" + code.prefix(4) }

    /// `-joinSSID NAME [-joinPassphrase PASS]` from the launch arguments only (not persisted).
    static var joinRequest: (ssid: String, passphrase: String)? {
        let args = UserDefaults.standard.volatileDomain(forName: UserDefaults.argumentDomain)
        guard let ssid = args["joinSSID"] as? String, !ssid.isEmpty else { return nil }
        return (ssid, (args["joinPassphrase"] as? String) ?? "")
    }

    /// Simulator demos: `-hostMapB64 <base64 of {"drop.c2-exfil.test":"127.0.0.1:9999"}>` sends test names to local
    /// stand-ins, so nothing reaches a real address. The relay still logs the name it was asked for.
    static let hostMap: [String: String] = {
        // Launch arguments are parsed as property lists, so JSON object syntax usually arrives as a dictionary.
        // `-hostMapB64` (base64 JSON) avoids the property-list parsing of launch arguments.
        let args = UserDefaults.standard.volatileDomain(forName: UserDefaults.argumentDomain)
        var parsed = args["hostMap"] as? [String: String]
        var json = (args["hostMap"] as? String)?.data(using: .utf8)
        if let b = args["hostMapB64"] as? String { json = Data(base64Encoded: b) }
        if parsed == nil, let d = json {
            parsed = try? JSONSerialization.jsonObject(with: d) as? [String: String]
        }
        guard let m = parsed else { return [:] }
        return Dictionary(uniqueKeysWithValues: m.map { ($0.key.lowercased(), $0.value) })
    }()

    static var forgetSSID: String? {
        UserDefaults.standard.volatileDomain(forName: UserDefaults.argumentDomain)["forgetSSID"] as? String
    }
}

struct RelayStatus: Codable {
    var updatedAt: Date
    var launchedAt: Date
    var listenerState: String
    var port: UInt16          // port actually listening (0 if none)
    var requestedPort: UInt16
    var wifiIPv4: [String]
    var allIPv4: [String: [String]]
    var cellularAvailable: Bool
    var activeConnections: Int
    var totalConnections: Int
    var failedConnections: Int
    var authFailures: Int
    var bytesUp: UInt64
    var bytesDown: UInt64
    var lastError: String?
    var lastErrorAt: Date?
    var hotspot: String?
    var background: String?
    var backgroundEvents: [String] = []
    var recentErrors: [String] = []
}

@MainActor
final class RelayModel: ObservableObject {
    @Published var status: RelayStatus
    private let server: RelayServer
    private let launchedAt = Date()
    private var hotspotState: String?
    private var cellularAvailable = false
    private let cellMonitor = NWPathMonitor(requiredInterfaceType: .cellular)
    private var tick = 0
    private let keepAlive = KeepAlive()

    static var statusURL: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("status.json")
    }

    init() {
        server = RelayServer(port: RelayConfig.port, username: RelayConfig.username, password: RelayConfig.pairingSecret)
        status = RelayStatus(updatedAt: Date(), launchedAt: launchedAt, listenerState: "starting", port: 0, requestedPort: RelayConfig.port,
                             wifiIPv4: [], allIPv4: [:], cellularAvailable: false, activeConnections: 0,
                             totalConnections: 0, failedConnections: 0, authFailures: 0, bytesUp: 0, bytesDown: 0)
    }

    func start() {
        UIApplication.shared.isIdleTimerDisabled = true
        keepAlive.start()
        cellMonitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in self?.cellularAvailable = path.status == .satisfied }
        }
        cellMonitor.start(queue: .main)
        StandIns.shared.start(kinds: Set(RelayConfig.hostMap.values.filter { $0.hasPrefix("standin:") }.map { String($0.dropFirst(8)) }))
        server.start()
        Shield.shared.start(relay: server)
        if let ssid = RelayConfig.forgetSSID, !ssid.isEmpty {
            if ssid.hasSuffix("*") {
                // "VioletRelay*" removes every configuration this app saved with that prefix.
                let prefix = String(ssid.dropLast())
                NEHotspotConfigurationManager.shared.getConfiguredSSIDs { [weak self] ssids in
                    let matches = ssids.filter { $0.hasPrefix(prefix) }
                    matches.forEach { NEHotspotConfigurationManager.shared.removeConfiguration(forSSID: $0) }
                    self?.hotspotState = "removed \(matches.count) configurations matching \(ssid)"
                }
            } else {
                NEHotspotConfigurationManager.shared.removeConfiguration(forSSID: ssid)
                hotspotState = "removed configuration for \(ssid)"
            }
        }
        if let join = RelayConfig.joinRequest { joinNetwork(ssid: join.ssid, passphrase: join.passphrase) }
        Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
        refresh()
    }

    /// Save a pairing code from VioletOS and join its network. Returns false for a bad code.
    @discardableResult
    func pair(code text: String) -> Bool {
        guard let code = RelayConfig.normalizedCode(text) else {
            hotspotState = "That code isn't 12 letters and numbers (0-9, A-F)"
            refresh()
            return false
        }
        RelayConfig.pairingCode = code
        server.setPassword(String(code.suffix(8)))
        joinNetwork(ssid: RelayConfig.ssid(forCode: code), passphrase: "")
        return true
    }

    /// Join (and remember) a Wi-Fi network. Empty passphrase = open network.
    func joinNetwork(ssid: String, passphrase: String) {
        let config = passphrase.isEmpty
            ? NEHotspotConfiguration(ssid: ssid)
            : NEHotspotConfiguration(ssid: ssid, passphrase: passphrase, isWEP: false)
        config.joinOnce = false
        hotspotState = "joining \(ssid)…"
        relayLog.info("joining \(ssid, privacy: .public)")
        NEHotspotConfigurationManager.shared.apply(config) { [weak self] error in
            Task { @MainActor in
                guard let self else { return }
                if let error = error as NSError? {
                    if error.domain == NEHotspotConfigurationErrorDomain,
                       error.code == NEHotspotConfigurationError.alreadyAssociated.rawValue {
                        self.hotspotState = "joined \(ssid) (already associated)"
                    } else {
                        self.hotspotState = "join \(ssid) failed: \(error.localizedDescription) [\(error.code)]"
                    }
                    relayLog.info("hotspot: \(self.hotspotState ?? "", privacy: .public)")
                } else {
                    // apply() reports success even when iOS then shows "Unable to join", so confirm
                    // the association with fetchCurrent (allowed for networks this app configured).
                    self.hotspotState = "apply ok for \(ssid); verifying…"
                    self.verifyJoined(ssid, attempt: 1)
                }
            }
        }
    }

    private func verifyJoined(_ ssid: String, attempt: Int) {
        NEHotspotNetwork.fetchCurrent { [weak self] network in
            Task { @MainActor in
                guard let self else { return }
                if network?.ssid == ssid {
                    self.hotspotState = "joined \(ssid)"
                } else if attempt < 10 {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { self.verifyJoined(ssid, attempt: attempt + 1) }
                    return
                } else {
                    self.hotspotState = "join \(ssid) failed: current Wi-Fi is \(network?.ssid ?? "unknown/none")"
                }
                relayLog.info("hotspot: \(self.hotspotState ?? "", privacy: .public)")
            }
        }
    }

    func refresh() {
        let snapshot = server.queue.sync { () -> RelayCounters in
            let c = RelayCounters()
            let s = server.counters
            c.activeConnections = s.activeConnections; c.totalConnections = s.totalConnections
            c.failedConnections = s.failedConnections; c.authFailures = s.authFailures
            c.bytesUp = s.bytesUp; c.bytesDown = s.bytesDown
            c.lastError = s.lastError; c.lastErrorAt = s.lastErrorAt; c.listenerState = s.listenerState
            c.recentErrors = s.recentErrors
            c.boundPort = s.boundPort
            return c
        }
        let ifs = Self.ipv4Addresses()
        status = RelayStatus(updatedAt: Date(), launchedAt: launchedAt, listenerState: snapshot.listenerState,
                             port: snapshot.boundPort ?? 0, requestedPort: RelayConfig.port, wifiIPv4: ifs["en0"] ?? [], allIPv4: ifs,
                             cellularAvailable: cellularAvailable,
                             activeConnections: snapshot.activeConnections, totalConnections: snapshot.totalConnections,
                             failedConnections: snapshot.failedConnections, authFailures: snapshot.authFailures,
                             bytesUp: snapshot.bytesUp, bytesDown: snapshot.bytesDown,
                             lastError: snapshot.lastError, lastErrorAt: snapshot.lastErrorAt, hotspot: hotspotState, background: keepAlive.state, backgroundEvents: keepAlive.events,
                             recentErrors: snapshot.recentErrors)
        tick += 1
        if tick % 4 == 1 { writeStatus() }
    }

    private func writeStatus() {
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        enc.dateEncodingStrategy = .iso8601
        if let data = try? enc.encode(status) {
            try? data.write(to: Self.statusURL, options: .atomic)
            DemoReport.send("status.json", data)
        }
    }

    /// IPv4 addresses per interface name (en0 is Wi-Fi on iPhone, pdp_ip0 is cellular).
    static func ipv4Addresses() -> [String: [String]] {
        var result: [String: [String]] = [:]
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0, let first = head else { return result }
        defer { freeifaddrs(head) }
        for ptr in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let ifa = ptr.pointee
            guard let addr = ifa.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET),
                  (ifa.ifa_flags & UInt32(IFF_UP)) != 0 else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            if getnameinfo(addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
                let name = String(cString: ifa.ifa_name)
                result[name, default: []].append(String(cString: host))
            }
        }
        return result
    }
}
