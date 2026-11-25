import SwiftUI

@main
struct VioletRelayApp: App {
    @StateObject private var model = RelayModel()

    var body: some Scene {
        WindowGroup {
            StatusView(model: model)
                .onAppear { model.start() }
        }
    }
}

struct StatusView: View {
    @ObservedObject var model: RelayModel
    @State private var code = RelayConfig.pairingCode ?? ""

    var body: some View {
        let s = model.status
        List {
            Section {
                Button("Find Violet networks", systemImage: "wifi") { model.findNetworks() }
                    .disabled(!model.canFindNetworks)
                if !model.knownNetworks.isEmpty {
                    Menu("Previously selected networks") {
                        ForEach(model.knownNetworks, id: \.self) { network in
                            Button(network) { model.selectNetwork(network) }
                        }
                    }
                }
                if let network = model.selectedNetwork {
                    row("Selected Wi-Fi", network, id: "selected-network")
                }
                TextField("Pairing code", text: $code)
                    .font(.title3.monospaced())
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("pairing-code")
                Button(model.selectedNetwork == nil ? "Join by code" : "Connect relay") { model.pair(code: code) }
                    .disabled(RelayConfig.normalizedCode(code) == nil)
                    .accessibilityIdentifier("join")
                row("Network", RelayConfig.pairingCode.map(RelayConfig.ssid(forCode:)) ?? "not paired", id: "network")
                row("Wi-Fi", s.hotspot ?? "—", id: "hotspot")
                row("Background", s.background ?? "—", id: "background")
            } header: {
                Text("VioletOS")
            } footer: {
                Text("Turn on Phone Internet in VioletOS Settings › Phone Link. Find its Wi-Fi network, then enter the code shown on VioletOS. For a connection that stays joined when you lock your iPhone, choose the Violet network in iPhone Settings › Wi-Fi.")
            }
            Section("SOCKS5 relay") {
                row("Listener", "\(s.listenerState) :\(s.port)", id: "listener-state")
                row("Wi-Fi IPv4 (en0)", s.wifiIPv4.isEmpty ? "none" : s.wifiIPv4.joined(separator: ", "), id: "wifi-ip")
                row("Cellular", s.cellularAvailable ? "available" : "unavailable", id: "cellular")
                Text("The relay uses silent audio to stay active with the screen locked. Playing other audio or video can pause it.")
                    .font(.footnote)
            }
            Section("Traffic") {
                row("Active connections", "\(s.activeConnections)", id: "active")
                row("Total / failed", "\(s.totalConnections) / \(s.failedConnections)", id: "total")
                row("Auth failures", "\(s.authFailures)", id: "auth-failures")
                row("Up (to internet)", Self.bytes(s.bytesUp), id: "bytes-up")
                row("Down (to client)", Self.bytes(s.bytesDown), id: "bytes-down")
            }
            Section("Last error") {
                Text(s.lastError ?? "none")
                    .font(.footnote.monospaced())
                    .accessibilityIdentifier("last-error")
            }
        }
        .onChange(of: model.selectedNetwork) { _, network in
            if let network, let saved = RelayConfig.normalizedCode(code), RelayConfig.ssid(forCode: saved) != network {
                code = ""
            }
        }
        .navigationTitle("Violet Relay")
    }

    private func row(_ label: String, _ value: String, id: String) -> some View {
        HStack {
            Text(label)
            Spacer()
            Text(value).monospacedDigit().foregroundStyle(.secondary).accessibilityIdentifier(id)
        }
    }

    static func bytes(_ n: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .binary)
    }
}
