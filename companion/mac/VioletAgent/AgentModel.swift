// Violet Agent's state: the paired phone, speech in and out, Codex, and the
// installs it asks for. Everything runs on the main queue.
import CryptoKit
import Foundation
import SwiftUI
import CoreImage
import AppKit

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
    /// The one-time code this Mac shows for the phone's camera, while pairing.
    @Published var showingCode: String?
    /// Where the direct path to the phone goes, when it is open.
    @Published var directPath: String?
    @Published var localReady = false
    /// The relay this Mac names in its own code: --relay, VIOLET_AGENT_RELAY or the Chariot.
    let relayURL: String = {
        let args = CommandLine.arguments
        if let i = args.firstIndex(of: "--relay"), i + 1 < args.count { return args[i + 1] }
        return ProcessInfo.processInfo.environment["VIOLET_AGENT_RELAY"] ?? "https://agent-2af29451c05342fcbb9db79b4df48ce1.apps.chariots.sh"
    }()
    @Published var lastTiming: String?
    @Published var agentName = "Violet"

    private var saved: Saved
    private let identity: Curve25519.Signing.PrivateKey
    private var link: PhoneLink!
    private var local: LocalCompanion?
    private var localInvite: UUID?
    private var shieldSession: UUID?
    private var started = false
    private let listener = Listener()
    private let speaker = Speaker()
    /// RoseOS's listener and speaker (voice/violet_voice.py); Apple's speech when unavailable.
    private let rose = RoseVoice()
    private var useRose = false
    private var replyIDs: [String: Int] = [:]
    private var replySent: [String: Int] = [:]
    private var nextReply = 1
    private var renamed = false
    private var roseAudible = false
    /// The conversation: fast turns, read-only, never writes code.
    private var codex: Codex!
    /// Apps: writes, builds and installs in the workspace while the talk goes on.
    private var builder: Codex!
    /// The build in progress: what was asked, when, and what the builder did.
    private var building: (request: String, started: Date, progress: [String])?
    private var lastBuild = ""
    private var builderReply = ""
    /// Whether the phone has Python (its hello says); nil until it says.
    private var phonePython: Bool?
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
    /// Answers the phone owes: its list of apps, and removals by name.
    private var appLists: [([String: Any]) -> Void] = []
    private var removals: [String: ([String: Any]) -> Void] = [:]
    private var opens: [String: (UUID, ([String: Any]) -> Void)] = [:]
    private var timing: [String: Any] = [:]
    /// --say TEXT (repeatable): typed requests once online, --say-gap seconds apart (default 10).
    private var pendingSays: [String] = []
    /// --install NAME[|TITLE]=PATH (repeatable): install locally built programs after connecting.
    private var pendingInstalls: [(name: String, title: String, path: String)] = []
    /// --record-mic: the phone's microphone stream, kept as mic.wav (measurements).
    private var recording: Data?
    private var micBytes = 0
    private var micSince = Date()
    private var speechBytes = 0

    init() {
        saved = Store.load()
        identity = Store.identity(saved)
        let fm = FileManager.default
        let profile = ProcessInfo.processInfo.environment["VIOLET_AGENT_PROFILE"] ?? ""
        support = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(profile.isEmpty ? "Violet Agent" : "Violet Agent \(profile)")
        try? fm.createDirectory(at: support, withIntermediateDirectories: true)
        let path = Bundle.main.object(forInfoDictionaryKey: "VioletRepoPath") as? String ?? ""
        // The checkout it was built from, else ~/VioletOS (a worktree may be gone).
        let built = URL(fileURLWithPath: ProcessInfo.processInfo.environment["VIOLET_REPO"] ?? path).standardizedFileURL
        let fallback = URL(fileURLWithPath: NSHomeDirectory() + "/VioletOS")
        repo = fm.fileExists(atPath: built.appendingPathComponent("tools/agent_build.py").path) ? built : fallback
        workspace = repo.appendingPathComponent("build/agent-workspace")
        try? fm.createDirectory(at: workspace, withIntermediateDirectories: true)
        link = PhoneLink(identity: identity) { [weak self] e in self?.on(e) }
        phone = saved.phones.first
        agentName = saved.name ?? "Violet"
        log("Violet Agent started; Mac key \(identity.publicKey.rawRepresentation.hex.prefix(16)); repo \(repo.path)")
    }

    // MARK: start

    func start() {
        guard !started else { return }
        started = true
        let args = CommandLine.arguments
        if let i = args.firstIndex(of: "--unpair"), i >= 0 { saved.phones = []; Store.save(saved); phone = nil }
        for (i, a) in args.enumerated() where a == "--say" && i + 1 < args.count { pendingSays.append(args[i + 1]) }
        if args.contains("--record-mic") { recording = Data() }
        if let i = args.firstIndex(of: "--name"), i + 1 < args.count { rename(args[i + 1]) }
        for (i, a) in args.enumerated() where a == "--install" && i + 1 < args.count {
            if let eq = args[i + 1].firstIndex(of: "=") {
                let spec = args[i + 1]
                let label = spec[..<eq].split(separator: "|", maxSplits: 1)
                guard let first = label.first else { continue }
                let name = String(first)
                let title = label.count == 2 ? String(label[1]) : name.capitalized
                pendingInstalls.append((name, title, String(spec[spec.index(after: eq)...])))
            }
        }
        if let p = phone, p.relay != LocalCompanion.relay { link.connect(p) }
        let key = ProcessInfo.processInfo.environment["VIOLET_SHIELD_KEY"].map { URL(fileURLWithPath: $0) }
            ?? repo.appendingPathComponent("build/shield/companion-signing.pem")
        Shield.shared.start(emulatorKey: key, dataDirectory: support)
        let companion = LocalCompanion(repo: repo, invitation: { [weak self] in self?.inviteEmulator() }) { [weak self] port in
            guard let self else { return }
            self.localReady = port != nil
            self.link.localRelayPort = port
            if let p = self.phone, p.relay == LocalCompanion.relay {
                if port != nil { self.link.connect(p) } else { self.link.stop(); self.relayUp = false }
            }
        }
        local = companion
        companion.start()
        if let i = args.firstIndex(of: "--pair"), i + 1 < args.count, let inv = Invitation(args[i + 1]) { pair(inv) }
        if args.contains("--show-code") { showCode() }
        speaker.onPCM = { [weak self] id, pcm in self?.sendSpeech(id, pcm) }
        speaker.onEnd = { [weak self] id in
            guard let self else { return }
            self.log("spoke utterance \(id): \(self.speechBytes) bytes of speech sent")
            self.speechBytes = 0
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
        } else if !args.contains("--apple-speech") && RoseVoice.available() {
            startRose()
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

    private func startRose() {
        useRose = true
        speechState = "Loading the RoseOS voice (Silero, smart-turn, Nemotron, Parakeet, Phonon)…"
        rose.onReady = { [weak self] voice in
            self?.speechState = "RoseOS voice ready (Phonon \(voice))"
            self?.log("RoseOS voice ready: \(voice)")
        }
        rose.onFailed = { [weak self] why in
            guard let self else { return }
            self.log("RoseOS voice failed: \(why); using the Mac's speech")
            self.useRose = false
            self.listener.start { ok in
                self.speechState = ok ? "Speech recognition on this Mac (RoseOS voice unavailable)" : "Allow Speech Recognition in System Settings › Privacy"
            }
        }
        rose.onTurn = { [weak self] text, hold in
            guard let self else { return }
            self.heard = ""
            if hold { self.log("user: \(text) (holding)"); return }
            self.userTurn(text)
        }
        rose.onPartial = { [weak self] t in self?.heard = t }
        rose.onControl = { [weak self] m in self?.link.send(["t": "audio", "ctl": m]) }
        rose.onPCM = { [weak self] seg, pcm in self?.sendSpeech(seg, pcm) }
        rose.onAudible = { [weak self] on in
            guard let self else { return }
            self.roseAudible = on
            if on { self.setActivity("speaking") } else if self.codex.activeTurn == nil { self.setActivity("idle") }
        }
        rose.onLog = { [weak self] s in self?.log(s) }
        let script = repo.appendingPathComponent("companion/mac/voice/violet_voice.py")
        rose.start(script: script, voice: saved.voice ?? "zoey")
    }

    /// Give the agent a new name (the Mac's setting, shown on the phone).
    func rename(_ name: String) {
        let n = String(name.trimmingCharacters(in: .whitespacesAndNewlines).prefix(24))
        guard !n.isEmpty else { return }
        agentName = n
        saved.name = n
        Store.save(saved)
        renamed = true
        link?.send(["t": "hello", "name": Host.current().localizedName ?? "Mac", "agent_name": n])
        log("the agent is now called \(n)")
    }

    private var instructions: String {
        Self.instructions.replacingOccurrences(of: "You are Violet,", with: "You are \(agentName),")
    }

    private static let instructions = """
    You are Violet, the voice agent of the user's VioletOS phone (a Moto G Pure). You run on their Mac. \
    Everything you write is spoken aloud on the phone, so answer like a person talking: one or two short sentences, \
    no markdown, lists, code, paths or URLs. You never write code or run commands yourself. \
    When the user asks for an app or a change to one, call build_app with a clear description of what to make \
    (say "in Python" when they asked for Python), then say one short line such as "Sure, building it now." \
    The builder works in the background and you keep talking with the user meanwhile: answer questions, chat, \
    take more requests (more changes to the app go to build_app too). If they ask how it is going, call build_status. \
    A message starting with [builder] is a report from the builder, not from the user: give the user the news in \
    one short sentence, for example that the app is ready and they can tap Open. Choose sensible defaults instead of asking. \
    If the user asks to open an app, call open_app. If they want to call you something else, call rename_assistant \
    with the new name and use it from then on. list_apps tells you the apps installed on the phone and the room left. \
    When an install fails for lack of room, offer to remove apps the user no longer wants; call remove_app only for \
    apps the user has agreed to remove, then build_app again if they still want the new app.
    """

    private static let builderInstructions = """
    You build apps for the user's VioletOS phone, following AGENTS.md in this workspace. Requests come from the \
    voice agent the user is talking with. Build the app, install it with install_app, then reply with one short plain \
    sentence saying what you made, or what went wrong. Never ask questions; choose sensible defaults.
    """

    private static let tools: [[String: Any]] = [
        ["type": "function", "name": "build_app",
         "description": "Start building an app, or change one, in the background. Returns at once; a [builder] message arrives when it is installed on the phone.",
         "inputSchema": ["type": "object", "properties": [
            "request": ["type": "string", "description": "What to build or change, in plain words, with any details the user gave"]],
                         "required": ["request"]]],
        ["type": "function", "name": "build_status",
         "description": "How the build in progress is going.",
         "inputSchema": ["type": "object", "properties": [String: Any]()]],
        ["type": "function", "name": "cancel_build",
         "description": "Stop the build in progress.",
         "inputSchema": ["type": "object", "properties": [String: Any]()]],
        ["type": "function", "name": "rename_assistant",
         "description": "Change the name the user calls you. It is saved and shown on the phone.",
         "inputSchema": ["type": "object", "properties": ["name": ["type": "string"]], "required": ["name"]]],
        ["type": "function", "name": "open_app",
         "description": "Open an installed app on the phone's screen.",
         "inputSchema": ["type": "object", "properties": ["name": ["type": "string"]], "required": ["name"]]],
        ["type": "function", "name": "list_apps",
         "description": "The apps installed on the phone (name, title, size) and the room left for more.",
         "inputSchema": ["type": "object", "properties": [String: Any]()]],
        ["type": "function", "name": "remove_app",
         "description": "Remove an installed app from the phone. Only after the user agreed to remove that app.",
         "inputSchema": ["type": "object", "properties": ["name": ["type": "string"]], "required": ["name"]]],
    ]

    private static let builderTools: [[String: Any]] = [
        ["type": "function", "name": "install_app",
         "description": "Install a program built with ./build on the user's phone. Returns once it is installed and listed on the phone.",
         "inputSchema": ["type": "object", "properties": [
            "name": ["type": "string", "description": "The app's folder name"],
            "title": ["type": "string", "description": "The title shown on the phone"],
            "path": ["type": "string", "description": "The path ./build printed"]],
                         "required": ["name", "title", "path"]]],
    ]

    private func startCodex() {
        codex = Codex("talker", workspace: workspace, instructions: instructions, tools: Self.tools, sandbox: "read-only")
        codex.onLog = { [weak self] s in self?.log(s) }
        codex.onDelta = { [weak self] id, d in self?.delta(id, d) }
        codex.onMessage = { [weak self] id, t in self?.message(id, t) }
        codex.onActivity = { [weak self] a in self?.setActivity(a == "building" ? "thinking" : a) }
        codex.onTool = { [weak self] tool, args, done in self?.tool(tool, args, done) }
        codex.start { [weak self] err in
            guard let self else { return }
            self.codexState = err ?? "Codex ready (\(self.codex.model))"
            self.log(self.codexState)
            self.sayPending()
        }
        builder = Codex("builder", workspace: workspace, instructions: Self.builderInstructions, tools: Self.builderTools)
        builder.onLog = { [weak self] s in self?.log(s) }
        builder.onMessage = { [weak self] _, t in self?.builderReply = t }
        builder.onProgress = { [weak self] p in
            guard let self, self.building != nil else { return }
            self.building!.progress.append(p)
            self.log("builder: \(p)")
        }
        builder.onTool = { [weak self] tool, args, done in self?.tool(tool, args, done) }
        builder.onTurnEnd = { [weak self] ok in self?.buildEnded(ok) }
        builder.start { [weak self] err in
            if let err { self?.log("builder: \(err)") }
        }
    }

    // MARK: building in the background

    private func startBuild(_ request: String) -> String {
        guard builder.thread != nil else { return "The builder isn't ready on the Mac yet." }
        if building != nil {
            // More for the build in progress: the builder takes it into its turn.
            building!.request += "; " + request
            builder.say(request)
            log("build: more for the build in progress: \(request)")
            return "Added that to the build in progress; a [builder] message comes when it is done."
        }
        building = (request, Date(), [])
        builderReply = ""
        log("build: \(request)")
        builder.say(request)
        setActivity(activity)
        return "Started. The builder works in the background, usually under a minute; a [builder] message comes when it is installed. Keep talking with the user meanwhile."
    }

    private func buildStatus() -> String {
        guard let b = building else {
            return lastBuild.isEmpty ? "No build is running." : "No build is running. The last one: \(lastBuild)"
        }
        let secs = Int(Date().timeIntervalSince(b.started))
        let recent = b.progress.suffix(3).joined(separator: "; ")
        return "Building for \(secs) s: \(b.request). " + (recent.isEmpty ? "It is still planning." : "Latest steps: \(recent).")
    }

    private func buildEnded(_ ok: Bool) {
        guard let b = building else { return }
        building = nil
        let secs = Int(Date().timeIntervalSince(b.started))
        let reply = builderReply.trimmingCharacters(in: .whitespacesAndNewlines)
        lastBuild = reply.isEmpty ? (ok ? "finished" : "stopped before it finished") : reply
        log("build ended (\(ok ? "done" : "stopped")) in \(secs) s: \(lastBuild)")
        setActivity(activity)
        codex.say("[builder] " + (ok ? "" : "The build stopped. ") + lastBuild)
    }

    /// Process queued installs once online; with --wait-direct, once the direct
    /// path is open (or 20 s have passed), so a test sees a program cross it.
    private func installPending(waited: Double) {
        guard let (name, title, path) = pendingInstalls.first, online else { return }
        if CommandLine.arguments.contains("--wait-direct") && directPath == nil && waited < 20 {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in self?.installPending(waited: waited + 0.5) }
            return
        }
        pendingInstalls.removeFirst()
        let via = directPath.map { "direct path \($0)" } ?? "relay"
        install(["name": name, "title": title, "path": path]) { [weak self] ok, text in
            self?.log("test install \(name): \(ok ? "ok" : "failed") \(text) (via \(via))")
            self?.installPending(waited: 20)
        }
    }

    private func sayPending() {
        guard let s = pendingSays.first, online, codex.thread != nil else { return }
        pendingSays.removeFirst()
        log("typed request: \(s)")
        userTurn(s)
        guard !pendingSays.isEmpty else { return }
        let args = CommandLine.arguments
        let gap = args.firstIndex(of: "--say-gap").flatMap { $0 + 1 < args.count ? Double(args[$0 + 1]) : nil } ?? 10
        DispatchQueue.main.asyncAfter(deadline: .now() + gap) { [weak self] in self?.sayPending() }
    }

    // MARK: pairing

    func pair(_ inv: Invitation) {
        localInvite = nil
        scanning = false
        log("pairing with \(inv.name) through \(inv.relay)")
        link.pair(inv)
    }

    private func inviteEmulator() -> String? {
        guard localReady, phone == nil, !scanning, showingCode == nil else { return nil }
        let id = UUID()
        localInvite = id
        let code = link.invite(relay: LocalCompanion.relay, name: Host.current().localizedName ?? "Mac")
        DispatchQueue.main.asyncAfter(deadline: .now() + 45) { [weak self] in
            guard let self, self.localInvite == id else { return }
            self.localInvite = nil
            self.link.cancelInvite()
            self.relayUp = false
        }
        return code
    }

    /// Show a one-time code for the phone to read with its camera.
    func showCode() {
        localInvite = nil
        let code = link.invite(relay: relayURL, name: Host.current().localizedName ?? "Mac")
        showingCode = code
        log("showing a pairing code for the phone's camera through \(relayURL)")
        // For tests and setups without a person at the Mac: the code, as text and a picture.
        try? code.write(to: support.appendingPathComponent("pairing-code.txt"), atomically: true, encoding: .utf8)
        if let png = qrPNG(code) { try? png.write(to: support.appendingPathComponent("pairing-code.png")) }
    }

    func cancelCode() {
        guard showingCode != nil else { return }
        showingCode = nil
        link.cancelInvite()
        try? FileManager.default.removeItem(at: support.appendingPathComponent("pairing-code.txt"))
        if let p = phone { link.connect(p) }
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
            localInvite = nil
            saved.phones = [p]
            Store.save(saved)
            phone = p
            if showingCode != nil {
                showingCode = nil
                scanning = false
                try? FileManager.default.removeItem(at: support.appendingPathComponent("pairing-code.txt"))
            }
            lines.append(Line(user: false, text: "Paired with \(p.name)."))
            log("paired with \(p.name)")
        case .online(let up):
            if up != online { log(up ? "phone online" : "phone offline") }
            online = up
            if let session = shieldSession { Shield.shared.disconnectAgent(session) }
            shieldSession = up ? UUID() : nil
            if !up {
                // A transfer in flight is lost with the link: fail it now rather than at its timeout.
                for (name, (_, done)) in installs { log("install of \(name) cut off"); done(["ok": false, "error": "the phone went offline during the transfer"]) }
                installs.removeAll()
                phonePython = nil
            }
            if up {
                link.send(["t": "hello", "name": Host.current().localizedName ?? "Mac",
                           "agent_name": agentName, "shield": true])
                sayPending()
                if let i = CommandLine.arguments.firstIndex(of: "--speak"), i + 1 < CommandLine.arguments.count {
                    let line = CommandLine.arguments[i + 1]
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.say(line) }
                }
                if CommandLine.arguments.contains("--unpair-remote") {
                    log("unpairing (--unpair-remote)")
                    unpair()
                }
                installPending(waited: 0)
            }
        case .note(let s):
            log(s)
        case .direct(let addr):
            directPath = addr
        case .mic(let pcm):
            if useRose { rose.feed(pcm) } else { listener.feed(pcm) }
            micBytes += pcm.count
            if Date().timeIntervalSince(micSince) >= 10 {
                log(String(format: "microphone: %.1f KB/s from the phone, peak %.2f", Double(micBytes) / Date().timeIntervalSince(micSince) / 1000, listener.level))
                micBytes = 0
                micSince = Date()
            }
            if recording != nil {
                recording!.append(pcm)
                if recording!.count % (32000 * 5) < pcm.count { saveRecording() }
            }
        case .shield(let data):
            guard let session = shieldSession else { return }
            Shield.shared.receiveAgent(data, session: session) { [weak self] warning in
                DispatchQueue.main.async {
                    guard let self, self.shieldSession == session else { return }
                    self.link.send(kind: 0x32, warning)
                }
            }
        case .json(let o):
            phoneMessage(o)
        }
    }

    private func phoneMessage(_ o: [String: Any]) {
        switch o["t"] as? String {
        case "hello":
            if let py = o["python"] as? Bool { phonePython = py }
            // A phone paired by reading our code is named once it says hello.
            if let name = o["name"] as? String, !name.isEmpty, var p = phone, p.name != name {
                p.name = name
                phone = p
                saved.phones = [p]
                Store.save(saved)
            }
        case "stop":
            log("tapped to interrupt")
            if useRose { rose.stop("tapped") } else { bargeIn() }
        case "audio_report":
            if useRose, let m = o["msg"] as? [String: Any] { rose.client(m) }
        case "installed":
            let name = o["name"] as? String ?? ""
            if let (_, done) = installs.removeValue(forKey: name) {
                done(o)
            }
        case "apps":
            if !appLists.isEmpty { appLists.removeFirst()(o) }
        case "uninstalled":
            let name = o["name"] as? String ?? ""
            log("the phone removed \(name): \(o["ok"] ?? false)")
            removals.removeValue(forKey: name)?(o)
        case "opened":
            let name = o["name"] as? String ?? ""
            opens.removeValue(forKey: name)?.1(o)
            if o["ok"] as? Bool == true, timing["name"] as? String == name, timing["opened_s"] == nil, let at = requestAt {
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
        if renamed {
            // A name changed in the app or by --name: tell Codex with the next turn.
            renamed = false
            codex.say("(Your name is now \(agentName).) " + t)
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
        if useRose {
            let rid = nextReply
            nextReply += 1
            rose.reply(rid, t)
            rose.replyEnd(rid)
            return
        }
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
        speechBytes += pcm.count
        let seconds = Double(pcm.count / 2) / 24000
        playingUntil = max(playingUntil, Date()) + seconds
        // The phone's ring and the relay add about half a second.
        playingUntil = max(playingUntil, Date() + 0.6)
    }

    private func rid(_ item: String) -> Int {
        if let r = replyIDs[item] { return r }
        let r = nextReply
        nextReply += 1
        replyIDs[item] = r
        return r
    }

    /// Speak whole sentences of a reply as they stream in.
    private func delta(_ id: String, _ d: String) {
        if useRose {
            // Rose's speaker cuts clauses and sentences itself.
            rose.reply(rid(id), d.replacingOccurrences(of: "`", with: "").replacingOccurrences(of: "**", with: ""))
            replySent[id, default: 0] += d.count
            texts[id] = (texts[id] ?? "") + d
            link.send(["t": "transcript", "who": "agent", "text": clean(texts[id] ?? ""), "final": false])
            return
        }
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
        if useRose {
            if replySent[id] == nil { rose.reply(rid(id), clean(full)) }
            rose.replyEnd(rid(id))
            lines.append(Line(user: false, text: clean(full)))
            link.send(["t": "transcript", "who": "agent", "text": clean(full), "final": true])
            log("agent: \(full)")
            return
        }
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
        if state == "idle" && (speaker.speaking || (useRose && roseAudible)) { state = "speaking" }
        // While the talker is quiet, the phone shows the build going on (it keeps listening).
        if state == "idle" && building != nil { state = "building" }
        if state == "building" && building == nil && codex.activeTurn == nil { state = "idle" }
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
        case "build_app":
            reply(true, startBuild(args["request"] as? String ?? ""))
        case "build_status":
            reply(true, buildStatus())
        case "cancel_build":
            let was = building != nil
            builder.interrupt()
            reply(true, was ? "Stopped the build." : "No build was running.")
        case "install_app":
            install(args) { ok, text in reply(ok, text) }
        case "rename_assistant":
            let n = args["name"] as? String ?? ""
            rename(n)
            renamed = false
            reply(!n.isEmpty, n.isEmpty ? "No name given." : "Saved. Your name is now \(agentName), and the phone shows it.")
        case "list_apps":
            guard online else { return reply(false, "The phone is offline.") }
            appLists.append { o in
                let apps = (o["apps"] as? [[String: Any]] ?? []).filter { ($0["name"] as? String) != "python" }
                let list = apps.map { "\($0["title"] ?? "") (\($0["name"] ?? ""), \(($0["bytes"] as? Int ?? 0) / 1024) KB)" }
                let free = (o["free"] as? Int ?? 0) / 1024
                let python = (o["python"] as? Bool ?? false) ? "Python is installed." : "Python is not installed yet (a Python app needs about 5 MB once)."
                reply(true, (list.isEmpty ? "No apps are installed." : "Installed: " + list.joined(separator: "; ") + ".") + " Room left: \(free) KB. " + python)
            }
            link.send(["t": "apps"])
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
                guard let self, !self.appLists.isEmpty else { return }
                self.appLists.removeFirst()(["apps": [], "free": 0])
            }
        case "remove_app":
            let app = args["name"] as? String ?? ""
            guard online else { return reply(false, "The phone is offline.") }
            removals[app] = { o in
                let ok = o["ok"] as? Bool ?? false
                reply(ok, ok ? "Removed \(app) from the phone." : "Could not remove \(app): \(o["error"] as? String ?? "no answer")")
            }
            link.send(["t": "uninstall", "name": app])
            log("removing \(app) from the phone")
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
                if let d = self?.removals.removeValue(forKey: app) { d(["ok": false, "error": "the phone did not answer"]) }
            }
        case "open_app":
            let app = args["name"] as? String ?? ""
            guard online else { return reply(false, "The phone is offline.") }
            guard opens[app] == nil else { return reply(false, "An open request for \(app) is already pending.") }
            let request = UUID()
            opens[app] = (request, { answer in
                let ok = answer["ok"] as? Bool == true
                let reason = answer["error"] as? String ?? "the device refused the launch"
                reply(ok, ok ? "Opened \(app) on the phone." : "Could not open \(app): \(reason)")
            })
            link.send(["t": "open", "name": app])
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
                guard let self, self.opens[app]?.0 == request else { return }
                self.opens.removeValue(forKey: app)?.1(["ok": false, "error": "the phone did not answer"])
            }
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
        let python = data.starts(with: Data("#!/bin/python3".utf8))
        timing["language"] = python ? "python" : "rust"
        let finish: (Bool, String) -> Void = { [weak self] ok, error in
            guard let self else { return }
            let secs = Date().timeIntervalSince(started)
            self.timing["transfer_install_s"] = round(secs * 10) / 10
            if let at = self.requestAt { self.timing["installed_s"] = round(Date().timeIntervalSince(at) * 10) / 10 }
            self.log("installed \(name): \(ok) \(error) in \(String(format: "%.1f", secs)) s")
            done(ok, ok ? "Installed \(title) on the phone. It has an icon on the phone's Home screen and survives a restart; the user can tap Open \(title) in the agent."
                     : "The phone could not install it: \(error)")
        }
        let app = { [weak self] in
            self?.send(name: name, title: title, data: data, timeout: 90) { ok, error in finish(ok, error) }
        }
        if python && phonePython == nil {
            // Not known yet (the phone's hello may still be on its way): ask it.
            appLists.append { [weak self] o in
                self?.phonePython = o["python"] as? Bool
                self?.install(args, done)
            }
            link.send(["t": "apps"])
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
                guard let self, self.phonePython == nil, !self.appLists.isEmpty else { return }
                self.appLists.removeFirst()(["python": false])
            }
            return
        }
        if python && phonePython != true {
            installPython { ok, error in
                ok ? app() : finish(false, "Python could not be installed on the phone: \(error)" +
                                    (error.contains("no room") ? ". The phone's app space is full: say so; the voice agent can offer to remove apps." : ""))
            }
        } else {
            app()
        }
    }

    /// Python for the phone (tools/build_python_package.py), installed once as the "python" package.
    private func installPython(_ done: @escaping (Bool, String) -> Void) {
        let package = repo.appendingPathComponent("build/python-package.vgz")
        log("the phone has no Python yet: installing it first")
        DispatchQueue.global().async {
            if !FileManager.default.fileExists(atPath: package.path) {
                let p = Process()
                p.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
                p.arguments = [self.repo.appendingPathComponent("tools/build_python_package.py").path, package.path]
                p.standardOutput = FileHandle.nullDevice
                try? p.run()
                p.waitUntilExit()
            }
            let data = try? Data(contentsOf: package)
            DispatchQueue.main.async {
                guard let data, !data.isEmpty else { return done(false, "the Python package is missing on the Mac (tools/build_python_package.py)") }
                let started = Date()
                self.send(name: "python", title: "Python", data: data, timeout: 600) { ok, error in
                    self.log("python package: \(ok ? "installed" : "failed \(error)") in \(Int(Date().timeIntervalSince(started))) s")
                    if ok { self.phonePython = true }
                    done(ok, error)
                }
            }
        }
    }

    /// Send a program (or a package) to the phone; `done` gets the phone's answer.
    private func send(name: String, title: String, data: Data, timeout: Double, _ done: @escaping (Bool, String) -> Void) {
        // Raw DEFLATE halves a Rust program; the phone's link is the slow part.
        let packed = (try? (data as NSData).compressed(using: .zlib) as Data) ?? data
        let deflated = packed.count < data.count * 9 / 10
        let wire = deflated ? packed : data
        link.send(["t": "install", "name": name, "title": title, "size": wire.count, "bytes": data.count,
                   "encoding": deflated ? "deflate" : "none", "sha256": sha256(data).hex])
        let chunk = 48 * 1024
        var offset = 0
        while offset < wire.count {
            var body = Data()
            withUnsafeBytes(of: UInt32(offset).littleEndian) { body.append(contentsOf: $0) }
            body.append(wire[offset..<min(offset + chunk, wire.count)])
            link.send(kind: 0x30, body)
            offset += chunk
        }
        link.send(["t": "install_end"])
        log("sent \(name) (\(data.count) bytes, \(wire.count) on the wire)")
        installs[name] = (Date(), { o in
            done(o["ok"] as? Bool ?? false, o["error"] as? String ?? "unknown error")
        })
        DispatchQueue.main.asyncAfter(deadline: .now() + timeout) { [weak self] in
            if let (_, d) = self?.installs.removeValue(forKey: name) { d(["ok": false, "error": "the phone did not answer"]) }
        }
    }

    private func saveRecording() {
        guard let pcm = recording else { return }
        var wav = Data("RIFF".utf8)
        func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { wav.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { wav.append(contentsOf: $0) } }
        u32(UInt32(36 + pcm.count)); wav.append(Data("WAVEfmt ".utf8)); u32(16); u16(1); u16(1); u32(16000); u32(32000); u16(2); u16(16)
        wav.append(Data("data".utf8)); u32(UInt32(pcm.count)); wav.append(pcm)
        try? wav.write(to: support.appendingPathComponent("mic.wav"))
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
            "direct": directPath ?? "", "showing_code": showingCode != nil,
            "speech": speechState, "workspace": workspaceState, "activity": activity,
            "last_timing": lastTiming ?? "", "lines": lines.suffix(8).map { ($0.user ? "user: " : "agent: ") + $0.text },
        ]
        if let d = try? JSONSerialization.data(withJSONObject: s, options: .prettyPrinted) {
            try? d.write(to: support.appendingPathComponent("status.json"))
        }
    }
}

/// A QR code as a PNG, a few pixels per module, with its quiet zone.
func qrPNG(_ text: String, scale: CGFloat = 12) -> Data? {
    guard let f = CIFilter(name: "CIQRCodeGenerator") else { return nil }
    f.setValue(Data(text.utf8), forKey: "inputMessage")
    f.setValue("M", forKey: "inputCorrectionLevel")
    guard let img = f.outputImage?.transformed(by: CGAffineTransform(scaleX: scale, y: scale)) else { return nil }
    let rep = NSBitmapImageRep(ciImage: img)
    return rep.representation(using: .png, properties: [:])
}
