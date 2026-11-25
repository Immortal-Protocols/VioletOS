import AccessorySetupKit
import Foundation
import Network
import NetworkExtension
import UIKit

/// App configuration. Read from launch arguments (`-key value` lands in UserDefaults' argument domain)
/// and persisted so a later plain launch keeps the pairing secret.
enum RelayConfig {
    static let username = "violet"
    static let ssidPrefix = "VioletRelayOpen-"
    // Keep in sync with the Moto's relay clients in targets/moto-g-pure/ip.
    static let handsetPort: UInt16 = 1081
    static var port: UInt16 {
        let v = UserDefaults.standard.integer(forKey: "port")
        return (1...65535).contains(v) ? UInt16(v) : handsetPort
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

    static func ssid(forCode code: String) -> String { ssidPrefix + code.prefix(4) }

    static func isVioletNetwork(_ ssid: String) -> Bool {
        guard ssid.hasPrefix(ssidPrefix) else { return false }
        let suffix = ssid.dropFirst(ssidPrefix.count)
        return suffix.count == 4 && suffix.allSatisfy { "0123456789ABCDEF".contains($0) }
    }

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
    @Published private(set) var selectedNetwork: String?
    @Published private(set) var knownNetworks: [String] = []
    @Published private(set) var canFindNetworks = false
    private let server: RelayServer
    private let accessories = ASAccessorySession()
    private var pendingNetwork: String?
    private var pickerOpen = false
    private enum JoinPhase {
        case configuring
        case waiting(ContinuousClock.Instant)
        case needsSettings
        case failed
    }

    private var started = false
    private let clock = ContinuousClock()
    private var targetSSID = RelayConfig.pairingCode.map(RelayConfig.ssid(forCode:))
    private var joinPhase: JoinPhase?
    private var currentSSID: String?
    private var wifiIPv4: [String] = []
    private var wifiRevision = 0
    private var checkingSSID = false
    private var nextSSIDCheck: ContinuousClock.Instant?
    // Every VioletOS access point assigns the same address. When changing
    // targets, a lease already on en0 still belongs to the old connection.
    private var needsNewLease = false
    private let launchedAt = Date()
    private var hotspotMessage: String?
    private var cellularAvailable = false
    private let cellMonitor = NWPathMonitor(requiredInterfaceType: .cellular)
    private var tick = 0
    private let keepAlive = KeepAlive()

    var isJoining: Bool {
        if case .configuring = joinPhase { return true }
        return false
    }

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
        guard !started else { return }
        started = true
        accessories.activate(on: .main) { [weak self] event in
            self?.handleAccessoryEvent(event)
        }
        UIApplication.shared.isIdleTimerDisabled = true
        keepAlive.start()
        cellMonitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in self?.cellularAvailable = path.status == .satisfied }
        }
        cellMonitor.start(queue: .main)
        StandIns.shared.start(kinds: Set(RelayConfig.hostMap.values.filter { $0.hasPrefix("standin:") }.map { String($0.dropFirst(8)) }))
        server.start()
        if let ssid = RelayConfig.forgetSSID, !ssid.isEmpty {
            if ssid.hasSuffix("*") {
                // "VioletRelay*" removes every configuration this app saved with that prefix.
                let prefix = String(ssid.dropLast())
                NEHotspotConfigurationManager.shared.getConfiguredSSIDs { [weak self] ssids in
                    let matches = ssids.filter { $0.hasPrefix(prefix) }
                    matches.forEach { NEHotspotConfigurationManager.shared.removeConfiguration(forSSID: $0) }
                    Task { @MainActor in
                        self?.hotspotMessage = "removed \(matches.count) configurations matching \(ssid)"
                    }
                }
            } else {
                NEHotspotConfigurationManager.shared.removeConfiguration(forSSID: ssid)
                hotspotMessage = "removed configuration for \(ssid)"
            }
        }
        if let join = RelayConfig.joinRequest { joinNetwork(ssid: join.ssid, passphrase: join.passphrase) }
        Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
        refresh()
    }

    private func handleAccessoryEvent(_ event: ASAccessoryEvent) {
        switch event.eventType {
        case .activated:
            canFindNetworks = true
            refreshKnownNetworks()
        case .accessoryAdded:
            pendingNetwork = event.accessory?.ssid
            refreshKnownNetworks()
        case .accessoryChanged, .accessoryRemoved:
            refreshKnownNetworks()
        case .pickerDidDismiss:
            pickerOpen = false
            if let pendingNetwork { selectNetwork(pendingNetwork) }
            pendingNetwork = nil
        case .invalidated:
            canFindNetworks = false
            pickerOpen = false
            pendingNetwork = nil
        default:
            break
        }
    }

    private func refreshKnownNetworks() {
        knownNetworks = Set(accessories.accessories.compactMap(\.ssid).filter(RelayConfig.isVioletNetwork)).sorted()
    }

    func findNetworks() {
        guard canFindNetworks, !pickerOpen, let image = UIImage(systemName: "wifi") else { return }
        pickerOpen = true
        pendingNetwork = nil
        let descriptor = ASDiscoveryDescriptor()
        descriptor.ssidPrefix = RelayConfig.ssidPrefix
        let item = ASPickerDisplayItem(name: "VioletOS Phone Internet", productImage: image, descriptor: descriptor)
        accessories.showPicker(for: [item]) { [weak self] error in
            guard let error else { return }
            Task { @MainActor in
                guard let self else { return }
                self.pickerOpen = false
                guard (error as? ASError)?.code != .userCancelled else { return }
                self.hotspotMessage = "Couldn't find Violet networks. Check Phone Internet on VioletOS or join by code."
                self.refresh()
            }
        }
    }

    func selectNetwork(_ ssid: String) {
        guard RelayConfig.isVioletNetwork(ssid) else { return }
        selectedNetwork = ssid
        hotspotMessage = "Enter the pairing code shown on VioletOS"
        refresh()
    }

    /// Save a pairing code from VioletOS and join its network. Returns false for a bad code.
    @discardableResult
    func pair(code text: String) -> Bool {
        guard !isJoining else { return false }
        guard let code = RelayConfig.normalizedCode(text) else {
            hotspotMessage = "That code isn't 12 letters and numbers (0-9, A-F)"
            refresh()
            return false
        }
        let ssid = RelayConfig.ssid(forCode: code)
        if let selectedNetwork, selectedNetwork != ssid {
            hotspotMessage = "That code is for a different Violet network"
            refresh()
            return false
        }
        RelayConfig.pairingCode = code
        server.setPassword(String(code.suffix(8)))
        joinNetwork(ssid: ssid, passphrase: "")
        return true
    }

    /// Recheck immediately on returning from the system picker or Settings.
    func becameActive() {
        nextSSIDCheck = nil
        refresh()
    }

    private func observeSSID() {
        guard UIApplication.shared.applicationState == .active, targetSSID != nil, !checkingSSID,
              nextSSIDCheck.map({ clock.now >= $0 }) ?? true else { return }
        checkingSSID = true
        let revision = wifiRevision
        Task {
            let ssid = await NEHotspotNetwork.fetchCurrent()?.ssid
            checkingSSID = false
            if revision == wifiRevision {
                currentSSID = ssid
                nextSSIDCheck = clock.now.advanced(by: .seconds(2))
                refresh()
            }
        }
    }

    private var connectionMessage: String? {
        if let hotspotMessage { return hotspotMessage }
        guard let targetSSID else { return nil }
        let violet = RelayConfig.isVioletNetwork(targetSSID)
        let observed = RelayWiFiStatus.observed(targetSSID: targetSSID, currentSSID: currentSSID,
                                               wifiIPv4: needsNewLease ? [] : wifiIPv4, requiresVioletLease: violet)
        switch observed {
        case .connected:
            if !isJoining { joinPhase = nil }
            return "Connected to \(targetSSID)"
        case .awaitingAddress:
            return "Waiting for a Wi-Fi address from VioletOS…"
        case .waitingForNetwork:
            switch joinPhase {
            case .configuring:
                return "Joining \(targetSSID)…"
            case .waiting(let until) where clock.now < until:
                return "Connecting to \(targetSSID)…"
            case .needsSettings:
                return "iOS couldn’t join \(targetSSID). Try Reconnect or join it in iPhone Wi-Fi Settings."
            case .waiting, .failed:
                return "Still waiting for \(targetSSID). Check Phone Internet on VioletOS, then try Reconnect."
            case nil:
                return "Waiting for \(targetSSID). Tap Reconnect if needed."
            }
        }
    }

    /// Use the system accessory join for approved accessories. A code-only join
    /// can use iOS's persistent configuration when the network permits it.
    func joinNetwork(ssid: String, passphrase: String) {
        guard !isJoining else { return }
        let hasLease = Self.ipv4Addresses()["en0"]?.contains(RelayWiFiStatus.violetLease) == true
        if let targetSSID, targetSSID != ssid, hasLease { needsNewLease = true }
        targetSSID = ssid
        hotspotMessage = nil
        joinPhase = .configuring
        refresh()
        relayLog.info("joining \(ssid, privacy: .public)")
        Task {
            defer { refresh() }
            let currentSSID = await NEHotspotNetwork.fetchCurrent()?.ssid
            let hasLease = Self.ipv4Addresses()["en0"]?.contains(RelayWiFiStatus.violetLease) == true
            if currentSSID == ssid || (currentSSID == nil && hasLease && !needsNewLease && RelayConfig.isVioletNetwork(ssid)) {
                if currentSSID == ssid { needsNewLease = false }
                joinPhase = .waiting(clock.now.advanced(by: .seconds(15)))
                return
            }
            let manager = NEHotspotConfigurationManager.shared
            do {
                if passphrase.isEmpty, let accessory = accessories.accessories.first(where: { $0.ssid == ssid }) {
                    try await manager.joinAccessoryHotspotWithoutSecurity(accessory)
                } else {
                    let config = passphrase.isEmpty
                        ? NEHotspotConfiguration(ssid: ssid)
                        : NEHotspotConfiguration(ssid: ssid, passphrase: passphrase, isWEP: false)
                    config.joinOnce = false
                    try await manager.apply(config)
                }
                joinPhase = .waiting(clock.now.advanced(by: .seconds(15)))
            } catch {
                let error = error as NSError
                switch (error.domain, error.code) {
                case (NEHotspotConfigurationErrorDomain, NEHotspotConfigurationError.alreadyAssociated.rawValue):
                    needsNewLease = false
                    joinPhase = .waiting(clock.now.advanced(by: .seconds(15)))
                case (NEHotspotConfigurationErrorDomain, NEHotspotConfigurationError.systemDenied.rawValue),
                     (NEHotspotConfigurationErrorDomain, NEHotspotConfigurationError.userUnauthorized.rawValue):
                    joinPhase = .needsSettings
                default:
                    joinPhase = .failed
                }
                relayLog.info("hotspot join returned \(error.localizedDescription, privacy: .public) [\(error.code)]")
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
        let addresses = (ifs["en0"] ?? []).sorted()
        if addresses != wifiIPv4 {
            wifiIPv4 = addresses
            wifiRevision += 1
            currentSSID = nil
            nextSSIDCheck = nil
        }
        if !wifiIPv4.contains(RelayWiFiStatus.violetLease) || (targetSSID != nil && currentSSID == targetSSID) {
            needsNewLease = false
        }
        observeSSID()
        status = RelayStatus(updatedAt: Date(), launchedAt: launchedAt, listenerState: snapshot.listenerState,
                             port: snapshot.boundPort ?? 0, requestedPort: RelayConfig.port, wifiIPv4: wifiIPv4, allIPv4: ifs,
                             cellularAvailable: cellularAvailable,
                             activeConnections: snapshot.activeConnections, totalConnections: snapshot.totalConnections,
                             failedConnections: snapshot.failedConnections, authFailures: snapshot.authFailures,
                             bytesUp: snapshot.bytesUp, bytesDown: snapshot.bytesDown,
                             lastError: snapshot.lastError, lastErrorAt: snapshot.lastErrorAt, hotspot: connectionMessage, background: keepAlive.state, backgroundEvents: keepAlive.events,
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
