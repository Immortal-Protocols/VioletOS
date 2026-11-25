// The direct path to the phone (docs/agent-protocol.md, "Direct path"),
// mirroring apps/open/agent/src/direct.rs: one UDP socket, its public address
// from STUN, probes to the phone's candidates (hole punching), then records
// in fragments delivered in order with acknowledgements. Every datagram
// carries an HMAC under the key the phone sent through the encrypted session.
// Everything runs on the main queue.
import CryptoKit
import Darwin
import Foundation

enum DirectWire {
    static let probe: UInt8 = 1
    static let probeAck: UInt8 = 2
    static let data: UInt8 = 3
    static let ack: UInt8 = 4
    static let ping: UInt8 = 5
    static let header = 11
    static let tag = 16
    static let fragment = 1200
    static let window: UInt32 = 12

    static func packet(_ key: SymmetricKey, _ kind: UInt8, seq: UInt32, ack: UInt32, flags: UInt8 = 0, _ payload: Data = Data()) -> Data {
        var p = Data([UInt8(ascii: "V"), kind])
        withUnsafeBytes(of: seq.bigEndian) { p.append(contentsOf: $0) }
        withUnsafeBytes(of: ack.bigEndian) { p.append(contentsOf: $0) }
        p.append(flags)
        p.append(payload)
        p.append(Data(HMAC<SHA256>.authenticationCode(for: p, using: key)).prefix(tag))
        return p
    }

    struct Packet {
        let kind: UInt8
        let seq: UInt32
        let ack: UInt32
        let flags: UInt8
        let payload: Data
    }

    static func parse(_ key: SymmetricKey, _ d: Data) -> Packet? {
        let b = [UInt8](d)
        guard b.count >= header + tag, b[0] == UInt8(ascii: "V") else { return nil }
        let body = Data(b[0..<(b.count - tag)])
        let want = Data(HMAC<SHA256>.authenticationCode(for: body, using: key)).prefix(tag)
        guard zip(want, b[(b.count - tag)...]).reduce(0, { $0 | ($1.0 ^ $1.1) }) == 0 else { return nil }
        func u32(_ at: Int) -> UInt32 { b[at..<at + 4].reduce(0) { $0 << 8 | UInt32($1) } }
        return Packet(kind: b[1], seq: u32(2), ack: u32(6), flags: b[10], payload: Data(b[header..<(b.count - tag)]))
    }
}

/// Ordered, acknowledged delivery of records (direct.rs's Transport).
final class DirectTransport {
    private struct Fragment { let seq: UInt32; let last: Bool; let data: Data }
    let key: SymmetricKey
    private var next: UInt32 = 1
    private var acked: UInt32 = 1
    private var sent: UInt32 = 1
    private var queue: [Fragment] = []
    private var records: [(UInt32, Data)] = []
    private var timer = Date()
    var rto: TimeInterval = 0.3
    private var expect: UInt32 = 1
    private var partial = Data()
    private var ackDue = false

    init(key: SymmetricKey) { self.key = key }

    func queue(_ record: Data) {
        let bytes = [UInt8](record)
        var chunks: [Data] = stride(from: 0, to: bytes.count, by: DirectWire.fragment).map { Data(bytes[$0..<min($0 + DirectWire.fragment, bytes.count)]) }
        if chunks.isEmpty { chunks = [Data()] }
        for (k, c) in chunks.enumerated() {
            queue.append(Fragment(seq: next, last: k + 1 == chunks.count, data: c))
            next += 1
        }
        records.append((next - 1, record))
    }

    func unsent() -> [Data] {
        queue = []
        defer { records = [] }
        return records.map { $0.1 }
    }

    private func onAck(_ ack: UInt32) {
        guard ack > acked, ack <= sent else { return }
        acked = ack
        timer = Date()
        queue.removeAll { $0.seq < ack }
        records.removeAll { $0.0 < ack }
    }

    func receive(_ p: DirectWire.Packet) -> [Data] {
        onAck(p.ack)
        guard p.kind == DirectWire.data else { return [] }
        ackDue = true
        guard p.seq == expect else { return [] }
        expect += 1
        partial.append(p.payload)
        guard p.flags & 1 != 0 else { return [] }
        defer { partial = Data() }
        return [partial]
    }

    func outgoing(_ now: Date) -> [Data] {
        var out: [Data] = []
        if acked < sent && now.timeIntervalSince(timer) > rto {
            for f in queue.prefix(while: { $0.seq < sent }).prefix(4) {
                out.append(DirectWire.packet(key, DirectWire.data, seq: f.seq, ack: expect, flags: f.last ? 1 : 0, f.data))
            }
            timer = now
        }
        if acked == sent { timer = now }
        while sent < next && sent - acked < DirectWire.window {
            let f = queue[Int(sent - acked)]
            out.append(DirectWire.packet(key, DirectWire.data, seq: f.seq, ack: expect, flags: f.last ? 1 : 0, f.data))
            sent += 1
        }
        if ackDue {
            ackDue = false
            if out.isEmpty { out.append(DirectWire.packet(key, DirectWire.ack, seq: 0, ack: expect)) }
        }
        return out
    }

    func ping() -> Data { DirectWire.packet(key, DirectWire.ping, seq: 0, ack: expect) }
}

/// "a.b.c.d:port" <-> sockaddr_in.
func sockaddrIn(_ s: String) -> sockaddr_in? {
    guard let colon = s.lastIndex(of: ":"), let port = UInt16(s[s.index(after: colon)...]) else { return nil }
    var a = sockaddr_in()
    a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    a.sin_family = sa_family_t(AF_INET)
    a.sin_port = port.bigEndian
    guard inet_pton(AF_INET, String(s[..<colon]), &a.sin_addr) == 1 else { return nil }
    return a
}

func describe(_ a: sockaddr_in) -> String {
    var buf = [CChar](repeating: 0, count: 16)
    var addr = a.sin_addr
    inet_ntop(AF_INET, &addr, &buf, 16)
    return "\(String(cString: buf)):\(UInt16(bigEndian: a.sin_port))"
}

final class DirectPath {
    enum Event {
        case gathered([String])
        case open(String)
        case message(Data)
        /// Failed or never opened; the records not known delivered.
        case down(String, [Data])
    }

    static let stunServers = ["stun.l.google.com:19302", "stun.cloudflare.com:3478"]

    private let onEvent: (Event) -> Void
    private let transport: DirectTransport
    private var fd: Int32 = -1
    private var reader: DispatchSourceRead?
    private var ticker: Timer?
    private var stunIDs: [Data] = []
    private var mapped: [String] = []
    private var gathering = true
    private var peers: [sockaddr_in] = []
    private var path: sockaddr_in?
    private var punchUntil: Date?
    private var probed = Date.distantPast
    private var probes: [(UInt32, Date)] = []
    private var heard = Date()
    private var pinged = Date()
    private var closed = false

    /// Open a socket for the phone's key and candidates; `gathered` follows
    /// with ours (to send back), and probing starts at once.
    init?(key: Data, peers: [String], stun: [String], onEvent: @escaping (Event) -> Void) {
        self.onEvent = onEvent
        transport = DirectTransport(key: SymmetricKey(data: key))
        self.peers = peers.compactMap(sockaddrIn)
        fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else { return nil }
        var any = sockaddr_in()
        any.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        any.sin_family = sa_family_t(AF_INET)
        let bound = withUnsafePointer(to: &any) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        guard bound == 0 else { Darwin.close(fd); return nil }
        _ = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)
        let r = DispatchSource.makeReadSource(fileDescriptor: fd, queue: .main)
        r.setEventHandler { [weak self] in self?.readAll() }
        r.resume()
        reader = r
        for s in stun { sendStun(s) }
        punchUntil = Date().addingTimeInterval(12)
        ticker = Timer.scheduledTimer(withTimeInterval: 0.01, repeats: true) { [weak self] _ in self?.tick() }
        // Report candidates once the STUN servers had time to answer.
        DispatchQueue.main.asyncAfter(deadline: .now() + (stun.isEmpty ? 0 : 1.2)) { [weak self] in self?.finishGathering() }
    }

    var port: UInt16 {
        var a = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) } }
        return UInt16(bigEndian: a.sin_port)
    }

    /// This Mac's IPv4 addresses on its networks, with the socket's port.
    private func localCandidates() -> [String] {
        var out: [String] = []
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0 else { return [] }
        defer { freeifaddrs(list) }
        var p = list
        while let i = p {
            defer { p = i.pointee.ifa_next }
            let flags = Int32(i.pointee.ifa_flags)
            guard let sa = i.pointee.ifa_addr, sa.pointee.sa_family == sa_family_t(AF_INET),
                  flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0 else { continue }
            var a = sa.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee }
            a.sin_port = port.bigEndian
            let s = describe(a)
            if !s.hasPrefix("169.254.") && !out.contains(s) { out.append(s) }
        }
        return out
    }

    private func finishGathering() {
        guard gathering, !closed else { return }
        gathering = false
        let cands = localCandidates() + mapped.filter { !localCandidates().contains($0) }
        onEvent(.gathered(cands))
    }

    private func sendStun(_ server: String) {
        guard let colon = server.lastIndex(of: ":") else { return }
        var hints = addrinfo()
        hints.ai_family = AF_INET
        hints.ai_socktype = SOCK_DGRAM
        var res: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(String(server[..<colon]), String(server[server.index(after: colon)...]), &hints, &res) == 0, let ai = res else { return }
        defer { freeaddrinfo(res) }
        let id = Data((0..<12).map { _ in UInt8.random(in: 0...255) })
        stunIDs.append(id)
        var req = Data([0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42])
        req.append(id)
        _ = req.withUnsafeBytes { sendto(fd, $0.baseAddress, req.count, 0, ai.pointee.ai_addr, ai.pointee.ai_addrlen) }
    }

    private func stunMapped(_ b: [UInt8]) -> String? {
        guard b.count >= 20, b[0] == 1, b[1] == 1, b[4...7].elementsEqual([0x21, 0x12, 0xa4, 0x42]),
              stunIDs.contains(Data(b[8..<20])) else { return nil }
        let end = min(20 + (Int(b[2]) << 8 | Int(b[3])), b.count)
        var at = 20
        var plain: String?
        while at + 4 <= end {
            let kind = Int(b[at]) << 8 | Int(b[at + 1]), len = Int(b[at + 2]) << 8 | Int(b[at + 3])
            guard at + 4 + len <= b.count else { break }
            let v = Array(b[(at + 4)..<(at + 4 + len)])
            if len >= 8 && v[1] == 1 {
                let port = UInt16(v[2]) << 8 | UInt16(v[3])
                let ip = v[4...7].reduce(UInt32(0)) { $0 << 8 | UInt32($1) }
                func text(_ ip: UInt32, _ port: UInt16) -> String { "\(ip >> 24).\(ip >> 16 & 255).\(ip >> 8 & 255).\(ip & 255):\(port)" }
                if kind == 0x0020 { return text(ip ^ 0x2112_a442, port ^ 0x2112) }
                if kind == 0x0001 { plain = text(ip, port) }
            }
            at += 4 + (len + 3) / 4 * 4
        }
        return plain
    }

    private func send(_ d: Data, to a: sockaddr_in) {
        var to = a
        _ = d.withUnsafeBytes { buf in
            withUnsafePointer(to: &to) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sendto(fd, buf.baseAddress, d.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        }
    }

    private func readAll() {
        var buf = [UInt8](repeating: 0, count: 2048)
        while !closed {
            var from = sockaddr_in()
            var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            let n = withUnsafeMutablePointer(to: &from) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(fd, &buf, buf.count, 0, $0, &len) } }
            // All read: acks and data the datagrams call for go out at once.
            guard n > 0 else { return tick() }
            let b = Array(buf[0..<n])
            if b[0] < 2, let m = stunMapped(b) {
                if !mapped.contains(m) { mapped.append(m) }
                continue
            }
            guard let p = DirectWire.parse(transport.key, Data(b)) else { continue }
            heard = Date()
            switch p.kind {
            case DirectWire.probe:
                send(DirectWire.packet(transport.key, DirectWire.probeAck, seq: p.seq, ack: 0), to: from)
            case DirectWire.probeAck:
                guard path == nil, let sent = probes.first(where: { $0.0 == p.seq })?.1 else { continue }
                let rtt = Date().timeIntervalSince(sent)
                transport.rto = min(max(rtt * 3, 0.15), 1.0)
                open(from, rtt)
            default:
                if path == nil && p.kind == DirectWire.data { open(from, nil) }
                if path != nil { path = from } // follow the phone to a new address
                for r in transport.receive(p) { onEvent(.message(r)) }
            }
        }
    }

    private func open(_ a: sockaddr_in, _ rtt: TimeInterval?) {
        path = a
        NSLog("Violet Agent: direct path open to \(describe(a))\(rtt.map { String(format: ", %.0f ms", $0 * 1000) } ?? "")")
        onEvent(.open(describe(a)))
    }

    private func tick() {
        guard !closed else { return }
        let now = Date()
        if let to = path {
            for d in transport.outgoing(now) {
                send(d, to: to)
                pinged = now
            }
            if now.timeIntervalSince(pinged) > 1.5 {
                send(transport.ping(), to: to)
                pinged = now
            }
            if now.timeIntervalSince(heard) > 8 { fail("nothing from the phone for 8 s") }
        } else if let until = punchUntil {
            if now > until { return fail("no answer to the probes (a firewall or a strict NAT)") }
            if now.timeIntervalSince(probed) > 0.2 && !peers.isEmpty {
                probed = now
                let n = UInt32.random(in: 0...UInt32.max)
                probes.append((n, now))
                if probes.count > 64 { probes.removeFirst() }
                for p in peers { send(DirectWire.packet(transport.key, DirectWire.probe, seq: n, ack: 0), to: p) }
            }
        }
    }

    var isOpen: Bool { path != nil && !closed }

    func send(_ record: Data) {
        transport.queue(record)
        if let to = path, !closed {
            for d in transport.outgoing(Date()) { send(d, to: to) }
        }
    }

    private func fail(_ why: String) {
        guard !closed else { return }
        let left = transport.unsent()
        stop()
        onEvent(.down(why, left))
    }

    /// Close the socket; returns the records not known delivered.
    @discardableResult
    func stop() -> [Data] {
        guard !closed else { return [] }
        closed = true
        ticker?.invalidate()
        reader?.cancel()
        Darwin.close(fd)
        return transport.unsent()
    }
}
