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
    }

    private let identity: Curve25519.Signing.PrivateKey
    private let onEvent: (Event) -> Void
    private var relay: RelayLink?
    private var room = Data()
    private var offer: Offer?
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
        offer = nil
        onEvent(.online(false))
    }

    private func sendHello() {
        let mode = pairing != nil ? Wire.pairing : Wire.session
        let o = Offer(identity: identity, role: Wire.mac, mode: mode, room: room, secret: pairing?.secret)
        offer = o
        relay?.send(o.message)
    }

    private func on(_ e: RelayLink.Event) {
        switch e {
        case .connected:
            onEvent(.relay(true))
        case .peerUp:
            sendHello()
        case .peerDown:
            session = nil
            offer = nil
            onEvent(.online(false))
        case .lost(let why):
            NSLog("Violet Agent: relay: \(why)")
            session = nil
            offer = nil
            onEvent(.relay(false))
            onEvent(.online(false))
        case .message(let m):
            if m.first == Wire.hello { onHello(m) } else if let s = session, let p = s.open(m) { onRecord(p) }
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
        var ours = offer
        offer = nil
        if ours == nil {
            let o = Offer(identity: identity, role: Wire.mac, mode: pairing != nil ? Wire.pairing : Wire.session, room: room, secret: pairing?.secret)
            relay?.send(o.message)
            ours = o
        }
        guard let o = ours, let s = Session(ours: o, theirs: h, role: Wire.mac, room: room, phone: h.ed, mac: macKey) else { return }
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

    private func onRecord(_ p: Data) {
        guard let kind = p.first else { return }
        let body = p.dropFirst()
        switch kind {
        case 0x10:
            if let o = try? JSONSerialization.jsonObject(with: body) as? [String: Any] { onEvent(.json(o)) }
        case 0x20:
            onEvent(.mic(Data(body)))
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
