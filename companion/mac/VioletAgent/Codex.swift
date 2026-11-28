// The signed-in Codex app server (`codex app-server`, JSON-RPC over stdio), one
// thread per run of the app, in the app workspace. The builder writes and
// builds apps there with the workspace-write sandbox and no approvals.
import Foundation

final class Codex {
    var onDelta: (String, String) -> Void = { _, _ in }       // item id, text
    var onMessage: (String, String) -> Void = { _, _ in }     // item id, full text
    var onActivity: (String) -> Void = { _ in }               // "thinking", "building", "idle"
    var onTool: (String, [String: Any], @escaping ([String: Any]) -> Void) -> Void = { _, _, done in done(["success": false, "contentItems": []]) }
    var onLog: (String) -> Void = { _ in }
    /// What the thread is doing: a command it runs or a file it changes.
    var onProgress: (String) -> Void = { _ in }
    /// A turn ended (true when it completed rather than failed or was interrupted).
    var onTurnEnd: (Bool) -> Void = { _ in }

    private var process: Process?
    private var stdin: FileHandle?
    private var buffer = Data()
    private var nextID = 1
    private var pending: [Int: ([String: Any]?, String?) -> Void] = [:]
    private(set) var thread: String?
    private(set) var activeTurn: String?
    private(set) var model = "gpt-6.2-sol"
    private var tier = "ultrafast"
    private let workspace: URL
    private let instructions: String
    private let tools: [[String: Any]]
    private let sandbox: String
    private let effort: String
    let label: String

    init(_ label: String, workspace: URL, instructions: String, tools: [[String: Any]], sandbox: String = "workspace-write", effort: String = "low") {
        self.label = label
        self.workspace = workspace
        self.instructions = instructions
        self.tools = tools
        self.sandbox = sandbox
        self.effort = effort
    }

    static func binary() -> String? {
        let fm = FileManager.default
        let environment = ProcessInfo.processInfo.environment
        let installed = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex", NSHomeDirectory() + "/.local/bin/codex"]
        let onPath = (environment["PATH"] ?? "").split(separator: ":").map { "\($0)/codex" }
        let override = environment["PIXEL_CODEX_BIN"].map { [$0] } ?? []
        return (override + installed + onPath).first { $0.hasPrefix("/") && fm.isExecutableFile(atPath: $0) }
    }

    func start(login: CodexLogin, account: @escaping (Bool) -> Void, ready: @escaping (String?) -> Void) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: login.binary)
        p.arguments = ["app-server"]
        p.currentDirectoryURL = workspace
        p.environment = login.environment
        let inPipe = Pipe(), outPipe = Pipe()
        p.standardInput = inPipe
        p.standardOutput = outPipe
        p.standardError = FileHandle.nullDevice
        outPipe.fileHandleForReading.readabilityHandler = { [weak self] h in
            let d = h.availableData
            if d.isEmpty { h.readabilityHandler = nil; return }
            DispatchQueue.main.async { self?.received(d) }
        }
        p.terminationHandler = { [weak self] _ in
            DispatchQueue.main.async {
                guard let self else { return }
                self.onLog("Codex (\(self.label)) stopped")
                self.thread = nil
                self.activeTurn = nil
                let pending = self.pending
                self.pending.removeAll()
                for reply in pending.values { reply(nil, "Codex stopped") }
            }
        }
        do { try p.run() } catch { return ready("Could not start Codex: \(error.localizedDescription)") }
        process = p
        stdin = inPipe.fileHandleForWriting
        call("initialize", ["clientInfo": ["name": "violet-agent", "version": "1.0"], "capabilities": ["experimentalApi": true]]) { [weak self] _, err in
            guard let self else { return }
            if let err { return ready(err) }
            self.write(["method": "initialized", "params": [String: Any]()])
            self.call("account/read", ["refreshToken": true]) { r, err in
                if let err {
                    account(false)
                    return ready("Could not check ChatGPT sign-in: \(err)")
                }
                let signedIn = (r?["account"] as? [String: Any])?["type"] as? String == "chatgpt"
                account(signedIn)
                guard signedIn else { return ready("Builder is waiting for ChatGPT sign-in") }
                self.call("model/list", [:]) { r, _ in
                    let models = (r?["data"] as? [[String: Any]]) ?? []
                    let chosen = models.first { $0["id"] as? String == self.model } ?? models.first { $0["isDefault"] as? Bool == true }
                    if let m = chosen?["id"] as? String { self.model = m }
                    let tiers = ((chosen?["serviceTiers"] as? [[String: Any]]) ?? []).compactMap { $0["id"] as? String }
                    self.tier = tiers.contains("ultrafast") ? "ultrafast" : tiers.contains("priority") ? "priority" : "default"
                    self.newThread(ready)
                }
            }
        }
    }

    func newThread(_ ready: @escaping (String?) -> Void) {
        let params: [String: Any] = [
            "cwd": workspace.path, "approvalPolicy": "never", "sandbox": sandbox,
            "model": model, "serviceTier": tier, "developerInstructions": instructions, "dynamicTools": tools,
        ]
        call("thread/start", params) { [weak self] r, err in
            guard let self else { return }
            if let err { return ready(err) }
            self.thread = (r?["thread"] as? [String: Any])?["id"] as? String
            self.onLog("Codex \(self.label): \(self.model), \(self.tier), \(self.sandbox)")
            ready(self.thread == nil ? "Codex did not start a thread" : nil)
        }
    }

    /// A user turn, or more input for the turn in progress.
    func say(_ text: String) {
        guard let thread else { return }
        let input: [[String: Any]] = [["type": "text", "text": text]]
        if let turn = activeTurn {
            call("turn/steer", ["threadId": thread, "expectedTurnId": turn, "input": input]) { [weak self] _, err in
                if err != nil { self?.startTurn(input) }
            }
        } else {
            startTurn(input)
        }
    }

    private func startTurn(_ input: [[String: Any]]) {
        guard let thread else { return }
        onActivity("thinking")
        call("turn/start", ["threadId": thread, "input": input, "effort": effort, "serviceTierForTurn": tier]) { [weak self] r, err in
            if let err { self?.onLog("Codex \(self?.label ?? "") turn failed: \(err)"); self?.onActivity("idle"); self?.onTurnEnd(false); return }
            if let id = (r?["turn"] as? [String: Any])?["id"] as? String { self?.activeTurn = id }
        }
    }

    func interrupt() {
        guard let thread, let turn = activeTurn else { return }
        call("turn/interrupt", ["threadId": thread, "turnId": turn]) { _, _ in }
    }

    // MARK: JSON-RPC

    private func write(_ o: [String: Any]) {
        guard let d = try? JSONSerialization.data(withJSONObject: o) else { return }
        stdin?.write(d + Data("\n".utf8))
    }

    private func call(_ method: String, _ params: [String: Any], _ done: @escaping ([String: Any]?, String?) -> Void) {
        let id = nextID
        nextID += 1
        pending[id] = done
        write(["id": id, "method": method, "params": params])
    }

    private func received(_ d: Data) {
        buffer.append(d)
        while let nl = buffer.firstIndex(of: 0x0a) {
            let line = buffer[buffer.startIndex..<nl]
            buffer.removeSubrange(buffer.startIndex...nl)
            guard let o = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else { continue }
            handle(o)
        }
    }

    private func handle(_ e: [String: Any]) {
        let method = e["method"] as? String
        if let id = e["id"] as? Int, method == nil {
            let done = pending.removeValue(forKey: id)
            let err = (e["error"] as? [String: Any])?["message"] as? String
            done?(e["result"] as? [String: Any], err)
            return
        }
        let p = e["params"] as? [String: Any] ?? [:]
        if let id = e["id"] {
            // A request from the server: tool calls are ours; anything else is refused.
            if method == "item/tool/call", let tool = p["tool"] as? String {
                onTool(tool, p["arguments"] as? [String: Any] ?? [:]) { [weak self] result in
                    self?.write(["id": id, "result": result])
                }
            } else {
                write(["id": id, "error": ["code": -32000, "message": "Violet Agent does not approve requests"]])
            }
            return
        }
        if let t = p["threadId"] as? String, t != thread { return }
        switch method {
        case "item/agentMessage/delta":
            if let item = p["itemId"] as? String, let delta = p["delta"] as? String { onDelta(item, delta) }
        case "item/started":
            guard let item = p["item"] as? [String: Any] else { break }
            switch item["type"] as? String {
            case "commandExecution":
                onActivity("building")
                onProgress("running " + String((item["command"] as? String ?? "a command").prefix(120)))
            case "fileChange":
                let paths = ((item["changes"] as? [[String: Any]]) ?? []).compactMap { $0["path"] as? String }
                onProgress("editing " + (paths.map { URL(fileURLWithPath: $0).lastPathComponent }.joined(separator: ", ")))
            case "mcpToolCall", "dynamicToolCall":
                onProgress("calling " + (item["tool"] as? String ?? "a tool"))
            default:
                break
            }
        case "item/completed":
            if let item = p["item"] as? [String: Any], item["type"] as? String == "agentMessage",
               let id = item["id"] as? String, let text = item["text"] as? String {
                onMessage(id, text)
            }
        case "turn/started":
            if let t = (p["turn"] as? [String: Any])?["id"] as? String { activeTurn = t }
        case "turn/completed":
            activeTurn = nil
            onActivity("idle")
            let status = (p["turn"] as? [String: Any])?["status"] as? String ?? "completed"
            onTurnEnd(status == "completed")
        default:
            break
        }
    }

    func stop() {
        if let process, process.isRunning { process.terminate() }
    }
}
