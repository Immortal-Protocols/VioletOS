// The agent itself: Rose, from RoseOS (companion/mac/roseos, run by
// companion/mac/violet_rose/host.py in Rose's Python environment). Rose hears
// the phone's microphone, holds the conversation with her Codex conductor,
// keeps her name, voice, tasks, transcripts and her life and journal, and
// speaks; this app carries her audio to and from the phone and answers her
// calls for VioletOS (opening, listing and removing apps, building apps).
import Foundation

final class RoseAgent {
    var onBus: ([String: Any]) -> Void = { _ in }          // every event on Rose's bus
    var onControl: ([String: Any]) -> Void = { _ in }      // for the phone's player
    var onPCM: (UInt32, Data) -> Void = { _, _ in }        // segment, 24 kHz s16
    var onCall: (Int, String, [String: Any]) -> Void = { _, _, _ in }  // id, name, args: answer(id, result)
    var onBuild: (Int, String) -> Void = { _, _ in }       // task, request
    var onBuildUpdate: (Int, String) -> Void = { _, _ in } // task, instruction
    var onBuildCancel: (Int) -> Void = { _ in }
    var onDashboard: (String) -> Void = { _ in }
    var onFailed: (String) -> Void = { _ in }

    private var process: Process?
    private var input: FileHandle?
    private var buffer = Data()
    private let queue = DispatchQueue(label: "rose-agent-in")
    private(set) var ready = false

    /// Rose's Python environment (MLX, her models): ROSE_ENV, the checkout's build/rose-env
    /// (companion/mac/violet_rose/setup.sh), else the RoseOS checkout's.
    static func environment(repo: URL) -> URL? {
        var candidates: [URL] = []
        if let env = ProcessInfo.processInfo.environment["ROSE_ENV"] { candidates.append(URL(fileURLWithPath: env)) }
        candidates.append(repo.appendingPathComponent("build/rose-env"))
        let rose = ProcessInfo.processInfo.environment["ROSE_VOICE"] ?? NSHomeDirectory() + "/RoseOS-1/prototypes/voice-phone-demo"
        candidates.append(URL(fileURLWithPath: rose).appendingPathComponent(".venv"))
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.appendingPathComponent("bin/python").path) }
    }

    func start(repo: URL, profile: URL, name: String, voice: String?, log: URL, login: CodexLogin) -> Bool {
        guard let env = RoseAgent.environment(repo: repo) else {
            onFailed("Rose's Python environment is missing: run companion/mac/violet_rose/setup.sh")
            return false
        }
        let p = Process()
        p.executableURL = env.appendingPathComponent("bin/python")
        var args = [repo.appendingPathComponent("companion/mac/violet_rose/host.py").path, "--profile", profile.path, "--name", name]
        if let voice { args += ["--voice", voice] }
        p.arguments = args
        var environment = login.environment
        environment["VIOLET_CODEX_BIN"] = login.binary
        environment["PIXEL_CODEX_BIN"] = repo.appendingPathComponent("companion/mac/violet_rose/codex.sh").path
        environment["PYTHONUNBUFFERED"] = "1"
        environment["HF_HUB_OFFLINE"] = "1"
        p.environment = environment
        let inPipe = Pipe(), outPipe = Pipe()
        p.standardInput = inPipe
        p.standardOutput = outPipe
        FileManager.default.createFile(atPath: log.path, contents: nil)
        p.standardError = (try? FileHandle(forWritingTo: log)) ?? FileHandle.nullDevice
        outPipe.fileHandleForReading.readabilityHandler = { [weak self] h in
            let d = h.availableData
            DispatchQueue.main.async { self?.received(d) }
        }
        p.terminationHandler = { [weak self] proc in
            DispatchQueue.main.async {
                self?.ready = false
                self?.onFailed("Rose stopped (status \(proc.terminationStatus)); see rose.log")
            }
        }
        do { try p.run() } catch {
            onFailed("Could not start Rose: \(error.localizedDescription)")
            return false
        }
        process = p
        input = inPipe.fileHandleForWriting
        return true
    }

    private func frame(_ kind: UInt8, _ data: Data) {
        var head = Data([kind])
        var n = UInt32(data.count).bigEndian
        head.append(Data(bytes: &n, count: 4))
        let h = input
        queue.async { try? h?.write(contentsOf: head + data) }
    }

    private func command(_ c: [String: Any]) {
        guard let d = try? JSONSerialization.data(withJSONObject: c) else { return }
        frame(1, d)
    }

    /// The phone's microphone is quiet (speech a metre away peaks near -35 dBFS),
    /// and Rose's VAD and ASR expect a headset-like level: follow the recent
    /// peak and raise speech to about -12 dBFS, at most 20x.
    private var recentPeak: Float = 0.01
    private var gain: Float = 8

    func feed(_ pcm: Data) {
        guard ready else { return }
        var out = Data(count: pcm.count)
        var peak: Float = 0
        let g = gain
        pcm.withUnsafeBytes { raw in
            out.withUnsafeMutableBytes { dst in
                let s = raw.bindMemory(to: Int16.self), d = dst.bindMemory(to: Int16.self)
                for i in 0..<s.count {
                    let v = Float(Int16(littleEndian: s[i]))
                    peak = max(peak, abs(v) / 32768)
                    d[i] = Int16(max(-32767, min(32767, v * g))).littleEndian
                }
            }
        }
        recentPeak = max(peak, recentPeak * 0.97)
        gain = min(20, 0.25 / max(recentPeak, 0.0125))
        frame(3, out)
    }

    func phone(online: Bool) { command(["cmd": "phone", "online": online]) }
    func client(_ msg: [String: Any]) { command(["cmd": "client", "msg": msg]) }
    func text(_ t: String) { command(["cmd": "text", "text": t]) }
    func interrupt() { command(["cmd": "interrupt"]) }
    func rename(_ name: String) { command(["cmd": "rename", "name": name]) }
    func answer(_ id: Int, _ result: [String: Any]) { command(["cmd": "answer", "id": id, "result": result]) }
    func buildProgress(_ task: Int, _ detail: String) { command(["cmd": "build_progress", "task": task, "detail": detail]) }
    func buildDone(_ task: Int, status: String, result: String) {
        command(["cmd": "build_done", "task": task, "status": status, "result": result])
    }

    private func received(_ d: Data) {
        buffer.append(d)
        while buffer.count >= 5 {
            let b = [UInt8](buffer.prefix(5))
            let n = Int(b[1]) << 24 | Int(b[2]) << 16 | Int(b[3]) << 8 | Int(b[4])
            guard buffer.count >= 5 + n else { return }
            let body = buffer.subdata(in: buffer.startIndex + 5 ..< buffer.startIndex + 5 + n)
            buffer.removeSubrange(buffer.startIndex ..< buffer.startIndex + 5 + n)
            if b[0] == 2, body.count >= 4 {
                let seg = body.prefix(4).withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }
                onPCM(UInt32(littleEndian: seg), body.dropFirst(4))
            } else if b[0] == 1, let e = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                event(e)
            }
        }
    }

    private func event(_ e: [String: Any]) {
        switch e["ev"] as? String {
        case "bus":
            guard let m = e["msg"] as? [String: Any] else { return }
            if m["kind"] as? String == "ready" { ready = true }
            onBus(m)
        case "control":
            if let m = e["msg"] as? [String: Any] { onControl(m) }
        case "call":
            onCall(e["id"] as? Int ?? 0, e["name"] as? String ?? "", e["args"] as? [String: Any] ?? [:])
        case "build":
            onBuild(e["task"] as? Int ?? 0, e["request"] as? String ?? "")
        case "build_update":
            onBuildUpdate(e["task"] as? Int ?? 0, e["instruction"] as? String ?? "")
        case "build_cancel":
            onBuildCancel(e["task"] as? Int ?? 0)
        case "dashboard":
            onDashboard(e["url"] as? String ?? "")
        default:
            break
        }
    }

    func terminate() { process?.terminate() }
}
