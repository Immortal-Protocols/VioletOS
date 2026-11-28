// Violet Agent's state: the paired phone and its encrypted link, the agent
// (Rose, RoseAgent.swift: her voice, conversation, tasks, identity and life),
// the builder that makes the apps she is asked for, and their installs.
// Everything runs on the main queue.
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
    enum CodexSignIn { case checking, required, inTerminal, signedIn }

    @Published var relayUp = false
    @Published var online = false
    @Published var phone: PairedPhone?
    @Published var lines: [Line] = []
    @Published var heard = ""
    @Published var codexState = "Starting the builder…"
    @Published private(set) var codexSignIn: CodexSignIn = .checking
    @Published var speechState = "Starting Rose…"
    /// Rose's own dashboard (her life, journal, tasks and transcripts) on this Mac.
    @Published var dashboard: String?
    /// Rose's tasks (builds among them), newest last, as her bus reports them.
    @Published var tasks: [Int: [String: Any]] = [:]
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

    @Published var locomoStatus = "Downloads Locomo Music and prepares its local models."
    @Published private(set) var locomoApp: URL?
    @Published private(set) var locomoInstalling = false
    private var locomoJob: (id: UUID, phone: String, status: String, song: LocomoPlugin.Song?)?
    @Published private(set) var locomoRunning = false
    private var locomoRegistration: URL { support.appendingPathComponent("locomo-plugin") }

    private var saved: Saved
    private let identity: Curve25519.Signing.PrivateKey
    private var link: PhoneLink!
    private var local: LocalCompanion?
    private var localInvite: UUID?
    private var shieldSession: UUID?
    private var started = false
    /// The agent: Rose, from RoseOS.
    private let rose = RoseAgent()
    private var roseReady = false
    private let speechEnabled = !CommandLine.arguments.contains("--no-speech")
    private let codexHome: URL
    private var login: CodexLogin?
    private var terminalLogin: (command: CodexLogin.TerminalCommand, deadline: Date, restart: Bool)?
    /// Apps: Codex writes, builds and installs them in the workspace, for a task Rose started.
    private var builder: Codex!
    /// The build Rose asked for: her task id, when it started, whether she stopped it.
    private var building: (task: Int, started: Date, cancelled: Bool)?
    private var builderReply = ""
    /// Whether the phone has Python (its hello says); nil until it says.
    private var phonePython: Bool?
    let support: URL
    let repo: URL
    let workspace: URL
    // Installs and the request they answer, for timing.
    private var requestAt: Date?
    private var request = ""
    private var installs: [String: (Date, ([String: Any]) -> Void)] = [:]
    /// Answers the phone owes: its list of apps, and removals by name.
    private var appLists: [([String: Any]) -> Void] = []
    private var removals: [String: ([String: Any]) -> Void] = [:]
    private var opens: [String: (UUID, ([String: Any]) -> Void)] = [:]
    /// Rose's phone commands (screenshots, touches, opening apps) waiting for the phone's answer.
    private var phoneCalls: [Int: ([String: Any]) -> Void] = [:]
    private var nextPhoneCall = 1
    private var timing: [String: Any] = [:]
    /// --say TEXT (repeatable): typed requests once online, --say-gap seconds apart (default 10).
    private var pendingSays: [String] = []
    /// --say-after-install TEXT: a typed turn once an app (not Python) is installed (tests).
    private var afterInstall: [String] = []
    /// --install NAME[|TITLE]=PATH (repeatable): install locally built programs after connecting.
    private var pendingInstalls: [(name: String, title: String, path: String)] = []
    /// --record-mic: the phone's microphone stream, kept as mic.wav (measurements).
    private var recording: Data?
    private var micBytes = 0
    private var micSince = Date()

    init() {
        saved = Store.load()
        identity = Store.identity(saved)
        let fm = FileManager.default
        let profile = ProcessInfo.processInfo.environment["VIOLET_AGENT_PROFILE"] ?? ""
        let applicationSupport = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        support = applicationSupport.appendingPathComponent(profile.isEmpty ? "Violet Agent" : "Violet Agent \(profile)")
        codexHome = applicationSupport.appendingPathComponent("Violet Agent/Codex")
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
        for (i, a) in args.enumerated() where a == "--say-after-install" && i + 1 < args.count { afterInstall.append(args[i + 1]) }
        if args.contains("--record-mic") { recording = Data() }
        if let i = args.firstIndex(of: "--name"), i + 1 < args.count { agentName = args[i + 1]; saved.name = agentName; Store.save(saved) }
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
        if let i = args.firstIndex(of: "--locomo-app"), i + 1 < args.count {
            enableLocomo(URL(fileURLWithPath: args[i + 1]))
        } else if FileManager.default.fileExists(atPath: locomoRegistration.path),
                  FileManager.default.fileExists(atPath: LocomoSetup.installedApp.path) {
            enableLocomo(LocomoSetup.installedApp)
        }
        speechState = speechEnabled ? "Rose is checking ChatGPT sign-in…" : "No agent (--no-speech)"
        prepareWorkspace()
        startBuilder()
        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            self?.checkTerminalLogin()
            self?.writeStatus()
        }
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

    // MARK: the agent (Rose)

    /// Rose keeps her state with this profile: her name and voice, her life and journal, transcripts.
    var roseProfile: URL { support.appendingPathComponent("rose") }

    private func startRose(login: CodexLogin) {
        rose.onBus = { [weak self] m in self?.bus(m) }
        rose.onControl = { [weak self] m in self?.link.send(["t": "audio", "ctl": m]) }
        rose.onPCM = { [weak self] seg, pcm in self?.sendSpeech(seg, pcm) }
        rose.onCall = { [weak self] id, name, args in self?.call(id, name, args) }
        rose.onBuild = { [weak self] task, request in self?.startBuild(task, request) }
        rose.onBuildUpdate = { [weak self] task, instruction in
            guard let self, self.building?.task == task else { return }
            self.log("build \(task): more from the owner: \(instruction)")
            self.builder.say(instruction)
        }
        rose.onBuildCancel = { [weak self] task in
            guard let self, self.building?.task == task else { return }
            self.building?.cancelled = true
            self.builder.interrupt()
        }
        rose.onDashboard = { [weak self] url in self?.dashboard = url; self?.log("Rose's dashboard: \(url)") }
        rose.onFailed = { [weak self] why in
            guard let self else { return }
            self.roseReady = false
            self.speechState = why
            self.log("Rose: \(why)")
        }
        speechState = "Starting Rose (her voice models and Codex)…"
        if rose.start(repo: repo, profile: roseProfile, name: agentName, voice: saved.voice,
                      log: support.appendingPathComponent("rose.log"), login: login) {
            rose.phone(online: online)
        }
    }

    /// A typed turn from the Mac window, heard by Rose as if spoken.
    func userTurn(_ t: String) {
        guard roseReady else { return log("typed while Rose is not ready: \(t)") }
        rose.text(t)
    }

    /// Give the agent a new name from the Mac window: Rose keeps it, and says it everywhere.
    func rename(_ name: String) {
        let n = String(name.trimmingCharacters(in: .whitespacesAndNewlines).prefix(24))
        guard !n.isEmpty else { return }
        rose.rename(n)
    }

    /// An event on Rose's bus.
    private func bus(_ m: [String: Any]) {
        switch m["kind"] as? String {
        case "ready":
            roseReady = true
            speechState = "Rose is listening (ready in \(m["seconds"] ?? "?") s)"
            log("Rose ready")
            sayPending()
        case "status":
            setActivity(m["state"] as? String ?? "listening")
        case "rose_audio_start":
            setActivity("speaking")
        case "partial":
            heard = m["text"] as? String ?? ""
        case "user_turn":
            let t = m["text"] as? String ?? ""
            heard = ""
            lines.append(Line(user: true, text: t))
            link.send(["t": "transcript", "who": "user", "text": t, "final": true])
            log("user: \(t)")
            if requestAt == nil || activity == "listening" {
                requestAt = Date()
                request = t
                timing = ["request": t, "at": ISO8601DateFormatter().string(from: Date())]
            }
        case "rose_partial":
            link.send(["t": "transcript", "who": "agent", "text": m["text"] as? String ?? "", "final": false])
        case "rose_turn":
            let t = m["text"] as? String ?? ""
            lines.append(Line(user: false, text: t))
            link.send(["t": "transcript", "who": "agent", "text": t, "final": true])
            log("agent: \(t)\((m["interrupted"] as? Bool ?? false) ? " (interrupted)" : "")")
        case "task":
            if let t = m["task"] as? [String: Any], let id = t["id"] as? Int {
                let status = t["status"] as? String ?? ""
                if tasks[id]?["status"] as? String != status {
                    log("task \(id) (\(t["kind"] ?? "")): \(status) \((t["result"] as? String ?? "").prefix(160))")
                }
                tasks[id] = t
            }
        case "assistant_name":
            let n = m["name"] as? String ?? agentName
            agentName = n
            saved.name = n
            Store.save(saved)
            link.send(["t": "hello", "name": Host.current().localizedName ?? "Mac", "agent_name": n, "shield": true])
            log("the agent is now called \(n)")
        case "voice":
            saved.voice = m["voice"] as? String
            Store.save(saved)
        case "barge", "speech_start", "turn_end", "warning":
            log("Rose \(m["kind"] ?? ""): \(m["stage"] ?? m["text"] ?? m["message"] ?? "") \(m["reason"] ?? "")")
        case "error":
            log("Rose: \(m["message"] ?? "")")
            if !roseReady { speechState = "Rose could not start: \(m["message"] ?? "")" }
        default:
            break
        }
    }

    // MARK: building the apps Rose is asked for

    private static let builderInstructions = """
    You build apps for the user's VioletOS phone, following AGENTS.md in this workspace. Requests come from the \
    voice agent the user is talking with. Build the app, install it with install_app, then reply with one short plain \
    sentence saying what you made and that it is on the phone, or what went wrong. Never ask questions; choose sensible defaults.
    """

    private static let builderTools: [[String: Any]] = [
        ["type": "function", "name": "install_app",
         "description": "Install a program built with ./build on the user's phone. Returns once it is installed and listed on the phone.",
         "inputSchema": ["type": "object", "properties": [
            "name": ["type": "string", "description": "The app's folder name"],
            "title": ["type": "string", "description": "The title shown on the phone"],
            "path": ["type": "string", "description": "The path ./build printed"]],
                         "required": ["name", "title", "path"]]],
    ]

    func signInToCodex() {
        if let terminalLogin {
            do {
                try terminalLogin.command.cancel()
                self.terminalLogin?.restart = true
                codexState = "Restarting ChatGPT sign-in…"
            } catch {
                codexState = "Could not restart sign-in: \(error.localizedDescription)"
            }
            return
        }
        guard codexSignIn == .required, let login else { return }
        do {
            let command = try login.terminalCommand()
            terminalLogin = (command, Date().addingTimeInterval(15 * 60), false)
            codexSignIn = .inTerminal
            codexState = "Complete ChatGPT sign-in in Terminal."
            let terminal = URL(fileURLWithPath: "/System/Applications/Utilities/Terminal.app")
            NSWorkspace.shared.open([command.script], withApplicationAt: terminal, configuration: .init()) { [weak self] _, error in
                guard let error else { return }
                DispatchQueue.main.async {
                    guard self?.terminalLogin?.command.directory == command.directory else { return }
                    self?.finishTerminalLogin(error: "Could not open Terminal: \(error.localizedDescription)")
                }
            }
        } catch {
            codexState = "Could not start sign-in: \(error.localizedDescription)"
        }
    }

    private func checkTerminalLogin() {
        guard let terminalLogin else { return }
        if let status = terminalLogin.command.exitStatus {
            finishTerminalLogin(error: status == 0 ? nil : "Sign-in did not finish. Try again.")
            if status != 0 && terminalLogin.restart { signInToCodex() }
        } else if Date() >= terminalLogin.deadline {
            try? terminalLogin.command.cancel()
            self.terminalLogin?.deadline = .distantFuture
            codexState = "Closing the expired Terminal sign-in…"
        }
    }

    private func finishTerminalLogin(error: String?) {
        terminalLogin?.command.cleanUp()
        terminalLogin = nil
        if let error {
            codexSignIn = .required
            codexState = error
        } else {
            startBuilder()
        }
    }

    private func startBuilder() {
        codexSignIn = .checking
        codexState = "Checking ChatGPT sign-in…"
        guard let binary = Codex.binary() else {
            codexState = "Install Codex CLI 0.167.1 or later, then reopen Violet Agent."
            if speechEnabled { speechState = "Rose needs Codex CLI" }
            return
        }
        let login = CodexLogin(home: codexHome, binary: binary)
        do { try login.prepare() } catch {
            codexState = "Could not prepare Codex sign-in: \(error.localizedDescription)"
            if speechEnabled { speechState = "Rose needs a writable Codex directory" }
            return
        }
        self.login = login
        let builder = Codex("builder", workspace: workspace, instructions: Self.builderInstructions, tools: Self.builderTools)
        self.builder = builder
        builder.onLog = { [weak self] s in self?.log(s) }
        builder.onMessage = { [weak self] _, t in self?.builderReply = t }
        builder.onProgress = { [weak self] p in
            guard let self, let b = self.building else { return }
            self.log("builder: \(p)")
            self.rose.buildProgress(b.task, p)
        }
        builder.onTool = { [weak self] tool, args, done in
            guard let self, tool == "install_app" else {
                return done(["success": false, "contentItems": [["type": "inputText", "text": "Unknown tool \(tool)"]]])
            }
            if let b = self.building { self.rose.buildProgress(b.task, "Installing \(args["title"] ?? "the app") on the phone.") }
            self.install(args) { ok, text in done(["success": ok, "contentItems": [["type": "inputText", "text": text]]]) }
        }
        builder.onTurnEnd = { [weak self] ok in self?.buildEnded(ok) }
        builder.start(login: login, account: { [weak self] signedIn in
            guard let self else { return }
            self.codexSignIn = signedIn ? .signedIn : .required
            guard self.speechEnabled else { return }
            if signedIn {
                self.startRose(login: login)
            } else {
                self.speechState = "Rose is waiting for ChatGPT sign-in"
            }
        }, ready: { [weak self, weak builder] error in
            guard let self, let builder else { return }
            if let error {
                self.codexState = error
                self.log("builder: \(error)")
                if self.speechEnabled, self.codexSignIn == .checking {
                    self.speechState = "Rose could not check ChatGPT sign-in"
                }
                builder.stop()
            } else {
                self.codexState = "Builder ready (\(builder.model))"
            }
        })
    }

    /// Rose started a build task: the builder takes it, and reports back to her task.
    private func startBuild(_ task: Int, _ request: String) {
        guard builder.thread != nil else {
            return rose.buildDone(task, status: "failed", result: "The builder on the Mac isn't ready yet.")
        }
        if building != nil {
            builder.say(request)
            return
        }
        building = (task, Date(), false)
        builderReply = ""
        log("build \(task): \(request)")
        builder.say(request)
    }

    private func buildEnded(_ ok: Bool) {
        guard let b = building else { return }
        building = nil
        let reply = builderReply.trimmingCharacters(in: .whitespacesAndNewlines)
        let status = b.cancelled ? "cancelled" : ok ? "done" : "failed"
        let result = b.cancelled ? "Stopped the build." : reply.isEmpty ? (ok ? "The app is built." : "The build stopped before it finished.") : reply
        log("build \(b.task) \(status) in \(Int(Date().timeIntervalSince(b.started))) s: \(result)")
        rose.buildDone(b.task, status: status, result: result)
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
        guard let s = pendingSays.first, online, roseReady else { return }
        pendingSays.removeFirst()
        log("typed request: \(s)")
        rose.text(s)
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
            rose.phone(online: up)
            if up {
                link.send(["t": "hello", "name": Host.current().localizedName ?? "Mac",
                           "agent_name": agentName, "shield": true, "locomo": locomoApp != nil])
                sayPending()
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
            rose.feed(pcm)
            micBytes += pcm.count
            if Date().timeIntervalSince(micSince) >= 10 {
                log(String(format: "microphone: %.1f KB/s from the phone", Double(micBytes) / Date().timeIntervalSince(micSince) / 1000))
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

    func setupLocomo() {
        guard !locomoInstalling, locomoApp == nil else { return }
        locomoInstalling = true
        locomoStatus = "Downloading and verifying Locomo Music…"
        Task { @MainActor in
            defer { locomoInstalling = false }
            do {
                let app = try await LocomoSetup.installApp()
                if LocomoPlugin.encoder == nil {
                    locomoStatus = "Installing ffmpeg for phone playback…"
                    try await LocomoSetup.installEncoder()
                }
                locomoStatus = "Preparing Locomo’s music models…"
                try await LocomoSetup.prepareModels(app) { self.locomoStatus = $0 }
                try Data().write(to: locomoRegistration, options: .atomic)
                enableLocomo(app)
            } catch {
                locomoStatus = error.localizedDescription
            }
        }
    }

    func removeLocomo() {
        guard !locomoRunning else { return }
        do {
            if FileManager.default.fileExists(atPath: locomoRegistration.path) {
                try FileManager.default.removeItem(at: locomoRegistration)
            }
            locomoApp = nil
            locomoJob = nil
            locomoStatus = "Downloads Locomo Music and prepares its local models."
            link.send(["t": "locomo_available", "enabled": false])
        } catch {
            locomoStatus = error.localizedDescription
        }
    }

    private func enableLocomo(_ url: URL) {
        do {
            try LocomoPlugin.verify(url)
            locomoApp = url
            locomoStatus = "Cool Jazz is ready to request from Locomo Music on the paired device."
            link.send(["t": "locomo_available", "enabled": true])
        } catch {
            locomoStatus = error.localizedDescription
        }
    }

    private func receiveLocomo(_ o: [String: Any]) {
        guard let raw = o["id"] as? String, let id = UUID(uuidString: raw), let phone else { return }
        func reply(_ message: String, failed: Bool = true) {
            link.send(["t": "locomo_status", "id": raw, "message": message, "done": failed])
        }
        if let job = locomoJob, job.id == id, job.phone == phone.key {
            if let song = job.song { sendLocomo(song, id: raw) }
            else { reply(job.status, failed: !locomoRunning) }
            return
        }
        guard o["resume"] as? Bool != true else { return reply("The Mac no longer has that song request.") }
        guard !locomoRunning else { return reply("Locomo is already making a song.") }
        guard let app = locomoApp else { return reply("Enable Locomo in Violet Agent on the Mac.") }
        let track = (o["track"] as? String).flatMap(UUID.init(uuidString:))
        let options = LocomoPlugin.Options(o)
        guard track != nil || (options != nil && o["track"] == nil) else { return reply("Locomo request options are invalid.") }
        locomoRunning = true
        locomoJob = (id, phone.key, "Contacting Locomo Music…", nil)
        reply("Contacting Locomo Music…", failed: false)
        if track == nil, let options { log("locomo: requested \(options.station) song") }
        Task { @MainActor in
            var status = "Locomo request finished."
            func progress(_ message: String) {
                status = message
                self.locomoJob?.status = message
                self.locomoStatus = message
                self.log("locomo: \(message)")
                if self.phone?.key == phone.key { reply(message, failed: false) }
            }
            defer { locomoRunning = false }
            do {
                let song: LocomoPlugin.Song?
                if let track {
                    song = try await Task.detached(priority: .utility) { try LocomoPlugin.read(track) }.value
                } else if let options {
                    song = try await LocomoPlugin.run(app: app, options: options, status: progress)
                } else {
                    return
                }
                locomoJob?.song = song
                if let song {
                    locomoStatus = "Ready to play \(song.title)"
                    log("locomo: delivering \(song.title) (\(song.audio.count) MP3 bytes)")
                    if self.phone?.key == phone.key { sendLocomo(song, id: raw) }
                } else if self.phone?.key == phone.key {
                    reply(status)
                }
            } catch {
                locomoJob?.status = error.localizedDescription
                locomoStatus = error.localizedDescription
                log("locomo: \(error.localizedDescription)")
                if self.phone?.key == phone.key { reply(error.localizedDescription) }
            }
        }
    }

    private func receiveLocomoBrowse(_ message: [String: Any]) {
        guard let request = message["request"] as? UInt32, let phone else { return }
        guard let app = locomoApp else {
            link.send(["t": "locomo_library", "request": request, "tracks": [], "more": false,
                       "error": "Enable Locomo Music on the Mac."])
            return
        }
        let query = message["query"] as? String ?? ""
        let page = message["page"] as? Int ?? 0
        let favorites = message["favorites"] as? Bool == true
        Task { @MainActor in
            do {
                let (entries, more) = try await Task.detached(priority: .utility) {
                    try LocomoPlugin.verify(app)
                    return try LocomoPlugin.browse(query: query, page: page, favorites: favorites)
                }.value
                let bytes = try JSONEncoder().encode(entries)
                let tracks = try JSONSerialization.jsonObject(with: bytes)
                guard self.phone?.key == phone.key, self.locomoApp == app else { return }
                link.send(["t": "locomo_library", "request": request, "tracks": tracks, "more": more])
            } catch {
                guard self.phone?.key == phone.key else { return }
                link.send(["t": "locomo_library", "request": request, "tracks": [], "more": false,
                           "error": error.localizedDescription])
            }
        }
    }

    private func sendLocomo(_ song: LocomoPlugin.Song, id: String) {
        link.send(["t": "locomo_track", "id": id, "title": song.title, "durationMs": song.durationMs,
                   "station": song.station, "size": song.audio.count, "sha256": sha256(song.audio).hex])
        for offset in stride(from: 0, to: song.audio.count, by: 48 * 1024) {
            var body = Data()
            withUnsafeBytes(of: UInt32(offset).littleEndian) { body.append(contentsOf: $0) }
            body.append(song.audio[offset..<min(offset + 48 * 1024, song.audio.count)])
            link.send(kind: 0x33, body)
        }
        link.send(["t": "locomo_end", "id": id])
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
        case "locomo_control":
            guard let app = locomoApp, let peer = phone?.key,
                  let action = o["control"] as? String, ["stop", "disarm"].contains(action) else { return }
            Task { @MainActor in
                let message: String
                do {
                    message = try await LocomoPlugin.control(app: app, action: action)
                } catch {
                    message = error.localizedDescription
                }
                if self.phone?.key == peer {
                    self.link.send(["t": "locomo_control_status", "message": message])
                }
            }
        case "locomo_browse":
            receiveLocomoBrowse(o)
        case "locomo_make":
            receiveLocomo(o)
        case "stop":
            log("tapped to interrupt")
            rose.interrupt()
        case "audio_report":
            if let m = o["msg"] as? [String: Any] {
                if m["type"] as? String == "stopped" { log("the phone's player stopped: \(m)") }
                rose.client(m)
            }
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
        case "phone_result":
            if let id = o["id"] as? Int { phoneCalls.removeValue(forKey: id)?(o) }
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

    // MARK: Rose's calls for the phone

    /// open_app, list_apps, remove_app from Rose's conversation, answered from the phone.
    private func call(_ id: Int, _ name: String, _ args: [String: Any]) {
        let answer: ([String: Any]) -> Void = { [weak self] result in self?.rose.answer(id, result) }
        guard online else { return answer(["ok": false, "reason": "The phone is offline."]) }
        switch name {
        case "list_apps":
            appLists.append { o in
                let apps = (o["apps"] as? [[String: Any]] ?? []).filter { ($0["name"] as? String) != "python" }
                answer(["ok": true, "apps": apps.map { ["name": $0["name"] ?? "", "title": $0["title"] ?? "",
                                                         "kilobytes": ($0["bytes"] as? Int ?? 0) / 1024] },
                        "free_kilobytes": (o["free"] as? Int ?? 0) / 1024, "python_installed": o["python"] as? Bool ?? false])
            }
            link.send(["t": "apps"])
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
                guard let self, !self.appLists.isEmpty else { return }
                self.appLists.removeFirst()(["apps": [], "free": 0])
            }
        case "remove_app":
            let app = args["name"] as? String ?? ""
            removals[app] = { o in
                let ok = o["ok"] as? Bool ?? false
                answer(ok ? ["ok": true, "removed": app] : ["ok": false, "reason": o["error"] as? String ?? "no answer"])
            }
            link.send(["t": "uninstall", "name": app])
            log("removing \(app) from the phone")
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
                if let d = self?.removals.removeValue(forKey: app) { d(["ok": false, "error": "the phone did not answer"]) }
            }
        case "open_app":
            let app = args["name"] as? String ?? ""
            guard opens[app] == nil else { return answer(["ok": false, "reason": "An open request for \(app) is already pending."]) }
            let request = UUID()
            opens[app] = (request, { o in
                let ok = o["ok"] as? Bool == true
                answer(ok ? ["ok": true, "opened": app] : ["ok": false, "reason": o["error"] as? String ?? "the phone refused the launch"])
            })
            link.send(["t": "open", "name": app])
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
                guard let self, self.opens[app]?.0 == request else { return }
                self.opens.removeValue(forKey: app)?.1(["ok": false, "error": "the phone did not answer"])
            }
        case "phone":
            let id = nextPhoneCall
            nextPhoneCall += 1
            phoneCalls[id] = { o in answer(o["result"] as? [String: Any] ?? ["status": "failed"]) }
            link.send(["t": "phone", "id": id, "action": args["action"] as? String ?? "", "args": args["args"] ?? [String: Any]()])
            DispatchQueue.main.asyncAfter(deadline: .now() + 20) { [weak self] in
                self?.phoneCalls.removeValue(forKey: id)?(["result": ["status": "timeout", "reason": "the phone did not answer"]])
            }
        default:
            answer(["ok": false, "reason": "Unknown call \(name)"])
        }
    }

    private func sendSpeech(_ id: UInt32, _ pcm: Data) {
        var body = Data()
        withUnsafeBytes(of: id.littleEndian) { body.append(contentsOf: $0) }
        body.append(pcm)
        link.send(kind: 0x21, body)
    }

    /// Rose's state, shown on the phone: listening, thinking, speaking, or building while she listens.
    private func setActivity(_ a: String) {
        var state = a == "idle" ? "listening" : a
        if state == "listening" && building != nil { state = "building" }
        activity = state
        link.send(["t": "state", "state": state])
    }

    private func install(_ args: [String: Any], _ done: @escaping (Bool, String) -> Void) {
        let name = args["name"] as? String ?? ""
        let title = args["title"] as? String ?? name
        let path = URL(fileURLWithPath: args["path"] as? String ?? "").standardizedFileURL
        guard path.path.hasPrefix(workspace.appendingPathComponent("target").path) || pendingInstallAllowed(path),
              let data = try? Data(contentsOf: path), !data.isEmpty else {
            return done(false, "That program is not in the workspace's build output.")
        }
        guard online else {
            // The link can drop for a moment (a network change, a relay hiccup): wait for it before giving up.
            let deadline = (args["_wait_until"] as? Date) ?? Date().addingTimeInterval(20)
            guard Date() < deadline else { return done(false, "The phone is offline, so the app could not be installed.") }
            log("install of \(name) waits for the phone to come back")
            var retry = args
            retry["_wait_until"] = deadline
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in self?.install(retry, done) }
            return
        }
        let started = Date()
        if let at = requestAt { timing["install_called_s"] = round(started.timeIntervalSince(at) * 10) / 10 }
        timing["name"] = name
        timing["bytes"] = data.count
        let python = data.starts(with: Data("#!/bin/python3".utf8))
        timing["language"] = python ? "python" : "rust"
        let finish: (Bool, String) -> Void = { [weak self] ok, error in
            guard let self else { return }
            let secs = Date().timeIntervalSince(started)
            self.timing["transfer_install_s"] = round(secs * 10) / 10
            if let at = self.requestAt { self.timing["installed_s"] = round(Date().timeIntervalSince(at) * 10) / 10 }
            self.log("installed \(name): \(ok) \(error) in \(String(format: "%.1f", secs)) s")
            if ok, !self.afterInstall.isEmpty {
                let next = self.afterInstall.removeFirst()
                DispatchQueue.main.asyncAfter(deadline: .now() + 8) { self.log("typed request: \(next)"); self.rose.text(next) }
            }
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
