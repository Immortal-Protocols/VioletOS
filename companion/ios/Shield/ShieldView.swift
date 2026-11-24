import SwiftUI

/// Violet Shield's part of the companion screen: model, link to VioletOS,
/// what was contained, and every recent verdict.
struct ShieldSection: View {
    @ObservedObject var ui = ShieldUI.shared

    var body: some View {
        let s = ui.snap
        Section {
            HStack(spacing: 10) {
                Image(systemName: s.frozen.isEmpty ? (s.linkUp ? "checkmark.shield.fill" : "shield") : "exclamationmark.shield.fill")
                    .font(.title2)
                    .foregroundStyle(s.frozen.isEmpty ? (s.linkUp ? Color.green : Color.secondary) : Color.red)
                VStack(alignment: .leading, spacing: 2) {
                    Text(s.linkUp ? "Watching VioletOS" : "VioletOS not linked").font(.headline)
                    Text(s.linkUp ? "boot \(s.bootId) · chain \(s.chain)" : s.link)
                        .font(.caption.monospaced()).foregroundStyle(.secondary)
                }
            }
            .accessibilityIdentifier("shield-link")
            if !s.heartbeat.isEmpty {
                Text(s.heartbeat).font(.caption.monospaced()).foregroundStyle(.secondary)
            }
            row("Model", s.modelState, id: "shield-model")
            row("Runs on", s.computeUnits.isEmpty ? "—" : "\(s.computeUnits) · \(s.lastLatencyMs > 0 ? "\(Int(s.lastLatencyMs)) ms/window" : "idle")", id: "shield-compute")
            row("Signing key", s.signer, id: "shield-signer")
            row("Events · windows", "\(s.events) · \(s.windowsScored)", id: "shield-counts")
            row("Shields sent · verified", "\(s.shieldsSent) · \(s.shieldsAccepted)\(s.shieldsRejected > 0 ? " (\(s.shieldsRejected) rejected)" : "")", id: "shield-shields")
            Toggle("Auto-contain (p ≥ 0.90)", isOn: Binding(get: { s.autoContain }, set: { Shield.shared.setAutoContain($0) }))
                .accessibilityIdentifier("shield-auto")
            if !s.frozen.isEmpty {
                row("Frozen on VioletOS", s.frozen.joined(separator: ", "), id: "shield-frozen", tint: .red)
            }
            if !s.blocked.isEmpty {
                row("Blocked by relay", s.blocked.joined(separator: ", "), id: "shield-blocked", tint: .red)
            }
        } header: {
            Text("Violet Shield")
        } footer: {
            Text(ProcessInfo.processInfo.isiOSAppOnMac
                 ? "Checks run on this Mac, at home. Containment is signed with this Mac's key and expires on its own."
                 : "Checks run on this iPhone. Containment is signed with this phone's key and expires on its own.")
        }
        if !s.decisions.isEmpty {
            Section("Verdicts") {
                ForEach(s.decisions.prefix(8)) { d in
                    DecisionRow(d: d)
                }
            }
        }
    }

    private func row(_ label: String, _ value: String, id: String, tint: Color = .secondary) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
            Spacer()
            Text(value).font(.callout).multilineTextAlignment(.trailing).foregroundStyle(tint).accessibilityIdentifier(id)
        }
    }
}

private struct DecisionRow: View {
    let d: ShieldDecision

    var color: Color {
        if d.pMalicious >= 0.9 { return .red }
        if d.pMalicious >= 0.5 { return .orange }
        return .green
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(d.verdict.replacingOccurrences(of: "_", with: " ").uppercased())
                    .font(.caption.bold()).padding(.horizontal, 6).padding(.vertical, 2)
                    .background(color.opacity(0.2), in: Capsule()).foregroundStyle(color)
                Text(d.app).font(.subheadline.bold())
                if d.pid != 0 { Text("pid \(d.pid)").font(.caption).foregroundStyle(.secondary) }
                Spacer()
                Text(String(format: "p %.2f", d.pMalicious)).font(.caption.monospacedDigit())
            }
            Text("\(d.tier == "model" ? "model" : d.tier) → \(d.action)\(d.latencyMs > 0 ? " · \(Int(d.latencyMs)) ms" : "")\(d.tags.isEmpty ? "" : " · " + d.tags.map(\.0).joined(separator: ", "))")
                .font(.caption).foregroundStyle(.secondary)
            if !d.note.isEmpty {
                Text(d.note).font(.caption).foregroundStyle(d.contained ? .red : .secondary)
            }
            if d.contained {
                HStack {
                    Button("Release") { Shield.shared.release(d) }
                    Button("Allow & Learn") { Shield.shared.label(d, benign: true) }
                    Button("Threat") { Shield.shared.label(d, benign: false) }.tint(.red)
                }
                .buttonStyle(.bordered).controlSize(.small)
            }
        }
        .accessibilityIdentifier("shield-decision")
    }
}
