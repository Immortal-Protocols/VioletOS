// Violet Agent for macOS: pairs with the VioletOS phone by its QR code,
// listens through the phone, answers and builds apps with Codex, and installs
// them on the phone. See companion/mac/README.md.
import SwiftUI

@main
struct VioletAgentApp: App {
    @StateObject private var model = AgentModel()

    var body: some Scene {
        WindowGroup("Violet Agent") {
            ContentView()
                .environmentObject(model)
                .frame(minWidth: 560, minHeight: 640)
                .onAppear { model.start() }
        }
        .windowResizability(.contentMinSize)
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

struct ContentView: View {
    @EnvironmentObject var model: AgentModel
    @State private var typed = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Violet Agent").font(.largeTitle.bold())
                Spacer()
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
                    Pill(ok: false, text: "No phone paired. Open Agent on the phone and choose Pair a phone.")
                }
                Pill(ok: model.relayUp, text: model.relayUp ? "Relay connected" : "Relay not connected")
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
            ScannerView(onCode: { s in
                if let inv = Invitation(s) { model.pair(inv) } else { model.scanning = false }
            }, onCancel: { model.scanning = false })
        }
    }

    private func send() {
        let t = typed.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty else { return }
        typed = ""
        model.userTurn(t)
    }
}
