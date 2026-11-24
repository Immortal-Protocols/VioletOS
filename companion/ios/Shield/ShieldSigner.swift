import CryptoKit
import Foundation
import Security

/// Builds and signs `VSH1` Shield envelopes, byte for byte like
/// tools/shield-trainer/vsh1.py; VioletOS verifies them with BearSSL
/// against the public key pinned into its image (services/sentinel/mod.rs).
///
/// On an iPhone the key lives in the Secure Enclave and never leaves it. The iOS
/// Simulator has no Secure Enclave, so there the Mac provisions a software
/// P-256 key (`-shieldDevKey <base64 PKCS#8 or PEM>`), the same one whose
/// public half `image/packages/build.py` pins into the QEMU image.
final class ShieldSigner {
    enum Op: UInt8 { case install = 1, revoke = 2, unfreeze = 3, kill = 4, freeze = 5 }
    enum Matcher: UInt8 { case path = 1, host = 2, writeExec = 3, binaryHash = 4 }
    enum Verdict: UInt8 { case log = 1, deny = 2, freeze = 3, kill = 4 }

    private enum Key {
        case enclave(SecureEnclave.P256.Signing.PrivateKey)
        case software(P256.Signing.PrivateKey)
    }

    private let key: Key?
    let keyDescription: String
    private var lastSeq: UInt64 = 0
    private var nextShieldId: UInt32 = UInt32(Date().timeIntervalSince1970) % 100_000 * 10

    init() {
        if let provisioned = Self.provisionedKey() {
            key = .software(provisioned)
            keyDescription = (ProcessInfo.processInfo.isiOSAppOnMac ? "software P-256 (development key) " : "software P-256 (Simulator dev key) ") + Self.fingerprint(provisioned.publicKey.x963Representation)
        } else if SecureEnclave.isAvailable, let k = Self.enclaveKey() {
            key = .enclave(k)
            keyDescription = "Secure Enclave P-256 " + Self.fingerprint(k.publicKey.x963Representation)
        } else {
            key = nil
            keyDescription = "no signing key"
        }
    }

    var canSign: Bool { key != nil }

    private static func fingerprint(_ point: Data) -> String {
        SHA256.hash(data: point).prefix(4).map { String(format: "%02x", $0) }.joined()
    }

    private static func provisionedKey() -> P256.Signing.PrivateKey? {
        let d = UserDefaults.standard
        guard let raw = d.volatileDomain(forName: UserDefaults.argumentDomain)["shieldDevKey"] as? String ?? d.string(forKey: "shieldDevKey") else { return nil }
        d.set(raw, forKey: "shieldDevKey")
        if raw.contains("BEGIN") { return try? P256.Signing.PrivateKey(pemRepresentation: raw) }
        guard let der = Data(base64Encoded: raw) else { return nil }
        return (try? P256.Signing.PrivateKey(derRepresentation: der)) ?? (try? P256.Signing.PrivateKey(rawRepresentation: der))
    }

    /// The Secure Enclave key, created on first use and kept in the keychain as an opaque blob.
    private static func enclaveKey() -> SecureEnclave.P256.Signing.PrivateKey? {
        let tag = "com.immortal.violet.shield.p256"
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrAccount as String: tag, kSecReturnData as String: true]
        var item: CFTypeRef?
        if SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let data = item as? Data,
           let k = try? SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data) {
            return k
        }
        guard let k = try? SecureEnclave.P256.Signing.PrivateKey() else { return nil }
        let add: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrAccount as String: tag,
                                  kSecValueData as String: k.dataRepresentation,
                                  kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly]
        SecItemAdd(add as CFDictionary, nil)
        return k
    }

    func newShieldId() -> UInt32 {
        nextShieldId &+= 1
        return nextShieldId
    }

    /// A monotonic sequence number: milliseconds since 1970, never repeating.
    private func nextSeq() -> UInt64 {
        lastSeq = max(lastSeq + 1, UInt64(Date().timeIntervalSince1970 * 1000))
        return lastSeq
    }

    static func rulePayload(_ m: Matcher, _ v: Verdict, _ text: String, writeOnly: Bool = false, crossAppOnly: Bool = false) -> Data {
        let raw = Data(text.utf8.prefix(255))
        return Data([m.rawValue, v.rawValue, (writeOnly ? 1 : 0) | (crossAppOnly ? 2 : 0), UInt8(raw.count)]) + raw
    }

    /// Pushed as soon as the Sentinel links: writes into another app's code or the system freeze the writer
    /// inside that call, before the write lands. Renewed before the hour-long TTL runs out.
    static let baseline: [(Matcher, Verdict, String, Bool, Bool)] = [
        (.path, .freeze, "/apps/", true, true),
        (.path, .freeze, "/bin/", true, false),
        (.path, .freeze, "/lib/", true, false),
    ]

    func envelope(op: Op, shieldId: UInt32, ttlSecs: UInt32, pid: UInt32 = 0, payload: Data = Data()) throws -> (Data, UInt64) {
        guard let key else { throw NSError(domain: "Shield", code: 10, userInfo: [NSLocalizedDescriptionKey: "no signing key"]) }
        let seq = nextSeq()
        var head = Data("VSH1".utf8)
        head.appendLE(UInt16(1))
        head.append(1)  // tier: companion auto (TTL-bounded)
        head.append(op.rawValue)
        head.appendLE(shieldId)
        head.appendLE(seq)
        head.appendLE(ttlSecs)
        head.appendLE(pid)
        head.appendLE(UInt16(payload.count))
        // The signed header states the signature's length, and DER signatures
        // are 70-72 bytes: sign until the two agree.
        var guess = 71
        for _ in 0..<16 {
            var signed = head
            signed.appendLE(UInt16(guess))
            signed.append(payload)
            let sig: Data
            switch key {
            case .enclave(let k): sig = try k.signature(for: signed).derRepresentation
            case .software(let k): sig = try k.signature(for: signed).derRepresentation
            }
            if sig.count == guess { return (signed + sig, seq) }
            guess = sig.count
        }
        throw NSError(domain: "Shield", code: 11, userInfo: [NSLocalizedDescriptionKey: "signature length never settled"])
    }
}

extension Data {
    mutating func appendLE<T: FixedWidthInteger>(_ v: T) {
        var le = v.littleEndian
        Swift.withUnsafeBytes(of: &le) { append(contentsOf: $0) }
    }
}
