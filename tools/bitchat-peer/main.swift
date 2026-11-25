// A bitchat peer for the Mac, to test VioletOS's bitchat app over the air.
// It plays the central side the way the iOS app does: scans for the bitchat
// service, connects, subscribes, announces itself, says something in the
// public channel, and checks every announce and message it gets back with
// CryptoKit (the iOS app's own crypto).
//
//   make bitchat-peer
//   open -W -n build/BitchatPeer.app --stdout LOG --stderr LOG --args \
//       [--name VioletOS|any] [--scan] [--say TEXT] [--expect TEXT] [--seconds N]
//
// It is an app bundle because macOS gives Bluetooth only to apps that say why
// (Info.plist). It asks once and keeps the answer while the signing identity
// stays the same.
//
// Exits 0 once it has a verified announce from the phone, has posted, and
// (with --expect) has seen a verified message containing TEXT.
import CoreBluetooth
import CryptoKit
import Foundation

let service = CBUUID(string: "F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C")
let characteristicID = CBUUID(string: "A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D")

var args = CommandLine.arguments.dropFirst().makeIterator()
var say = "hello from the Mac"
var expect: String? = nil
var seconds = 60.0
var name: String? = "VioletOS"
var scanOnly = false
while let a = args.next() {
    switch a {
    case "--say": say = args.next() ?? say
    case "--expect": expect = args.next()
    case "--name": name = args.next(); if name == "any" { name = nil }
    case "--scan": scanOnly = true
    case "--seconds": seconds = Double(args.next() ?? "60") ?? 60
    default: print("unknown argument \(a)"); exit(2)
    }
}

setvbuf(stdout, nil, _IOLBF, 0)
let signing = Curve25519.Signing.PrivateKey()
let noise = Curve25519.KeyAgreement.PrivateKey()
let myID = Data(SHA256.hash(data: noise.publicKey.rawRepresentation).prefix(8))

func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }

func pad(_ d: Data) -> Data {
    guard let bucket = [256, 512, 1024, 2048].first(where: { $0 >= d.count + 16 }) else { return d }
    let n = bucket - d.count
    guard n > 0 && n <= 255 else { return d }
    return d + Data(repeating: UInt8(n), count: n)
}

struct Packet {
    var type: UInt8, ttl: UInt8, timestamp: UInt64, flags: UInt8
    var sender: Data, recipient: Data?, payload: Data, signature: Data?

    func encode(forSigning: Bool = false) -> Data {
        var f = flags & ~UInt8(0x0b)
        if recipient != nil { f |= 1 }
        if signature != nil && !forSigning { f |= 2 }
        var d = Data([1, type, forSigning ? 0 : ttl])
        withUnsafeBytes(of: timestamp.bigEndian) { d.append(contentsOf: $0) }
        d.append(f)
        withUnsafeBytes(of: UInt16(payload.count).bigEndian) { d.append(contentsOf: $0) }
        d.append(sender)
        if let r = recipient { d.append(r) }
        d.append(payload)
        if let s = signature, !forSigning { d.append(s) }
        return forSigning ? pad(d) : d
    }

    static func decode(_ d: Data) -> Packet? {
        let b = [UInt8](d)
        guard b.count >= 22, b[0] == 1 else { return nil }
        let ts = b[3..<11].reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
        let flags = b[11]
        let len = Int(b[12]) << 8 | Int(b[13])
        var at = 14
        func take(_ n: Int) -> Data? {
            guard at + n <= b.count else { return nil }
            defer { at += n }
            return Data(b[at..<at + n])
        }
        guard let sender = take(8) else { return nil }
        let recipient = flags & 1 != 0 ? take(8) : nil
        guard let payload = take(len) else { return nil }
        let sig = flags & 2 != 0 ? take(64) : nil
        if flags & 2 != 0 && sig == nil { return nil }
        return Packet(type: b[1], ttl: b[2], timestamp: ts, flags: flags & ~UInt8(0x0b), sender: sender, recipient: recipient, payload: payload, signature: sig)
    }

    mutating func sign() {
        signature = nil
        signature = try! signing.signature(for: encode(forSigning: true))
    }
    func verify(_ key: Data) -> Bool {
        guard let s = signature, let k = try? Curve25519.Signing.PublicKey(rawRepresentation: key) else { return false }
        return k.isValidSignature(s, for: encode(forSigning: true))
    }
}

func nowMS() -> UInt64 { UInt64(Date().timeIntervalSince1970 * 1000) }

func announce() -> Data {
    var tlv = Data()
    let nick = Data("macpeer".utf8)
    tlv.append(contentsOf: [1, UInt8(nick.count)]); tlv.append(nick)
    tlv.append(contentsOf: [2, 32]); tlv.append(noise.publicKey.rawRepresentation)
    tlv.append(contentsOf: [3, 32]); tlv.append(signing.publicKey.rawRepresentation)
    var p = Packet(type: 1, ttl: 7, timestamp: nowMS(), flags: 0, sender: myID, recipient: nil, payload: tlv, signature: nil)
    p.sign()
    return p.encode()
}
func message(_ text: String) -> Data {
    var p = Packet(type: 2, ttl: 7, timestamp: nowMS(), flags: 0, sender: myID, recipient: nil, payload: Data(text.utf8), signature: nil)
    p.sign()
    return p.encode()
}

final class Central: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    var manager: CBCentralManager!
    var peripheral: CBPeripheral?
    var characteristic: CBCharacteristic?
    var keys: [Data: (String, Data)] = [:]
    var announced = false, posted = false, heard = expect == nil

    override init() {
        super.init()
        manager = CBCentralManager(delegate: self, queue: nil)
    }
    func finishIfDone() {
        if announced && posted && heard {
            print("PASS: verified announce from the phone, posted\(expect == nil ? "" : ", and heard \"\(expect!)\"")")
            // Let CoreBluetooth send what is queued before the process ends.
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { exit(0) }
        }
    }
    func centralManagerDidUpdateState(_ c: CBCentralManager) {
        print("bluetooth state \(c.state.rawValue)")
        if c.state == .poweredOn {
            if scanOnly {
                c.scanForPeripherals(withServices: nil, options: [CBCentralManagerScanOptionAllowDuplicatesKey: true])
            } else {
                c.scanForPeripherals(withServices: [service])
            }
        }
    }
    func centralManager(_ c: CBCentralManager, didDiscover p: CBPeripheral, advertisementData: [String: Any], rssi: NSNumber) {
        if scanOnly {
            let local = advertisementData[CBAdvertisementDataLocalNameKey] as? String ?? p.name ?? ""
            let uuids = (advertisementData[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID]) ?? []
            if local.contains("Violet") || uuids.contains(service) {
                print("heard \(local.isEmpty ? "unnamed" : local) \(p.identifier) rssi \(rssi) services \(uuids)")
            }
            return
        }
        guard peripheral == nil else { return }
        let local = advertisementData[CBAdvertisementDataLocalNameKey] as? String ?? p.name ?? ""
        if let n = name, local != n {
            print("skipping \(local.isEmpty ? "unnamed" : local) rssi \(rssi)")
            return
        }
        print("found \(p.name ?? "?") rssi \(rssi) \(advertisementData[CBAdvertisementDataLocalNameKey] ?? "")")
        peripheral = p
        p.delegate = self
        c.stopScan()
        c.connect(p)
    }
    func centralManager(_ c: CBCentralManager, didConnect p: CBPeripheral) {
        print("connected")
        p.discoverServices([service])
    }
    func centralManager(_ c: CBCentralManager, didFailToConnect p: CBPeripheral, error: Error?) {
        print("connect failed: \(String(describing: error))"); exit(1)
    }
    func centralManager(_ c: CBCentralManager, didDisconnectPeripheral p: CBPeripheral, error: Error?) {
        print("disconnected: \(String(describing: error))"); exit(1)
    }
    func peripheral(_ p: CBPeripheral, didDiscoverServices error: Error?) {
        guard let s = p.services?.first(where: { $0.uuid == service }) else { print("no service \(String(describing: error))"); exit(1) }
        p.discoverCharacteristics([characteristicID], for: s)
    }
    func peripheral(_ p: CBPeripheral, didDiscoverCharacteristicsFor s: CBService, error: Error?) {
        guard let ch = s.characteristics?.first(where: { $0.uuid == characteristicID }) else { print("no characteristic"); exit(1) }
        print("characteristic properties \(ch.properties.rawValue)")
        characteristic = ch
        p.setNotifyValue(true, for: ch)
    }
    func peripheral(_ p: CBPeripheral, didUpdateNotificationStateFor ch: CBCharacteristic, error: Error?) {
        print("subscribed: \(ch.isNotifying) \(String(describing: error)); write limit \(p.maximumWriteValueLength(for: .withoutResponse))")
        p.writeValue(announce(), for: ch, type: .withoutResponse)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            p.writeValue(message(say), for: ch, type: .withoutResponse)
            print("posted: \(say)")
            self.posted = true
            self.finishIfDone()
        }
    }
    func peripheral(_ p: CBPeripheral, didUpdateValueFor ch: CBCharacteristic, error: Error?) {
        guard let v = ch.value, let pk = Packet.decode(v) else { print("undecodable notification \(ch.value.map(hex) ?? "")"); return }
        let skew = Int64(pk.timestamp) - Int64(nowMS())
        switch pk.type {
        case 1:
            var nick = "", nk = Data(), sk = Data()
            var b = [UInt8](pk.payload)[...]
            while b.count >= 2 {
                let t = b[b.startIndex], n = Int(b[b.startIndex + 1])
                let v = Data(b.dropFirst(2).prefix(n))
                if t == 1 { nick = String(decoding: v, as: UTF8.self) }
                if t == 2 { nk = v }
                if t == 3 { sk = v }
                b = b.dropFirst(2 + n)
            }
            let idOK = Data(SHA256.hash(data: nk).prefix(8)) == pk.sender
            let sigOK = pk.verify(sk)
            print("announce from \(nick) \(hex(pk.sender)) ttl \(pk.ttl) skew \(skew) ms: id \(idOK) signature \(sigOK)")
            if idOK && sigOK && abs(skew) < 120_000 {
                keys[pk.sender] = (nick, sk)
                announced = true
            }
        case 2:
            let text = String(decoding: pk.payload, as: UTF8.self)
            guard let (nick, sk) = keys[pk.sender] else { print("message before announce: \(text)"); return }
            let ok = pk.verify(sk)
            print("<\(nick)> \(text) (signature \(ok), skew \(skew) ms)")
            if ok, let e = expect, text.contains(e) { heard = true }
        default:
            print("packet type \(pk.type) from \(hex(pk.sender))")
        }
        finishIfDone()
    }
}

let central = Central()
DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
    print("FAIL: timed out (announce \(central.announced), posted \(central.posted), heard \(central.heard))")
    exit(1)
}
RunLoop.main.run()
