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

    var body: some View {
        let s = model.status
        List {
            Section("SOCKS5 relay") {
                row("Listener", "\(s.listenerState) :\(s.port)", id: "listener-state")
                row("Wi-Fi IPv4 (en0)", s.wifiIPv4.isEmpty ? "none" : s.wifiIPv4.joined(separator: ", "), id: "wifi-ip")
                row("Cellular", s.cellularAvailable ? "available" : "unavailable", id: "cellular")
            }
            Section("Traffic") {
                row("Active connections", "\(s.activeConnections)", id: "active")
                row("Total / failed", "\(s.totalConnections) / \(s.failedConnections)", id: "total")
                row("Auth failures", "\(s.authFailures)", id: "auth-failures")
                row("Up (to internet)", Self.bytes(s.bytesUp), id: "bytes-up")
                row("Down (to client)", Self.bytes(s.bytesDown), id: "bytes-down")
            }
            Section("Wi-Fi join") {
                row("Hotspot", s.hotspot ?? "—", id: "hotspot")
            }
            Section("Last error") {
                Text(s.lastError ?? "none")
                    .font(.footnote.monospaced())
                    .accessibilityIdentifier("last-error")
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
