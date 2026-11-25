// The Mac's identity and the phones it is paired with, kept in the login
// Keychain so a restart (or a move to another network) needs no new scan.
import CryptoKit
import Foundation
import Security

struct PairedPhone: Codable, Equatable {
    var key: String      // the phone's Ed25519 public key, hex
    var name: String
    var relay: String
}

struct Saved: Codable {
    var seed: String     // the Mac's Ed25519 seed, hex
    var phones: [PairedPhone]
    var name: String?    // what the user calls the agent; "Violet" when unset
    var voice: String?   // the RoseOS (Phonon) voice
}

enum Store {
    private static let service = "com.immortal.violet.agent"
    /// VIOLET_AGENT_PROFILE keeps a separate identity and pairing (the QEMU test uses "test").
    private static var account: String {
        let p = ProcessInfo.processInfo.environment["VIOLET_AGENT_PROFILE"] ?? ""
        return p.isEmpty ? "identity" : "identity-\(p)"
    }

    static func load() -> Saved {
        let q: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
                                kSecAttrService as String: service,
                                kSecAttrAccount as String: account,
                                kSecReturnData as String: true]
        var out: AnyObject?
        let result = SecItemCopyMatching(q as CFDictionary, &out)
        if result == errSecSuccess, let d = out as? Data,
           let s = try? JSONDecoder().decode(Saved.self, from: d) {
            return s
        }
        let fresh = Saved(seed: Curve25519.Signing.PrivateKey().rawRepresentation.hex, phones: [])
        // A different build may need Keychain approval to read an existing
        // pairing. Preserve it if the lookup fails or its data cannot decode.
        if result == errSecItemNotFound { save(fresh) }
        return fresh
    }

    static func save(_ s: Saved) {
        guard let d = try? JSONEncoder().encode(s) else { return }
        let q: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
                                kSecAttrService as String: service,
                                kSecAttrAccount as String: account]
        let update: [String: Any] = [kSecValueData as String: d]
        if SecItemUpdate(q as CFDictionary, update as CFDictionary) == errSecItemNotFound {
            var add = q
            add[kSecValueData as String] = d
            add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            add[kSecAttrLabel as String] = "Violet Agent pairing keys"
            SecItemAdd(add as CFDictionary, nil)
        }
    }

    static func identity(_ s: Saved) -> Curve25519.Signing.PrivateKey {
        (Data(hex: s.seed)).flatMap { try? Curve25519.Signing.PrivateKey(rawRepresentation: $0) } ?? Curve25519.Signing.PrivateKey()
    }
}
