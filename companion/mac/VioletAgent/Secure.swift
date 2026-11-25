// The end-to-end layer of docs/agent-protocol.md on the Mac: the HELLO
// handshake and ChaCha20-Poly1305 records. It mirrors
// apps/open/agent/src/secure.rs on the phone.
import CryptoKit
import Foundation

enum Wire {
    static let hello: UInt8 = 0x01
    static let record: UInt8 = 0x02
    static let phone: UInt8 = UInt8(ascii: "p")
    static let mac: UInt8 = UInt8(ascii: "m")
    static let session: UInt8 = 0
    static let pairing: UInt8 = 1
}

func sha256(_ parts: Data...) -> Data {
    var h = SHA256()
    for p in parts { h.update(data: p) }
    return Data(h.finalize())
}

func pairingRoom(_ secret: Data) -> Data {
    sha256(Data("violet-agent pair".utf8), secret).prefix(16)
}

func sessionRoom(phone: Data, mac: Data) -> Data {
    sha256(Data("violet-agent room".utf8), phone, mac).prefix(16)
}

extension Data {
    var hex: String { map { String(format: "%02x", $0) }.joined() }
    init?(hex: String) {
        guard hex.count % 2 == 0 else { return nil }
        var d = Data()
        var i = hex.startIndex
        while i < hex.endIndex {
            let j = hex.index(i, offsetBy: 2)
            guard let b = UInt8(hex[i..<j], radix: 16) else { return nil }
            d.append(b)
            i = j
        }
        self = d
    }
    init?(base64url s: String) {
        var t = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while t.count % 4 != 0 { t += "=" }
        guard let d = Data(base64Encoded: t) else { return nil }
        self = d
    }
}

private func signed(role: UInt8, mode: UInt8, room: Data, ed: Data, x: Data, answer: Data) -> Data {
    var m = Data("violet-agent hello v1".utf8)
    m.append(contentsOf: [role, mode])
    m.append(room)
    m.append(ed)
    m.append(x)
    m.append(answer)
    return m
}

private func pairProof(secret: Data, ed: Data, x: Data) -> Data {
    var m = Data("violet-agent pair".utf8)
    m.append(ed)
    m.append(x)
    return Data(HMAC<SHA256>.authenticationCode(for: m, using: SymmetricKey(data: secret)))
}

struct Hello {
    let role: UInt8
    let mode: UInt8
    let ed: Data
    let x: Data
    let answer: Data
    let proof: Data?

    init?(_ m: Data, room: Data) {
        let b = [UInt8](m)
        guard b.count >= 163, b[0] == Wire.hello else { return nil }
        role = b[1]
        mode = b[2]
        ed = Data(b[3..<35])
        x = Data(b[35..<67])
        let sig = Data(b[67..<131])
        answer = Data(b[131..<163])
        guard let key = try? Curve25519.Signing.PublicKey(rawRepresentation: ed),
              key.isValidSignature(sig, for: signed(role: role, mode: mode, room: room, ed: ed, x: x, answer: answer)) else { return nil }
        proof = b.count >= 195 ? Data(b[163..<195]) : nil
    }
}

struct Offer {
    let key: Curve25519.KeyAgreement.PrivateKey
    let message: Data
    var x: Data { key.publicKey.rawRepresentation }

    /// The Mac's HELLO: an answer to the phone's offer `to` (its X25519 key).
    init(identity: Curve25519.Signing.PrivateKey, role: UInt8, mode: UInt8, room: Data, secret: Data? = nil, answering to: Data) {
        key = Curve25519.KeyAgreement.PrivateKey()
        let ed = identity.publicKey.rawRepresentation
        let x = key.publicKey.rawRepresentation
        var m = Data([Wire.hello, role, mode])
        m.append(ed)
        m.append(x)
        m.append((try? identity.signature(for: signed(role: role, mode: mode, room: room, ed: ed, x: x, answer: to))) ?? Data(count: 64))
        m.append(to)
        if let s = secret { m.append(pairProof(secret: s, ed: ed, x: x)) }
        message = m
    }
}

final class Session {
    private let send: SymmetricKey
    private let recv: SymmetricKey
    private var sent: UInt64 = 0
    /// The highest counter received, and which of the 64 below it arrived:
    /// records may cross when the path changes (relay and direct).
    private var received: UInt64 = 0
    private var seen: UInt64 = 0

    /// Finish a handshake; `phone` and `mac` are the two Ed25519 keys.
    init?(ours: Offer, theirs: Hello, role: UInt8, room: Data, phone: Data, mac: Data) {
        guard let peer = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: theirs.x),
              let shared = try? ours.key.sharedSecretFromKeyAgreement(with: peer) else { return nil }
        let (px, mx) = role == Wire.phone ? (ours.x, theirs.x) : (theirs.x, ours.x)
        var info = Data("violet-agent v1".utf8)
        info.append(phone); info.append(mac); info.append(px); info.append(mx)
        let okm = shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: room, sharedInfo: info, outputByteCount: 64)
        let bytes = okm.withUnsafeBytes { Data($0) }
        let toMac = SymmetricKey(data: bytes.prefix(32))
        let toPhone = SymmetricKey(data: bytes.suffix(32))
        (send, recv) = role == Wire.phone ? (toMac, toPhone) : (toPhone, toMac)
    }

    private static func nonce(_ c: UInt64) -> ChaChaPoly.Nonce {
        var n = Data(count: 4)
        withUnsafeBytes(of: c.bigEndian) { n.append(contentsOf: $0) }
        return try! ChaChaPoly.Nonce(data: n)
    }

    func seal(_ plain: Data) -> Data {
        sent += 1
        let box = try! ChaChaPoly.seal(plain, using: send, nonce: Session.nonce(sent))
        var out = Data([Wire.record])
        withUnsafeBytes(of: sent.bigEndian) { out.append(contentsOf: $0) }
        out.append(box.ciphertext)
        out.append(box.tag)
        return out
    }

    func open(_ m: Data) -> Data? {
        let b = [UInt8](m)
        guard b.count >= 25, b[0] == Wire.record else { return nil }
        let counter = b[1..<9].reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
        if counter <= received {
            let back = received - counter
            guard counter > 0, back > 0, back < 64, seen & (1 << (back - 1)) == 0 else { return nil }
        }
        let body = Data(b[9...])
        guard let box = try? ChaChaPoly.SealedBox(nonce: Session.nonce(counter), ciphertext: body.dropLast(16), tag: body.suffix(16)),
              let plain = try? ChaChaPoly.open(box, using: recv) else { return nil }
        if counter > received {
            let shift = counter - received
            seen = shift >= 64 ? 0 : (seen << shift) | (received > 0 ? 1 << (shift - 1) : 0)
            received = counter
        } else {
            seen |= 1 << (received - counter - 1)
        }
        return plain
    }
}

func proofOK(secret: Data, hello: Hello) -> Bool {
    guard let p = hello.proof else { return false }
    var m = Data("violet-agent pair".utf8)
    m.append(hello.ed)
    m.append(hello.x)
    return HMAC<SHA256>.isValidAuthenticationCode(p, authenticating: m, using: SymmetricKey(data: secret))
}

/// What a pairing code says: a phone's (`k` is the phone's key), or one this
/// Mac shows (`o=m`, `k` is the Mac's key) for the phone's camera.
struct Invitation {
    let relay: String
    let phoneKey: Data
    let secret: Data
    let name: String
    var fromMac = false

    /// The code this Mac shows: roles reversed, the phone proves it read `secret`.
    static func macCode(relay: String, macKey: Data, secret: Data, name: String) -> String {
        func b64(_ d: Data) -> String {
            d.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        }
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        func q(_ s: String) -> String { s.addingPercentEncoding(withAllowedCharacters: allowed) ?? s }
        return "violet-agent:1?r=\(q(relay))&k=\(b64(macKey))&s=\(b64(secret))&n=\(q(name))&o=m"
    }

    init?(_ text: String) {
        guard text.hasPrefix("violet-agent:1?"), let q = URLComponents(string: "x://y?" + text.dropFirst("violet-agent:1?".count))?.queryItems else { return nil }
        func item(_ k: String) -> String? { q.first { $0.name == k }?.value }
        guard let r = item("r"), let k = item("k").flatMap({ Data(base64url: $0) }), k.count == 32,
              let s = item("s").flatMap({ Data(base64url: $0) }), s.count == 32 else { return nil }
        relay = r
        phoneKey = k
        secret = s
        name = item("n") ?? "Phone"
        fromMac = item("o") == "m"
    }
}
