// The connection to the relay (a Chariot): one WebSocket per room, kept up
// with reconnects whose waits grow from 1 to 30 seconds. Events arrive on
// the main queue.
import Foundation

final class RelayLink: NSObject, URLSessionWebSocketDelegate {
    enum Event {
        case connected
        case peerUp
        case peerDown
        case message(Data)
        case lost(String)
    }

    private let url: URL
    private let onEvent: (Event) -> Void
    private var session: URLSession!
    private var task: URLSessionWebSocketTask?
    private var stopped = false
    private var wait: TimeInterval = 1
    private var pinger: Timer?
    private var generation = 0

    init?(relay: String, room: Data, role: String, onEvent: @escaping (Event) -> Void) {
        var base = relay.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if base.hasPrefix("https://") { base = "wss://" + base.dropFirst(8) }
        if base.hasPrefix("http://") { base = "ws://" + base.dropFirst(7) }
        guard let u = URL(string: "\(base)/v1/pipe/\(room.hex)?role=\(role)") else { return nil }
        url = u
        self.onEvent = onEvent
        super.init()
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 70
        config.waitsForConnectivity = true
        session = URLSession(configuration: config, delegate: self, delegateQueue: .main)
        connect()
    }

    private func connect() {
        guard !stopped else { return }
        generation += 1
        var req = URLRequest(url: url)
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
                if s.contains("\"up\"") { self.onEvent(.peerUp) }
                if s.contains("\"down\"") { self.onEvent(.peerDown) }
            case .success:
                break
            case .failure(let e):
                self.fail(e.localizedDescription)
                return
            }
            self.receive(t, gen)
        }
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

    private func fail(_ why: String) {
        guard task != nil else { return }
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
        task?.send(.data(d)) { _ in }
    }

    func stop() {
        stopped = true
        pinger?.invalidate()
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
        session.invalidateAndCancel()
    }
}
