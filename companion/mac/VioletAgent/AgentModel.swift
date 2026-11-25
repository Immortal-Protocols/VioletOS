// Violet Agent's state: the paired phone, speech in and out, Codex, and the
// installs it asks for. Everything runs on the main queue.
import CryptoKit
import Foundation
import SwiftUI

struct Line: Identifiable {
    let id = UUID()
    let user: Bool
    let text: String
}

final class AgentModel: ObservableObject {
    @Published var relayUp = false
    @Published var online = false
    @Published var phone: PairedPhone?
    @Published var lines: [Line] = []
    @Published var heard = ""
    @Published var codexState = "Starting Codex…"
    @Published var speechState = "Starting speech recognition…"
    @Published var workspaceState = "Preparing the app workspace…"
    @Published var activity = "idle"
    @Published var scanning = false
    @Published var lastTiming: String?

    private var saved: Saved
    private let identity: Curve25519.Signing.PrivateKey
    private var link: PhoneLink!
    private let listener = Listener()
    private let speaker = Speaker()
    private var codex: Codex!
    let support: URL
    let repo: URL
    let workspace: URL

    // Speech the phone is playing, for the echo guard.
    private var playingUntil = Date.distantPast
    private var spoken: [(Date, Set<String>)] = []
    private var bargedIn = false
    // Streaming replies: what was already said of each message.
    private var said: [String: Int] = [:]
    private var texts: [String: String] = [:]
    // Installs and the request they answer, for timing.
    private var requestAt: Date?
    private var request = ""
    private var installs: [String: (Date, ([String: Any]) -> Void)] = [:]
    private var timing: [String: Any] = [:]
    private var pendingSay: String?
    /// --install NAME=PATH: install a built program once the phone is online (tests).
    private var pendingInstall: (String, String)?

    init() {
        saved = Store.load()
        identity = Store.identity(saved)
        let fm = FileManager.default
        let profile = ProcessInfo.processInfo.environment["VIOLET_AGENT_PROFILE"] ?? ""
        support = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(profile.isEmpty ? "Violet Agent" : "Violet Agent \(profile)")
        try? fm.createDirectory(at: support, withIntermediateDirectories: true)
        let path = Bundle.main.object(forInfoDictionaryKey: "VioletRepoPath") as? String ?? ""
        repo = URL(fileURLWithPath: ProcessInfo.processInfo.environment["VIOLET_REPO"] ?? path).standardizedFileURL
        workspace = repo.appendingPathComponent("build/agent-workspace")
        try? fm.createDirectory(at: workspace, withIntermediateDirectories: true)
        link = PhoneLink(identity: identity) { [weak self] e in self?.on(e) }
        phone = saved.phones.first
        log("Violet Agent started; Mac key \(identity.publicKey.rawRepresentation.hex.prefix(16)); repo \(repo.path)")
    }

    // MARK: start

    func start() {
        let args = CommandLine.arguments
        if let i = args.firstIndex(of: "--unpair"), i >= 0 { saved.phones = []; Store.save(saved); phone = nil }
        if let i = args.firstIndex(of: "--say"), i + 1 < args.count { pendingSay = args[i + 1] }
        if let i = args.firstIndex(of: "--install"), i + 1 < args.count, let eq = args[i + 1].firstIndex(of: "=") {
            pendingInstall = (String(args[i + 1][..<eq]), String(args[i + 1][args[i + 1].index(after: eq)...]))
        }
        if let p = phone { link.connect(p) }
        if let i = args.firstIndex(of: "--pair"), i + 1 < args.count, let inv = Invitation(args[i + 1]) { pair(inv) }
        speaker.onPCM = { [weak self] id, pcm in self?.sendSpeech(id, pcm) }
        speaker.onEnd = { [weak self] id in
            guard let self else { return }
            self.link.send(["t": "say_end", "id": id])
            // Back to listening once the last sentence is out and Codex is done.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                if !self.speaker.speaking && self.codex.activeTurn == nil && self.activity == "speaking" { self.setActivity("idle") }
            }
        }
        listener.onPartial = { [weak self] t in self?.partial(t) }
        listener.onFinal = { [weak self] t in self?.final(t) }
        if args.contains("--no-speech") {
            speechState = "Speech recognition off (--no-speech)"
        } else {
            listener.start { [weak self] ok in
                self?.speechState = ok ? "Speech recognition on this Mac (\(self?.speaker.voiceName ?? "") voice)" : "Allow Speech Recognition in System Settings › Privacy"
            }
        }
        prepareWorkspace()
        startCodex()
        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.writeStatus() }
    }

    private func prepareWorkspace() {
        let script = repo.appendingPathComponent("tools/agent_build.py")
        guard FileManager.default.fileExists(atPath: script.path) else {
            workspaceState = "VioletOS checkout not found at \(repo.path)"
            return
        }
        DispatchQueue.global().async {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
            p.arguments = [script.path, "setup", self.workspace.path]
            p.standardOutput = FileHandle.nullDevice
            p.standardError = FileHandle.nullDevice
            let started = Date()
            try? p.run()
            p.waitUntilExit()
            let ok = p.terminationStatus == 0
            DispatchQueue.main.async {
                self.workspaceState = ok ? String(format: "App workspace ready (warmed in %.0f s)", Date().timeIntervalSince(started)) : "The app workspace failed to build its example; see build/agent-workspace"
                self.log(self.workspaceState)
            }
        }
    }

    private static let instructions = """
    You are Violet, the voice agent of the user's VioletOS phone (a Moto G Pure). You run on their Mac. \
    Everything you write is spoken aloud on the phone, so answer like a person talking: one or two short sentences, \
    no markdown, lists, code, paths or URLs. When the user asks for an app, follow AGENTS.md in this workspace: \
    say one short line first, build it, call install_app, then say it is ready. Choose sensible defaults instead of asking. \
    If the user asks to open an app you installed, call open_app.
    """

    private static let tools: [[String: Any]] = [
        ["type": "function", "name": "install_app",
         "description": "Install a program built with ./build on the user's phone. Returns once it is installed and listed on the phone.",
         "inputSchema": ["type": "object", "properties": [
            "name": ["type": "string", "description": "The app's folder name under apps/"],
            "title": ["type": "string", "description": "The title shown on the phone"],
            "path": ["type": "string", "description": "The path ./build printed"]],
                         "required": ["name", "title", "path"]]],
        ["type": "function", "name": "open_app",
         "description": "Open an installed app on the phone's screen.",
         "inputSchema": ["type": "object", "properties": ["name": ["type": "string"]], "required": ["name"]]],
    ]

    private func startCodex() {
        codex = Codex(workspace: workspace, instructions: Self.instructions, tools: Self.tools)
        codex.onLog = { [weak self] s in self?.log(s) }
        codex.onDelta = { [weak self] id, d in self?.delta(id, d) }
        codex.onMessage = { [weak self] id, t in self?.message(id, t) }
        codex.onActivity = { [weak self] a in self?.setActivity(a) }
        codex.onTool = { [weak self] tool, args, done in self?.tool(tool, args, done) }
        codex.start { [weak self] err in
            guard let self else { return }
            self.codexState = err ?? "Codex ready (\(self.codex.model))"
            self.log(self.codexState)
            self.sayPending()
        }
    }

    private func sayPending() {
        guard let s = pendingSay, online, codex.thread != nil else { return }
        pendingSay = nil
        log("typed request: \(s)")
        userTurn(s)
    }

    // MARK: pairing

    func pair(_ inv: Invitation) {
        scanning = false
        log("pairing with \(inv.name) through \(inv.relay)")
        link.pair(inv)
    }

    func unpair() {
        link.send(["t": "unpair"])
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [self] in
            link.stop()
            saved.phones = []
            Store.save(saved)
            phone = nil
            online = false
            log("unpaired")
        }
    }

    private func on(_ e: PhoneLink.Event) {
        switch e {
        case .relay(let up):
            relayUp = up
        case .paired(let p):
            saved.phones = [p]
            Store.save(saved)
            phone = p
            lines.append(Line(user: false, text: "Paired with \(p.name)."))
            log("paired with \(p.name)")
        case .online(let up):
            if up != online { log(up ? "phone online" : "phone offline") }
            online = up
            if up {
                link.send(["t": "hello", "name": Host.current().localizedName ?? "Mac"])
                sayPending()
                if CommandLine.arguments.contains("--unpair-remote") {
                    log("unpairing (--unpair-remote)")
                    unpair()
                }
                if let (name, path) = pendingInstall {
                    pendingInstall = nil
                    install(["name": name, "title": name.capitalized, "path": path]) { [weak self] ok, text in
                        self?.log("test install \(name): \(ok ? "ok" : "failed") \(text)")
                    }
                }
            }
        case .mic(let pcm):
            listener.feed(pcm)
        case .json(let o):
            phoneMessage(o)
        }
    }

    private func phoneMessage(_ o: [String: Any]) {
        switch o["t"] as? String {
        case "stop":
            log("tapped to interrupt")
            bargeIn()
        case "installed":
            let name = o["name"] as? String ?? ""
            if let (_, done) = installs.removeValue(forKey: name) {
                done(o)
            }
        case "opened":
            let name = o["name"] as? String ?? ""
            if timing["name"] as? String == name, timing["opened_s"] == nil, let at = requestAt {
                timing["opened_s"] = round(Date().timeIntervalSince(at) * 10) / 10
                finishTiming()
            }
        case "unpair":
            log("the phone unpaired")
            link.stop()
            saved.phones = []
            Store.save(saved)
            phone = nil
            online = false
        default:
            break
        }
    }

    // MARK: listening

    private func words(_ s: String) -> [String] {
        s.lowercased().components(separatedBy: CharacterSet.alphanumerics.inverted).filter { $0.count > 2 }
    }

    private var agentAudible: Bool { speaker.speaking || Date() < playingUntil }

    /// Words heard that the agent is not saying itself.
    private func novel(_ s: String) -> Int {
        let recent = spoken.filter { Date().timeIntervalSince($0.0) < 15 }.reduce(into: Set<String>()) { $0.formUnion($1.1) }
        return words(s).filter { !recent.contains($0) }.count
    }

    private func partial(_ t: String) {
        heard = t
        if agentAudible && !bargedIn && novel(t) >= 2 {
            log("barge-in: \(t)")
            bargeIn()
        }
    }

    private func final(_ t: String) {
        heard = ""
        if agentAudible && !bargedIn && novel(t) < 2 {
            log("ignored our own voice: \(t)")
            return
        }
        bargedIn = false
        userTurn(t)
    }

    func userTurn(_ t: String) {
        lines.append(Line(user: true, text: t))
        link.send(["t": "transcript", "who": "user", "text": t, "final": true])
        log("user: \(t)")
        if requestAt == nil || activity == "idle" {
            requestAt = Date()
            request = t
            timing = ["request": t, "at": ISO8601DateFormatter().string(from: Date())]
        }
        guard codex.thread != nil else {
            say("Codex isn't ready on the Mac yet.")
            return
        }
        codex.say(t)
    }

    private func bargeIn() {
        bargedIn = agentAudible
        speaker.stop()
        playingUntil = .distantPast
        link.send(["t": "say_stop"])
        setActivity(codex.activeTurn != nil ? "thinking" : "idle")
    }

    // MARK: speaking

    private func clean(_ s: String) -> String {
        var t = s.replacingOccurrences(of: "`", with: "").replacingOccurrences(of: "**", with: "")
        t = t.replacingOccurrences(of: "#", with: "").replacingOccurrences(of: "\n", with: " ")
        return t.trimmingCharacters(in: .whitespaces)
    }

    private func say(_ s: String) {
        let t = clean(s)
        guard !t.isEmpty else { return }
        spoken.append((Date(), Set(words(t))))
        spoken.removeAll { Date().timeIntervalSince($0.0) > 30 }
        speaker.say(t)
        setActivity("speaking")
    }

    private func sendSpeech(_ id: UInt32, _ pcm: Data) {
        var body = Data()
        withUnsafeBytes(of: id.littleEndian) { body.append(contentsOf: $0) }
        body.append(pcm)
        link.send(kind: 0x21, body)
        let seconds = Double(pcm.count / 2) / 24000
        playingUntil = max(playingUntil, Date()) + seconds
        // The phone's ring and the relay add about half a second.
        playingUntil = max(playingUntil, Date() + 0.6)
    }

    /// Speak whole sentences of a reply as they stream in.
    private func delta(_ id: String, _ d: String) {
        let text = (texts[id] ?? "") + d
        texts[id] = text
        var from = said[id] ?? 0
        let chars = Array(text)
        var i = from
        while i < chars.count {
            let c = chars[i]
            if ".!?".contains(c), i + 1 < chars.count, chars[i + 1] == " " {
                say(String(chars[from...i]))
                from = i + 1
            }
            i += 1
        }
        said[id] = from
        link.send(["t": "transcript", "who": "agent", "text": clean(text), "final": false])
    }

    private func message(_ id: String, _ full: String) {
        let from = said[id] ?? 0
        let chars = Array(full)
        if from < chars.count { say(String(chars[from...])) }
        said[id] = chars.count
        lines.append(Line(user: false, text: clean(full)))
        link.send(["t": "transcript", "who": "agent", "text": clean(full), "final": true])
        log("agent: \(full)")
    }

    private func setActivity(_ a: String) {
        var state = a
        if a == "idle" && speaker.speaking { state = "speaking" }
        if a == "thinking" && activity == "building" { state = "building" }
        activity = state
        let phoneState = state == "idle" ? "listening" : state
        link.send(["t": "state", "state": phoneState])
    }

    // MARK: tools

    private func tool(_ name: String, _ args: [String: Any], _ done: @escaping ([String: Any]) -> Void) {
        func reply(_ ok: Bool, _ text: String) {
            done(["success": ok, "contentItems": [["type": "inputText", "text": text]]])
        }
        switch name {
        case "install_app":
            install(args) { ok, text in reply(ok, text) }
        case "open_app":
            let app = args["name"] as? String ?? ""
            link.send(["t": "open", "name": app])
            reply(online, online ? "Opened \(app) on the phone." : "The phone is offline.")
        default:
            reply(false, "Unknown tool \(name)")
        }
    }

    private func install(_ args: [String: Any], _ done: @escaping (Bool, String) -> Void) {
        let name = args["name"] as? String ?? ""
        let title = args["title"] as? String ?? name
        let path = URL(fileURLWithPath: args["path"] as? String ?? "").standardizedFileURL
        guard path.path.hasPrefix(workspace.appendingPathComponent("target").path) || pendingInstallAllowed(path),
              let data = try? Data(contentsOf: path), !data.isEmpty else {
            return done(false, "That program is not in the workspace's build output.")
        }
        guard online else { return done(false, "The phone is offline, so the app could not be installed.") }
        let started = Date()
        if let at = requestAt { timing["install_called_s"] = round(started.timeIntervalSince(at) * 10) / 10 }
        timing["name"] = name
        timing["bytes"] = data.count
        setActivity("building")
        link.send(["t": "install", "name": name, "title": title, "size": data.count, "sha256": sha256(data).hex])
        let chunk = 48 * 1024
        var offset = 0
        while offset < data.count {
            var body = Data()
            withUnsafeBytes(of: UInt32(offset).littleEndian) { body.append(contentsOf: $0) }
            body.append(data[offset..<min(offset + chunk, data.count)])
            link.send(kind: 0x30, body)
            offset += chunk
        }
        link.send(["t": "install_end"])
        log("sent \(name) (\(data.count) bytes)")
        installs[name] = (started, { [weak self] o in
            guard let self else { return }
            let ok = o["ok"] as? Bool ?? false
            let secs = Date().timeIntervalSince(started)
            self.timing["transfer_install_s"] = round(secs * 10) / 10
            if let at = self.requestAt { self.timing["installed_s"] = round(Date().timeIntervalSince(at) * 10) / 10 }
            self.log("installed \(name): \(ok) \(o["error"] ?? "") in \(String(format: "%.1f", secs)) s")
            done(ok, ok ? "Installed \(title) on the phone. It is listed in Programs and survives a restart; the user can tap Open \(title) in the agent."
                     : "The phone could not install it: \(o["error"] as? String ?? "unknown error")")
        })
        DispatchQueue.main.asyncAfter(deadline: .now() + 90) { [weak self] in
            if let (_, d) = self?.installs.removeValue(forKey: name) { d(["ok": false, "error": "the phone did not answer"]) }
        }
    }

    /// Tests may install a program from anywhere under the checkout's build/.
    private func pendingInstallAllowed(_ path: URL) -> Bool {
        CommandLine.arguments.contains("--install") && path.path.hasPrefix(repo.appendingPathComponent("build").path)
    }

    private func finishTiming() {
        let line = (try? JSONSerialization.data(withJSONObject: timing)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        lastTiming = "\(timing["name"] ?? "app"): first open \(timing["opened_s"] ?? "?") s after the request (installed at \(timing["installed_s"] ?? "?") s)"
        log("timing \(line)")
        let file = support.appendingPathComponent("timings.jsonl")
        if let h = try? FileHandle(forWritingTo: file) {
            h.seekToEndOfFile()
            h.write(Data((line + "\n").utf8))
            try? h.close()
        } else {
            try? Data((line + "\n").utf8).write(to: file)
        }
        requestAt = nil
    }

    // MARK: status for tools and tests

    func log(_ s: String) {
        let line = "\(ISO8601DateFormatter().string(from: Date())) \(s)\n"
        NSLog("Violet Agent: %@", s)
        let file = support.appendingPathComponent("agent.log")
        if let h = try? FileHandle(forWritingTo: file) {
            h.seekToEndOfFile()
            h.write(Data(line.utf8))
            try? h.close()
        } else {
            try? Data(line.utf8).write(to: file)
        }
    }

    private func writeStatus() {
        let s: [String: Any] = [
            "relay": relayUp, "online": online, "phone": phone?.name ?? "", "codex": codexState,
            "speech": speechState, "workspace": workspaceState, "activity": activity,
            "last_timing": lastTiming ?? "", "lines": lines.suffix(8).map { ($0.user ? "user: " : "agent: ") + $0.text },
        ]
        if let d = try? JSONSerialization.data(withJSONObject: s, options: .prettyPrinted) {
            try? d.write(to: support.appendingPathComponent("status.json"))
        }
    }
}
