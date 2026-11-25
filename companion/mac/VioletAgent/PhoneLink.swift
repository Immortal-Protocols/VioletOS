// One phone, through the relay: the pairing handshake after a QR scan, then
// sessions in the pair's room whenever both are online (docs/agent-protocol.md).
import CryptoKit
import Foundation

final class PhoneLink {
    enum Event {
        case relay(Bool)
        case paired(PairedPhone)
        case online(Bool)
        case json([String: Any])
        case mic(Data)
        case note(String)
    }

    private let identity: Curve25519.Signing.PrivateKey
    private let onEvent: (Event) -> Void
    private var relay: RelayLink?
    private var room = Data()
    private var session: Session?
    private var pairing: Invitation?
    private(set) var phone: PairedPhone?

    var online: Bool { session != nil }

    init(identity: Curve25519.Signing.PrivateKey, onEvent: @escaping (Event) -> Void) {
        self.identity = identity
        self.onEvent = onEvent
    }

    private var macKey: Data { identity.publicKey.rawRepresentation }

    /// Meet a paired phone in its room.
    func connect(_ p: PairedPhone) {
        stop()
        phone = p
        pairing = nil
        guard let key = Data(hex: p.key) else { return }
        room = sessionRoom(phone: key, mac: macKey)
        open(p.relay)
    }

    /// Pair with the phone whose code was scanned.
    func pair(_ inv: Invitation) {
        stop()
        phone = nil
        pairing = inv
        room = pairingRoom(inv.secret)
        open(inv.relay)
    }

    private func open(_ given: String) {
        // VIOLET_AGENT_RELAY_MAP="FROM=TO" reaches a relay the phone names differently (tests).
        var relayURL = given
        if let map = ProcessInfo.processInfo.environment["VIOLET_AGENT_RELAY_MAP"], let eq = map.firstIndex(of: "=") {
            let from = String(map[..<eq]), to = String(map[map.index(after: eq)...])
            if relayURL.hasPrefix(from) { relayURL = to + relayURL.dropFirst(from.count) }
        }
        relay = RelayLink(relay: relayURL, room: room, role: "mac") { [weak self] e in self?.on(e) }
        if relay == nil { NSLog("Violet Agent: bad relay address \(relayURL)") }
    }

    func stop() {
        relay?.stop()
        relay = nil
        session = nil
        onEvent(.online(false))
    }


    private func on(_ e: RelayLink.Event) {
        switch e {
        case .connected:
            onEvent(.relay(true))
        case .peerUp:
            break // the phone leads the handshake; its HELLO follows
        case .peerDown:
            onEvent(.note("relay: the phone left"))
            session = nil
                onEvent(.online(false))
        case .lost(let why):
            onEvent(.note("relay: \(why)"))
            session = nil
                onEvent(.relay(false))
            onEvent(.online(false))
        case .message(let m):
            if m.first == Wire.hello { onHello(m) } else if let s = session, let p = s.open(m) { onRecord(p) } else { dropped += 1 }
        }
    }

    private func onHello(_ m: Data) {
        guard let h = Hello(m, room: room), h.role == Wire.phone else { return }
        if let inv = pairing {
            guard h.mode == Wire.pairing, h.ed == inv.phoneKey else {
                NSLog("Violet Agent: the phone in the pairing room is not the one in the code")
                return
            }
        } else {
            guard h.mode == Wire.session, let p = phone, h.ed == Data(hex: p.key) else { return }
        }
        // Answer every offer; the phone completes only with the answer to its latest.
        let o = Offer(identity: identity, role: Wire.mac, mode: pairing != nil ? Wire.pairing : Wire.session, room: room,
                      secret: pairing?.secret, answering: h.x)
        relay?.send(o.message)
        guard let s = Session(ours: o, theirs: h, role: Wire.mac, room: room, phone: h.ed, mac: macKey) else { return }
        if let inv = pairing {
            let p = PairedPhone(key: inv.phoneKey.hex, name: inv.name, relay: inv.relay)
            onEvent(.paired(p))
            // Both sides move to the pair's own room.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in self?.connect(p) }
            return
        }
        session = s
        onEvent(.online(true))
    }

    private var kinds: [UInt8: Int] = [:]
    private var kindsSince = Date()
    private var dropped = 0

    private func onRecord(_ p: Data) {
        guard let kind = p.first else { return }
        kinds[kind, default: 0] += 1
        if Date().timeIntervalSince(kindsSince) > 15 {
            onEvent(.note("records from the phone in 15 s: " + kinds.sorted { $0.key < $1.key }.map { String(format: "%02x×%d", $0.key, $0.value) }.joined(separator: " ") + ", undecryptable \(dropped)"))
            kinds = [:]
            dropped = 0
            kindsSince = Date()
        }
        let body = p.dropFirst()
        switch kind {
        case 0x10:
            if let o = try? JSONSerialization.jsonObject(with: body) as? [String: Any] { onEvent(.json(o)) }
        case 0x20:
            onEvent(.mic(Data(body)))
        case 0x22:
            // µ-law (G.711) at 16 kHz.
            var out = Data(capacity: body.count * 2)
            for u in body {
                let x = ~u
                let sign = x & 0x80, exp = Int((x >> 4) & 7), mant = Int(x & 0x0f)
                var v = ((mant << 3) + 0x84) << exp
                v -= 0x84
                let s = Int16(sign != 0 ? -v : v)
                withUnsafeBytes(of: s.littleEndian) { out.append(contentsOf: $0) }
            }
            onEvent(.mic(out))
        case 0x23:
            // Silence the phone did not send: this many samples.
            let n = body.count >= 4 ? Int(body[body.startIndex]) | Int(body[body.startIndex + 1]) << 8 | Int(body[body.startIndex + 2]) << 16 | Int(body[body.startIndex + 3]) << 24 : 0
            if n > 0 && n <= 160000 { onEvent(.mic(Data(count: n * 2))) }
        default:
            break
        }
    }

    func send(kind: UInt8, _ body: Data) {
        guard let s = session else { return }
        var plain = Data([kind])
        plain.append(body)
        relay?.send(s.seal(plain))
    }

    func send(_ json: [String: Any]) {
        guard let d = try? JSONSerialization.data(withJSONObject: json) else { return }
        send(kind: 0x10, d)
    }
}
