import CryptoKit
import Foundation
import Security

/// Builds and signs `VSH1` Shield envelopes, byte for byte like
/// tools/shield-trainer/vsh1.py; VioletOS verifies them with BearSSL
/// against the public key pinned into its image (services/sentinel/mod.rs).
///
/// The Mac companion normally loads the P-256 key whose public half
/// `image/packages/build.py` pins into the VioletOS image.
final class ShieldSigner {
    private enum Key {
        case enclave(SecureEnclave.P256.Signing.PrivateKey)
        case software(P256.Signing.PrivateKey)
    }

    private let key: Key?
    let keyDescription: String
    private var lastSeq: UInt64 = 0
    private var nextShieldId: UInt32 = UInt32(Date().timeIntervalSince1970) % 100_000 * 10

    init(emulatorKey: URL? = nil) {
        if let emulatorKey {
            if let pem = try? String(contentsOf: emulatorKey, encoding: .utf8),
               let signingKey = try? P256.Signing.PrivateKey(pemRepresentation: pem) {
                key = .software(signingKey)
                keyDescription = "VioletOS P-256 " + Self.fingerprint(signingKey.publicKey.x963Representation)
            } else {
                key = nil
                keyDescription = "VioletOS signing key unavailable"
            }
        } else if let provisioned = Self.provisionedKey() {
            key = .software(provisioned)
            keyDescription = "software P-256 (Simulator dev key) " + Self.fingerprint(provisioned.publicKey.x963Representation)
        } else if SecureEnclave.isAvailable, let k = Self.enclaveKey() {
            key = .enclave(k)
            keyDescription = "Secure Enclave P-256 " + Self.fingerprint(k.publicKey.x963Representation)
        } else {
            key = nil
            keyDescription = "no signing key"
        }
    }

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

    private func newShieldId() -> UInt32 {
        nextShieldId &+= 1
        return nextShieldId
    }

    /// A monotonic sequence number: milliseconds since 1970, never repeating.
    private func nextSeq() -> UInt64 {
        lastSeq = max(lastSeq + 1, UInt64(Date().timeIntervalSince1970 * 1000))
        return lastSeq
    }

    func warning() throws -> (Data, UInt64) {
        guard let key else { throw NSError(domain: "Shield", code: 10, userInfo: [NSLocalizedDescriptionKey: "no signing key"]) }
        let seq = nextSeq()
        var head = Data("VSH1".utf8)
        head.appendLE(UInt16(1))
        head.append(1)  // companion signing tier
        head.append(6)  // display a warning on Home
        head.appendLE(newShieldId())
        head.appendLE(seq)
        head.appendLE(UInt32(0))  // no TTL or process target
        head.appendLE(UInt32(0))
        head.appendLE(UInt16(0))  // no payload
        // The signed header states the signature's length, and DER signatures
        // are 70-72 bytes: sign until the two agree.
        var guess = 71
        for _ in 0..<16 {
            var signed = head
            signed.appendLE(UInt16(guess))
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
