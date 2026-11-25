// The connection to the relay (a Chariot, or a relay on the local network):
// a WebSocket, or, when a proxy refuses the upgrade (a Chariot's does), plain
// HTTP: messages go up in POSTs and come down in GETs the relay holds for up
// to 20 seconds. Reconnects wait 1 to 30 seconds. Events arrive on the main queue.
import Foundation

final class RelayLink: NSObject, URLSessionWebSocketDelegate {
    enum Event {
        case connected
        case peerUp
        case peerDown
        case message(Data)
        case lost(String)
    }

    private let base: String
    private let room: String
    private let role: String
    private let onEvent: (Event) -> Void
    private var session: URLSession!
    private var task: URLSessionWebSocketTask?
    private var stopped = false
    private var wait: TimeInterval = 1
    private var pinger: Timer?
    private var generation = 0
    // Polling over HTTP.
    private var polling = false
    private var pollID = ""
    private var outbox = Data()
    private var sending = false

    init?(relay: String, room: Data, role: String, onEvent: @escaping (Event) -> Void) {
        base = relay.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard URL(string: base) != nil else { return nil }
        self.room = room.hex
        self.role = role
        self.onEvent = onEvent
        super.init()
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 40
        config.waitsForConnectivity = true
        config.httpMaximumConnectionsPerHost = 4
        session = URLSession(configuration: config, delegate: self, delegateQueue: .main)
        connect()
    }

    private func http(_ path: String) -> URL {
        var b = base
        if b.hasPrefix("wss://") { b = "https://" + b.dropFirst(6) }
        if b.hasPrefix("ws://") { b = "http://" + b.dropFirst(5) }
        return URL(string: b + path)!
    }

    private func connect() {
        guard !stopped else { return }
        generation += 1
        if polling { return startPolling(generation) }
        var u = http("/v1/pipe/\(room)?role=\(role)").absoluteString
        if u.hasPrefix("https://") { u = "wss://" + u.dropFirst(8) } else if u.hasPrefix("http://") { u = "ws://" + u.dropFirst(7) }
        var req = URLRequest(url: URL(string: u)!)
        req.setValue("VioletAgent-Mac/1", forHTTPHeaderField: "User-Agent")
        let t = session.webSocketTask(with: req)
        t.maximumMessageSize = 1 << 20
        task = t
        t.resume()
        receive(t, generation)
    }

    private func receive(_ t: URLSessionWebSocketTask, _ gen: Int) {
        t.receive { [weak self] result in
            guard let self, gen == self.generation else { return }
            switch result {
            case .success(.data(let d)):
                self.onEvent(.message(d))
            case .success(.string(let s)):
                self.control(s)
            case .success:
                break
            case .failure(let e):
                // A proxy that answers the upgrade with an HTTP error: poll instead.
                if let r = t.response as? HTTPURLResponse, r.statusCode != 101, !self.polling {
                    NSLog("Violet Agent: the relay refused WebSockets (\(r.statusCode)); polling over HTTP")
                    self.polling = true
                    self.task = nil
                    self.connect()
                    return
                }
                self.fail(e.localizedDescription)
                return
            }
            self.receive(t, gen)
        }
    }

    private func control(_ s: String) {
        if s.contains("\"up\"") { onEvent(.peerUp) }
        if s.contains("\"down\"") { onEvent(.peerDown) }
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol protocol: String?) {
        guard webSocketTask === task else { return }
        wait = 1
        onEvent(.connected)
        pinger?.invalidate()
        pinger = Timer.scheduledTimer(withTimeInterval: 20, repeats: true) { [weak self] _ in
            self?.task?.sendPing { error in
                if let error { DispatchQueue.main.async { self?.fail(error.localizedDescription) } }
            }
        }
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        guard webSocketTask === task else { return }
        fail("closed")
    }

    // MARK: polling

    private func startPolling(_ gen: Int) {
        pollID = (0..<8).map { _ in String(format: "%02x", UInt8.random(in: 0...255)) }.joined()
        outbox = Data()
        sending = false
        post(Data(), gen) { [weak self] ok in
            guard let self, gen == self.generation else { return }
            guard ok else { return self.fail("relay unreachable") }
            self.wait = 1
            self.onEvent(.connected)
            self.poll(gen)
        }
    }

    private func post(_ body: Data, _ gen: Int, _ done: @escaping (Bool) -> Void) {
        var req = URLRequest(url: http("/v1/send/\(room)?role=\(role)&id=\(pollID)"))
        req.httpMethod = "POST"
        req.httpBody = body
        req.timeoutInterval = 15
        session.dataTask(with: req) { _, resp, _ in
            let ok = (resp as? HTTPURLResponse)?.statusCode == 200
            DispatchQueue.main.async { done(ok) }
        }.resume()
    }

    private func poll(_ gen: Int) {
        var req = URLRequest(url: http("/v1/recv/\(room)?role=\(role)&id=\(pollID)"))
        req.timeoutInterval = 35
        session.dataTask(with: req) { [weak self] data, resp, err in
            DispatchQueue.main.async {
                guard let self, gen == self.generation else { return }
                let code = (resp as? HTTPURLResponse)?.statusCode ?? 0
                if code >= 500 && code != 501 {
                    // A proxy's own timeout or hiccup: ask again.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { self.poll(gen) }
                    return
                }
                guard code == 200, let data else {
                    return self.fail(err?.localizedDescription ?? "relay answered \((resp as? HTTPURLResponse)?.statusCode ?? 0)")
                }
                let b = [UInt8](data)
                var at = 0
                while at + 5 <= b.count {
                    let n = Int(b[at + 1]) << 24 | Int(b[at + 2]) << 16 | Int(b[at + 3]) << 8 | Int(b[at + 4])
                    guard at + 5 + n <= b.count else { break }
                    let m = Data(b[(at + 5)..<(at + 5 + n)])
                    if b[at] == 1 { self.control(String(decoding: m, as: UTF8.self)) } else if !m.isEmpty { self.onEvent(.message(m)) }
                    at += 5 + n
                }
                self.poll(gen)
            }
        }.resume()
    }

    private func flush() {
        guard polling, !sending, !outbox.isEmpty else { return }
        let gen = generation
        let body = outbox
        outbox = Data()
        sending = true
        post(body, gen) { [weak self] ok in
            guard let self, gen == self.generation else { return }
            self.sending = false
            if !ok { return self.fail("relay refused a message") }
            self.flush()
        }
    }

    // MARK: common

    private func fail(_ why: String) {
        let wasPolling = polling && task == nil
        guard task != nil || wasPolling else { return }
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        generation += 1
        pinger?.invalidate()
        guard !stopped else { return }
        onEvent(.lost(why))
        let delay = wait
        wait = min(wait * 2, 30)
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in self?.connect() }
    }

    func send(_ d: Data) {
        if polling {
            var n = UInt32(d.count).bigEndian
            outbox.append(2)
            outbox.append(Data(bytes: &n, count: 4))
            outbox.append(d)
            // Batch what arrives within 30 ms (40 ms microphone chunks, speech).
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.03) { [weak self] in self?.flush() }
            return
        }
        task?.send(.data(d)) { _ in }
    }

    func stop() {
        stopped = true
        generation += 1
        pinger?.invalidate()
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
        session.invalidateAndCancel()
    }
}
