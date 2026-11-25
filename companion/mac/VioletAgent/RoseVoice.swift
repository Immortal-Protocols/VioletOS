// The agent's voice on RoseOS's models (voice/violet_voice.py, run with the
// RoseOS voice environment): Rose's listener and Phonon speaker, and her
// barge-in rules. The phone plays the speech and reports progress back.
import Foundation

final class RoseVoice {
    var onReady: (String) -> Void = { _ in }
    var onFailed: (String) -> Void = { _ in }
    var onTurn: (String, Bool) -> Void = { _, _ in }      // text, hold ("wait")
    var onPartial: (String) -> Void = { _ in }
    var onControl: ([String: Any]) -> Void = { _ in }     // for the phone's player
    var onPCM: (UInt32, Data) -> Void = { _, _ in }       // segment, 24 kHz s16
    var onAudible: (Bool) -> Void = { _ in }
    var onReplyDone: (Int, Bool) -> Void = { _, _ in }    // id, interrupted
    var onLog: (String) -> Void = { _ in }

    private var process: Process?
    private var input: FileHandle?
    private var buffer = Data()
    private let queue = DispatchQueue(label: "rose-voice-in")
    private(set) var ready = false

    /// The RoseOS voice demo (models, venv): ROSE_VOICE, else ~/RoseOS-1/prototypes/voice-phone-demo.
    static func home() -> URL {
        let env = ProcessInfo.processInfo.environment["ROSE_VOICE"]
        return URL(fileURLWithPath: env ?? NSHomeDirectory() + "/RoseOS-1/prototypes/voice-phone-demo")
    }

    static func available() -> Bool {
        FileManager.default.isExecutableFile(atPath: home().appendingPathComponent(".venv/bin/python").path)
    }

    func start(script: URL, voice: String) {
        let p = Process()
        p.executableURL = RoseVoice.home().appendingPathComponent(".venv/bin/python")
        p.arguments = [script.path]
        var env = ProcessInfo.processInfo.environment
        env["ROSE_VOICE"] = RoseVoice.home().path
        env["VIOLET_VOICE"] = voice
        env["PYTHONUNBUFFERED"] = "1"
        env["HF_HUB_OFFLINE"] = "1"
        p.environment = env
        let inPipe = Pipe(), outPipe = Pipe()
        p.standardInput = inPipe
        p.standardOutput = outPipe
        p.standardError = FileHandle.nullDevice
        outPipe.fileHandleForReading.readabilityHandler = { [weak self] h in
            let d = h.availableData
            DispatchQueue.main.async { self?.received(d) }
        }
        p.terminationHandler = { [weak self] proc in
            DispatchQueue.main.async {
                self?.ready = false
                self?.onFailed("The RoseOS voice stopped (status \(proc.terminationStatus))")
            }
        }
        do { try p.run() } catch { return onFailed("Could not start the RoseOS voice: \(error.localizedDescription)") }
        process = p
        input = inPipe.fileHandleForWriting
    }

    private func frame(_ kind: UInt8, _ data: Data) {
        var head = Data([kind])
        var n = UInt32(data.count).bigEndian
        head.append(Data(bytes: &n, count: 4))
        let h = input
        queue.async { h?.write(head + data) }
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
    func reply(_ id: Int, _ text: String) { command(["cmd": "reply", "id": id, "text": text]) }
    func replyEnd(_ id: Int) { command(["cmd": "reply_end", "id": id]) }
    func stop(_ reason: String) { command(["cmd": "stop", "reason": reason]) }
    func client(_ msg: [String: Any]) { command(["cmd": "client", "msg": msg]) }
    func setVoice(_ name: String) { command(["cmd": "voice", "name": name]) }

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
        case "ready":
            ready = true
            onReady(e["voice"] as? String ?? "")
        case "turn":
            onTurn(e["text"] as? String ?? "", e["hold"] as? Bool ?? false)
        case "partial":
            onPartial(e["text"] as? String ?? "")
        case "speech_start":
            onLog("heard speech starting")
        case "control":
            if let m = e["msg"] as? [String: Any] { onControl(m) }
        case "audible":
            onAudible(e["audible"] as? Bool ?? false)
        case "reply_done":
            onReplyDone(e["id"] as? Int ?? 0, e["interrupted"] as? Bool ?? false)
        case "barge":
            onLog("barge-in: \(e["stage"] ?? "") (\(e["reason"] ?? ""))")
        case "error":
            onLog("voice: \(e["message"] ?? "")")
            if !ready { onFailed("\(e["message"] ?? "")") }
        default:
            break
        }
    }

    func terminate() { process?.terminate() }
}
