import Foundation
import Network

/// Demo stand-ins for the `.test` hosts the Violet Shield demo apps use, served
/// on this phone's loopback so nothing reaches a real address, on a device as in
/// the Simulator. A `-hostMapB64` value of `standin:sink` (counts what arrives,
/// keeps nothing) or `standin:web` (a 200 reply to any HTTP request) points a
/// name here. What the sink received is in Documents/standins.json.
final class StandIns {
    static let shared = StandIns()
    private let queue = DispatchQueue(label: "com.immortal.violet.relay.standins")
    private var listeners: [String: NWListener] = [:]
    private(set) var ports: [String: UInt16] = [:]
    private var received: [String: Int] = [:]
    private var connections: [ObjectIdentifier: NWConnection] = [:]

    /// Start the stand-ins the host map names; returns once their ports are known.
    func start(kinds: Set<String>) {
        let ready = DispatchGroup()
        for kind in kinds where ["sink", "web"].contains(kind) {
            let params = NWParameters.tcp
            params.requiredInterfaceType = .loopback
            guard let l = try? NWListener(using: params, on: .any) else { continue }
            ready.enter()
            var entered = true
            l.stateUpdateHandler = { [weak self] state in
                if case .ready = state, let p = l.port?.rawValue {
                    self?.ports[kind] = p
                    if entered { entered = false; ready.leave() }
                } else if case .failed = state, entered {
                    entered = false
                    ready.leave()
                }
            }
            l.newConnectionHandler = { [weak self] c in self?.accept(c, kind: kind) }
            l.start(queue: queue)
            listeners[kind] = l
        }
        _ = ready.wait(timeout: .now() + 5)
    }

    /// "standin:sink" -> ("127.0.0.1", port), or nil for an ordinary host:port.
    func resolve(_ mapped: String) -> (String, UInt16)? {
        guard mapped.hasPrefix("standin:") else { return nil }
        let kind = String(mapped.dropFirst("standin:".count))
        return queue.sync { ports[kind].map { ("127.0.0.1", $0) } }
    }

    private func accept(_ c: NWConnection, kind: String) {
        connections[ObjectIdentifier(c)] = c
        c.stateUpdateHandler = { [weak self] state in
            switch state {
            case .failed, .cancelled: self?.connections[ObjectIdentifier(c)] = nil
            default: break
            }
        }
        c.start(queue: queue)
        if kind == "web" { serveWeb(c, request: Data()) } else { drain(c) }
    }

    private func drain(_ c: NWConnection) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.received["sink", default: 0] += data.count
                self.save()
            }
            if done || error != nil { c.cancel(); return }
            self.drain(c)
        }
    }

    private func serveWeb(_ c: NWConnection, request: Data) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 16384) { [weak self] data, _, done, error in
            guard let self else { return }
            var request = request
            if let data { request.append(data) }
            // Answer once the headers and the declared body have arrived.
            if let end = request.range(of: Data("\r\n\r\n".utf8)) {
                let head = String(decoding: request[..<end.lowerBound], as: UTF8.self)
                let length = head.split(separator: "\r\n").compactMap { line -> Int? in
                    let p = line.split(separator: ":", maxSplits: 1)
                    return p.count == 2 && p[0].lowercased() == "content-length" ? Int(p[1].trimmingCharacters(in: .whitespaces)) : nil
                }.first ?? 0
                if request.count - end.upperBound >= length || done {
                    let body = head.contains("forecast") ? #"{"city": "Violet Bay", "forecast": "sunny, 21C"}"# : "OK synced\n"
                    let reply = "HTTP/1.0 200 OK\r\nContent-Length: \(body.utf8.count)\r\n\r\n\(body)"
                    self.received["web", default: 0] += request.count
                    self.save()
                    c.send(content: Data(reply.utf8), completion: .contentProcessed { _ in c.cancel() })
                    return
                }
            }
            if done || error != nil { c.cancel(); return }
            self.serveWeb(c, request: request)
        }
    }

    private var lastSave = Date.distantPast
    private func save() {
        guard Date().timeIntervalSince(lastSave) > 0.5 else { return }
        lastSave = Date()
        let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("standins.json")
        if let d = try? JSONSerialization.data(withJSONObject: ["received": received, "ports": ports]) { try? d.write(to: url) }
    }

    /// Write the totals now (the last few chunks may have arrived within the save interval).
    func flush() {
        queue.async { self.lastSave = .distantPast; self.save() }
    }
}
