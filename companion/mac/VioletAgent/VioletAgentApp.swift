// Violet Agent for macOS: pairs with the VioletOS phone by its QR code,
// listens through the phone, answers and builds apps with Codex, and installs
// them on the phone. See companion/mac/README.md.
import SwiftUI

@main
struct VioletAgentApp: App {
    @StateObject private var model = AgentModel()
    @State private var tab = Tab.agent
    private enum Tab { case agent, shield, locomo, add }

    var body: some Scene {
        WindowGroup("Violet Agent") {
            TabView(selection: $tab) {
                ContentView()
                    .tabItem { Label("Agent", systemImage: "waveform") }
                    .tag(Tab.agent)
                List { ShieldSection() }
                    .tabItem { Label("Violet Shield", systemImage: "shield") }
                    .tag(Tab.shield)
                if model.locomoApp != nil {
                    LocomoView {
                        tab = .add
                        model.removeLocomo()
                    }
                    .tabItem { Label("Locomo Music", systemImage: "music.note") }
                    .tag(Tab.locomo)
                }
                Form {
                    Section("Local Plugins") {
                        Label("Locomo Music", systemImage: "music.note")
                        Text("Generate songs on this Mac and play them on your paired device.")
                            .foregroundStyle(.secondary)
                        if model.locomoApp == nil {
                            Button("Set Up Locomo Music") { model.setupLocomo() }
                                .buttonStyle(.borderedProminent)
                                .disabled(model.locomoInstalling)
                            if model.locomoInstalling { ProgressView().controlSize(.small) }
                            Text(model.locomoStatus).foregroundStyle(.secondary)
                        } else {
                            Button("Open Locomo Music Tab") { tab = .locomo }
                        }
                    }
                }
                .formStyle(.grouped)
                .tabItem { Text("+").accessibilityLabel("Add Local Plugin") }
                .tag(Tab.add)
            }
                .environmentObject(model)
                .frame(minWidth: 560, minHeight: 640)
                .onAppear { model.start() }
                .onChange(of: model.locomoApp) { _, app in
                    if app != nil && tab == .add { tab = .locomo }
                }
        }
        .windowResizability(.contentMinSize)
    }
}

struct LocomoView: View {
    @EnvironmentObject var model: AgentModel
    let remove: () -> Void

    var body: some View {
        Form {
            Section("Locomo Music") {
                Text(model.locomoStatus)
                Button("Enable One-Tap Generation…") { LocomoSetup.requestAccessibility() }
                Button("Remove Plugin", action: remove)
                    .disabled(model.locomoRunning)
            }
        }
        .formStyle(.grouped)
    }
}

struct Pill: View {
    let ok: Bool
    let text: String
    var body: some View {
        HStack(spacing: 8) {
            Circle().fill(ok ? Color.green : Color.orange).frame(width: 9, height: 9)
            Text(text).font(.callout).lineLimit(2)
            Spacer(minLength: 0)
        }
    }
}

/// The agent's name ("Violet" until changed), saved on the Mac and shown on the phone.
struct NameField: View {
    @EnvironmentObject var model: AgentModel
    @State private var editing = false
    @State private var name = ""
    var body: some View {
        if editing {
            TextField("Name", text: $name).frame(width: 140).textFieldStyle(.roundedBorder)
                .onSubmit { model.rename(name); editing = false }
        } else {
            Button("Rename…") { name = model.agentName; editing = true }
        }
    }
}

struct ContentView: View {
    @EnvironmentObject var model: AgentModel
    @State private var typed = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text(model.agentName).font(.largeTitle.bold())
                Spacer()
                NameField()
                if model.phone != nil {
                    Button("Unpair") { model.unpair() }
                } else {
                    Button("Pair a phone…") { model.scanning = true }.buttonStyle(.borderedProminent)
                }
            }
            VStack(alignment: .leading, spacing: 6) {
                if let p = model.phone {
                    Pill(ok: model.online, text: model.online ? "\(p.name) is connected" : "Waiting for \(p.name)…")
                } else {
                    Pill(ok: false, text: "No phone paired")
                    if model.localReady {
                        Text("In the emulator, open Violet Agent and click Connect to this Mac.")
                            .foregroundStyle(.secondary)
                    }
                }
                Pill(ok: model.relayUp, text: model.relayUp ? "Relay connected" : "Relay not connected")
                if model.online {
                    if model.phone?.relay == LocalCompanion.relay {
                        Pill(ok: true, text: "Local emulator connection")
                    } else {
                        Pill(ok: model.directPath != nil, text: model.directPath.map { "Direct path to the phone (\($0))" } ?? "Through the relay (no direct path)")
                    }
                }
                Pill(ok: model.codexState.hasPrefix("Codex ready"), text: model.codexState)
                Pill(ok: model.speechState.hasPrefix("Speech recognition on"), text: model.speechState)
                Pill(ok: model.workspaceState.hasPrefix("App workspace ready"), text: model.workspaceState)
                if let t = model.lastTiming { Pill(ok: true, text: t) }
            }
            .padding(14)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 12))

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(model.lines) { l in
                            Text(l.text)
                                .padding(10)
                                .background(l.user ? Color.purple.opacity(0.18) : Color.gray.opacity(0.14), in: RoundedRectangle(cornerRadius: 10))
                                .frame(maxWidth: .infinity, alignment: l.user ? .trailing : .leading)
                                .id(l.id)
                        }
                        if !model.heard.isEmpty {
                            Text(model.heard).italic().foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .trailing)
                        }
                    }
                }
                .onChange(of: model.lines.count) { _, _ in
                    if let last = model.lines.last { proxy.scrollTo(last.id, anchor: .bottom) }
                }
            }
            HStack {
                TextField("Type to the agent", text: $typed).textFieldStyle(.roundedBorder)
                    .onSubmit(send)
                Button("Send", action: send).disabled(typed.isEmpty)
            }
            Text("Activity: \(model.activity)").font(.caption).foregroundStyle(.secondary)
        }
        .padding(22)
        .sheet(isPresented: $model.scanning) {
            PairSheet().environmentObject(model)
        }
    }

    private func send() {
        let t = typed.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty else { return }
        typed = ""
        model.userTurn(t)
    }
}

/// Pairing: this Mac shows a code for the phone's camera, or reads the phone's.
struct PairSheet: View {
    @EnvironmentObject var model: AgentModel
    @State private var scan = false

    var body: some View {
        VStack(spacing: 12) {
            Picker("", selection: $scan) {
                Text("Show this Mac's code").tag(false)
                Text("Scan the phone's code").tag(true)
            }
            .pickerStyle(.segmented)
            .frame(width: 420)
            .padding(.top, 18)
            if scan {
                ScannerView(onCode: { s in
                    if let inv = Invitation(s), !inv.fromMac { model.pair(inv) } else { model.scanning = false }
                }, onCancel: { model.scanning = false })
            } else {
                CodeView().environmentObject(model)
            }
        }
        .onChange(of: scan) { _, now in if now { model.cancelCode() } }
        .onDisappear { model.cancelCode() }
    }
}

struct CodeView: View {
    @EnvironmentObject var model: AgentModel

    var body: some View {
        VStack(spacing: 14) {
            Text("Scan this code with the phone").font(.title2.bold())
            Text("Open Agent on the phone and tap Scan the Mac's code. The code works once.").foregroundStyle(.secondary)
            if let code = model.showingCode, let png = qrPNG(code), let image = NSImage(data: png) {
                Image(nsImage: image).interpolation(.none).resizable().frame(width: 420, height: 420)
                    .padding(16).background(.white, in: RoundedRectangle(cornerRadius: 16))
            } else {
                ProgressView().frame(width: 420, height: 420)
            }
            Pill(ok: model.relayUp, text: model.relayUp ? "Waiting for the phone" : "Connecting to the relay…").frame(width: 300)
            Button("Cancel") { model.scanning = false }.keyboardShortcut(.cancelAction)
        }
        .padding(24)
        .onAppear { if model.showingCode == nil { model.showCode() } }
    }
}
