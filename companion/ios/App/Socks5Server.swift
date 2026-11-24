import Foundation
import Network
import os

let relayLog = Logger(subsystem: "com.immortal.violet.relay", category: "relay")

/// Counters shared by all sessions. Touched only on `RelayServer.queue`.
final class RelayCounters {
    var activeConnections = 0
    var totalConnections = 0
    var failedConnections = 0
    var authFailures = 0
    var bytesUp: UInt64 = 0     // client -> internet
    var bytesDown: UInt64 = 0   // internet -> client
    var lastError: String?
    var lastErrorAt: Date?
    var listenerState = "stopped"
    var boundPort: UInt16?
    var recentErrors: [String] = []

    func noteError(_ message: String) {
        lastError = message
        lastErrorAt = Date()
        recentErrors.append("\(ISO8601DateFormatter().string(from: Date())) \(message)")
        if recentErrors.count > 20 { recentErrors.removeFirst(recentErrors.count - 20) }
        relayLog.error("\(message, privacy: .public)")
    }
}

/// SOCKS5 server (RFC 1928 CONNECT + RFC 1929 username/password) that dials out over cellular only.
final class RelayServer {
    let queue = DispatchQueue(label: "com.immortal.violet.relay.net")
    let counters = RelayCounters()
    /// Port asked for (default 1080). On iOS 27 something in the OS already holds *:1080
    /// (bind gives EADDRINUSE, LAN connects are refused), so we walk up to `port + 9`.
    let port: UInt16
    private var tryPort: UInt16
    private var listener: NWListener?
    private var sessions: [ObjectIdentifier: Socks5Session] = [:]
    private var credentials: (user: String, pass: String)
    let anyUpstream = UserDefaults.standard.string(forKey: "upstream") == "any"

    init(port: UInt16, username: String, password: String) {
        self.port = port
        self.tryPort = port
        self.credentials = (username, password)
    }

    func start() {
        queue.async { self.startListener() }
    }

    private func startListener() {
        let params = NWParameters.tcp
        params.allowLocalEndpointReuse = true
        if let tcp = params.defaultProtocolStack.transportProtocol as? NWProtocolTCP.Options {
            tcp.noDelay = true
            tcp.enableKeepalive = true
            tcp.keepaliveIdle = 60
        }
        // Never accept over cellular: the client is on the local Wi-Fi.
        params.prohibitedInterfaceTypes = [.cellular]
        let listenPort = tryPort
        do {
            let l = try NWListener(using: params, on: NWEndpoint.Port(rawValue: listenPort)!)
            l.stateUpdateHandler = { [weak self] state in
                guard let self else { return }
                switch state {
                case .ready:
                    self.counters.listenerState = "listening"
                    self.counters.boundPort = l.port?.rawValue ?? listenPort
                    relayLog.info("listener ready on port \(listenPort)")
                case .waiting(let err):
                    self.counters.listenerState = "waiting"
                    self.counters.noteError("listener waiting on \(listenPort): \(err)")
                case .failed(let err):
                    self.counters.listenerState = "failed"
                    self.counters.boundPort = nil
                    l.cancel()
                    if case .posix(.EADDRINUSE) = err, listenPort < self.port &+ 9 {
                        self.tryPort = listenPort + 1
                        self.counters.noteError("port \(listenPort) in use; trying \(self.tryPort)")
                        self.startListener()
                    } else {
                        self.counters.noteError("listener on \(listenPort) failed: \(err); restarting in 2s")
                        if case .posix(.EADDRINUSE) = err { self.tryPort = self.port }
                        self.queue.asyncAfter(deadline: .now() + 2) { self.startListener() }
                    }
                case .cancelled:
                    if self.counters.listenerState != "failed" { self.counters.listenerState = "cancelled" }
                default:
                    break
                }
            }
            // Advertising over Bonjour lets VioletOS discover the relay and triggers the Local Network prompt early.
            l.service = NWListener.Service(name: "Violet Relay", type: "_violetrelay._tcp")
            l.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
            l.start(queue: queue)
            listener = l
        } catch {
            counters.listenerState = "failed"
            counters.noteError("cannot create listener: \(error)")
        }
    }

    private func accept(_ conn: NWConnection) {
        let session = Socks5Session(client: conn, server: self)
        let id = ObjectIdentifier(session)
        sessions[id] = session
        counters.activeConnections += 1
        counters.totalConnections += 1
        session.onFinish = { [weak self] in
            guard let self else { return }
            if self.sessions.removeValue(forKey: id) != nil {
                self.counters.activeConnections -= 1
            }
        }
        session.start()
    }

    /// Violet Shield blocked these hosts: end their live sessions now (on `queue`).
    func abortSessions(toHosts hosts: Set<String>) {
        for s in sessions.values where hosts.contains(s.targetHost.lowercased()) {
            s.abort("blocked by Violet Shield")
        }
    }

    func checkCredentials(user: String, pass: String) -> Bool {
        user == credentials.user && pass == credentials.pass
    }

    /// Change the relay password (a new pairing). Existing sessions keep running.
    func setPassword(_ password: String) {
        queue.async { self.credentials.pass = password }
    }
}

/// One SOCKS5 client connection and its cellular upstream.
final class Socks5Session {
    private enum Reply: UInt8 {
        case succeeded = 0x00, generalFailure = 0x01, notAllowed = 0x02, networkUnreachable = 0x03,
             hostUnreachable = 0x04, connectionRefused = 0x05, ttlExpired = 0x06,
             commandNotSupported = 0x07, addressTypeNotSupported = 0x08
    }

    static let handshakeTimeout: TimeInterval = 15
    static let connectTimeout: TimeInterval = 20
    static let idleTimeout: TimeInterval = 600
    static let chunk = 64 * 1024

    private let client: NWConnection
    private var upstream: NWConnection?
    private unowned let server: RelayServer
    private var queue: DispatchQueue { server.queue }
    private var counters: RelayCounters { server.counters }
    private var finished = false
    private var lastActivity = Date()
    private var timer: DispatchSourceTimer?
    private var phase = "handshake"
    private var target = "?"
    private(set) var targetHost = ""
    private var flowId: Int?
    private var upDone = false, downDone = false
    var onFinish: (() -> Void)?

    init(client: NWConnection, server: RelayServer) {
        self.client = client
        self.server = server
    }

    func start() {
        client.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            if case .failed(let err) = state { self.finish("client failed: \(err)") }
            if case .cancelled = state { self.finish(nil) }
        }
        client.start(queue: queue)
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 1, repeating: 1)
        t.setEventHandler { [weak self] in self?.tick() }
        t.resume()
        timer = t
        readGreeting()
    }

    private func tick() {
        let idle = Date().timeIntervalSince(lastActivity)
        switch phase {
        case "handshake" where idle > Self.handshakeTimeout:
            finish("handshake timeout")
        case "connecting" where idle > Self.connectTimeout:
            fail(.ttlExpired, "connect to \(target) timed out")
        case "relay" where idle > Self.idleTimeout:
            finish(nil)  // quiet idle close
        default:
            break
        }
    }

    // MARK: - Handshake

    private func read(_ n: Int, _ then: @escaping (Data) -> Void) {
        client.receive(minimumIncompleteLength: n, maximumLength: n) { [weak self] data, _, isComplete, error in
            guard let self, !self.finished else { return }
            if let error { return self.finish("client read error: \(error)") }
            guard let data, data.count == n else { return self.finish(isComplete ? nil : "short read") }
            self.lastActivity = Date()
            then(data)
        }
    }

    private func write(_ bytes: [UInt8], then: (() -> Void)? = nil) {
        client.send(content: Data(bytes), completion: .contentProcessed { [weak self] error in
            guard let self, !self.finished else { return }
            if let error { return self.finish("client write error: \(error)") }
            then?()
        })
    }

    private func readGreeting() {
        read(2) { head in
            guard head[0] == 0x05 else { return self.finish("not SOCKS5 (version \(head[0]))") }
            let n = Int(head[1])
            guard n > 0 else { return self.write([0x05, 0xFF]) { self.finish("no auth methods") } }
            self.read(n) { methods in
                guard methods.contains(0x02) else {
                    self.counters.authFailures += 1
                    return self.write([0x05, 0xFF]) { self.finish("client does not offer username/password auth") }
                }
                self.write([0x05, 0x02]) { self.readAuth() }
            }
        }
    }

    private func readAuth() {
        read(2) { head in
            guard head[0] == 0x01 else { return self.finish("bad auth version \(head[0])") }
            self.read(Int(head[1])) { user in
                self.read(1) { plen in
                    let passLen = Int(plen[0])
                    let check: (Data) -> Void = { pass in
                        let u = String(decoding: user, as: UTF8.self)
                        let p = String(decoding: pass, as: UTF8.self)
                        if self.server.checkCredentials(user: u, pass: p) {
                            self.write([0x01, 0x00]) { self.readRequest() }
                        } else {
                            self.counters.authFailures += 1
                            self.write([0x01, 0x01]) { self.finish("auth rejected for user '\(u)'") }
                        }
                    }
                    if passLen == 0 { check(Data()) } else { self.read(passLen, check) }
                }
            }
        }
    }

    private func readRequest() {
        read(4) { head in
            guard head[0] == 0x05 else { return self.finish("bad request version") }
            let cmd = head[1], atyp = head[3]
            let withHost: (NWEndpoint.Host) -> Void = { host in
                self.read(2) { p in
                    let port = UInt16(p[0]) << 8 | UInt16(p[1])
                    guard cmd == 0x01 else { return self.fail(.commandNotSupported, "command \(cmd) not supported") }
                    if case .name("time.violet", _) = host { return self.sendTime() }
                    if case .name("sentinel.violet", _) = host { return self.handOffToSentinel() }
                    self.connect(host: host, port: port)
                }
            }
            switch atyp {
            case 0x01:
                self.read(4) { a in withHost(.ipv4(IPv4Address(a)!)) }
            case 0x04:
                self.read(16) { a in withHost(.ipv6(IPv6Address(a)!)) }
            case 0x03:
                self.read(1) { len in
                    guard len[0] > 0 else { return self.fail(.hostUnreachable, "empty domain") }
                    self.read(Int(len[0])) { name in withHost(.name(String(decoding: name, as: UTF8.self), nil)) }
                }
            default:
                self.fail(.addressTypeNotSupported, "address type \(atyp) not supported")
            }
        }
    }

    // MARK: - Upstream

    private func connect(host: NWEndpoint.Host, port: UInt16) {
        target = "\(host):\(port)"
        targetHost = Self.name(of: host)
        if Shield.shared.isBlocked(host: targetHost, port: port) {
            return fail(.notAllowed, "\(target) blocked by Violet Shield")
        }
        flowId = Shield.shared.flowStarted(host: targetHost, port: port)
        phase = "connecting"
        lastActivity = Date()
        // Demos only: send named test hosts to local stand-ins (-hostMap JSON): a host:port
        // (the Mac, from the Simulator) or standin:sink / standin:web on this phone's loopback.
        var host = host, port = port
        var local = false
        if let mapped = RelayConfig.hostMap[targetHost.lowercased()] {
            if let (h, p) = StandIns.shared.resolve(mapped) {
                host = .name(h, nil)
                port = p
                local = true
            } else {
                let parts = mapped.split(separator: ":")
                host = .name(String(parts[0]), nil)
                port = parts.count > 1 ? UInt16(parts[1]) ?? port : port
            }
        }
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.connectionTimeout = Int(Self.connectTimeout)
        tcp.enableKeepalive = true
        tcp.keepaliveIdle = 60
        let params = NWParameters(tls: nil, tcp: tcp)
        // Always leave over cellular, even if Wi-Fi has internet. `-upstream any` exists only to
        // measure the relay's own overhead (Wi-Fi in, Wi-Fi out) for comparison.
        if local {
            params.requiredInterfaceType = .loopback
        } else if !server.anyUpstream {
            params.requiredInterfaceType = .cellular
        }
        params.prohibitExpensivePaths = false
        params.prohibitConstrainedPaths = false
        let up = NWConnection(host: host, port: NWEndpoint.Port(rawValue: port)!, using: params)
        upstream = up
        up.stateUpdateHandler = { [weak self] state in
            guard let self, !self.finished else { return }
            switch state {
            case .ready:
                guard self.phase == "connecting" else { return }
                relayLog.info("connected \(self.target, privacy: .public) via \(up.currentPath?.availableInterfaces.first?.name ?? "?", privacy: .public)")
                self.phase = "relay"
                self.write(self.successReply(up)) {
                    self.pump(from: self.client, to: up, up: true)
                    self.pump(from: up, to: self.client, up: false)
                }
            case .waiting(let err):
                // .waiting means NWConnection will retry; give up now on definitive errors.
                if self.phase == "connecting", let code = Self.definitiveReply(err) {
                    self.fail(code, "connect \(self.target): \(err)")
                }
            case .failed(let err):
                if self.phase == "connecting" {
                    self.fail(Self.definitiveReply(err) ?? .hostUnreachable, "connect \(self.target): \(err)")
                } else {
                    self.finish("upstream \(self.target) failed: \(err)")
                }
            default:
                break
            }
        }
        up.start(queue: queue)
    }

    static func name(of host: NWEndpoint.Host) -> String {
        switch host {
        case .name(let n, _): return n
        case .ipv4(let a): return a.rawValue.map(String.init).joined(separator: ".")
        case .ipv6(let a): return "\(a)"
        @unknown default: return "\(host)"
        }
    }

    /// "sentinel.violet" is Violet Shield's endpoint on this phone: after the SOCKS
    /// reply the connection belongs to the Shield, not to an upstream.
    private func handOffToSentinel() {
        phase = "sentinel"
        let reply: [UInt8] = [0x05, Reply.succeeded.rawValue, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
        write(reply) {
            self.timer?.cancel()
            self.timer = nil
            self.finished = true
            self.client.stateUpdateHandler = nil
            Shield.shared.attachSentinel(self.client)
            let done = self.onFinish
            self.onFinish = nil
            done?()
        }
    }

    func abort(_ why: String) {
        finish(why)
    }

    private static func definitiveReply(_ err: NWError) -> Reply? {
        switch err {
        case .posix(let code):
            switch code {
            case .ECONNREFUSED: return .connectionRefused
            case .ENETUNREACH, .ENETDOWN: return .networkUnreachable
            case .EHOSTUNREACH, .EHOSTDOWN: return .hostUnreachable
            case .ETIMEDOUT: return .ttlExpired
            default: return nil
            }
        case .dns: return .hostUnreachable
        default: return nil
        }
    }

    /// VioletOS has no clock of its own: "time.violet" answers with the phone's Unix
    /// time as four big-endian bytes, then closes.
    private func sendTime() {
        phase = "time"
        let now = UInt32(Date().timeIntervalSince1970)
        let reply: [UInt8] = [0x05, Reply.succeeded.rawValue, 0x00, 0x01, 0, 0, 0, 0, 0, 0,
                              UInt8(now >> 24), UInt8(now >> 16 & 0xFF), UInt8(now >> 8 & 0xFF), UInt8(now & 0xFF)]
        write(reply) { self.finish(nil) }
    }

    private func successReply(_ up: NWConnection) -> [UInt8] {
        var reply: [UInt8] = [0x05, Reply.succeeded.rawValue, 0x00]
        if case .hostPort(let host, let port)? = up.currentPath?.localEndpoint {
            switch host {
            case .ipv4(let a): reply += [0x01] + [UInt8](a.rawValue)
            case .ipv6(let a): reply += [0x04] + [UInt8](a.rawValue)
            default: reply += [0x01, 0, 0, 0, 0]
            }
            reply += [UInt8(port.rawValue >> 8), UInt8(port.rawValue & 0xFF)]
        } else {
            reply += [0x01, 0, 0, 0, 0, 0, 0]
        }
        return reply
    }

    private func fail(_ code: Reply, _ message: String) {
        guard !finished else { return }
        counters.failedConnections += 1
        counters.noteError(message)
        phase = "failed"
        client.send(content: Data([0x05, code.rawValue, 0x00, 0x01, 0, 0, 0, 0, 0, 0]),
                    completion: .contentProcessed { [weak self] _ in self?.finish(nil) })
    }

    // MARK: - Relay

    /// Copy bytes `from` -> `to`; the next receive is issued only after the send completes (backpressure).
    private func pump(from src: NWConnection, to dst: NWConnection, up: Bool) {
        src.receive(minimumIncompleteLength: 1, maximumLength: Self.chunk) { [weak self] data, _, isComplete, error in
            guard let self, !self.finished else { return }
            if let data, !data.isEmpty {
                self.lastActivity = Date()
                if up { self.counters.bytesUp += UInt64(data.count) } else { self.counters.bytesDown += UInt64(data.count) }
                if let id = self.flowId { Shield.shared.flowBytes(id, up: up ? data.count : 0, down: up ? 0 : data.count) }
                dst.send(content: data, completion: .contentProcessed { [weak self] sendErr in
                    guard let self, !self.finished else { return }
                    if let sendErr { return self.finish("send error (\(up ? "up" : "down")) \(self.target): \(sendErr)") }
                    if isComplete { self.halfClose(dst, up: up) } else { self.pump(from: src, to: dst, up: up) }
                })
                return
            }
            if let error { return self.finish("receive error (\(up ? "up" : "down")) \(self.target): \(error)") }
            if isComplete { self.halfClose(dst, up: up) } else { self.pump(from: src, to: dst, up: up) }
        }
    }

    /// Forward a FIN; tear the pair down once both directions are finished.
    private func halfClose(_ dst: NWConnection, up: Bool) {
        dst.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .contentProcessed { _ in })
        if up { upDone = true } else { downDone = true }
        if upDone && downDone {
            queue.asyncAfter(deadline: .now() + 1) { [weak self] in self?.finish(nil) }
        }
    }

    private func finish(_ error: String?) {
        guard !finished else { return }
        finished = true
        if let error { counters.noteError(error) }
        if let id = flowId { Shield.shared.flowEnded(id) }
        timer?.cancel()
        timer = nil
        upstream?.cancel()
        client.cancel()
        upstream?.stateUpdateHandler = nil
        client.stateUpdateHandler = nil
        let done = onFinish
        onFinish = nil
        done?()
    }
}
