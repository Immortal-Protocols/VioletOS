import SwiftUI

/// The Mac's Shield model, VioletOS link, and recent verdicts.
struct ShieldSection: View {
    @ObservedObject var ui = ShieldUI.shared

    var body: some View {
        let s = ui.snap
        Section {
            HStack(spacing: 10) {
                Image(systemName: s.linkUp ? "checkmark.shield.fill" : "shield")
                    .font(.title2)
                    .foregroundStyle(s.linkUp ? Color.green : Color.secondary)
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
            row("Home warnings verified", "\(s.shieldsAccepted)", id: "shield-shields")
            row("Mode", "Monitoring only", id: "shield-mode")
        } header: {
            Text("Violet Shield")
        } footer: {
            Text("Checks run on this Mac. VioletOS shows warnings on Home.")
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
            Text("\(d.tier == "model" ? "model" : d.tier)\(d.latencyMs > 0 ? " · \(Int(d.latencyMs)) ms" : "")\(d.tags.isEmpty ? "" : " · " + d.tags.map(\.0).joined(separator: ", "))")
                .font(.caption).foregroundStyle(.secondary)
            if !d.note.isEmpty {
                Text(d.note).font(.caption).foregroundStyle(.secondary)
            }
            if d.warned {
                Text("Home warning sent").font(.caption).foregroundStyle(.orange)
                HStack {
                    Button("Mark Benign") { Shield.shared.label(d, benign: true) }
                    Button("Mark Threat") { Shield.shared.label(d, benign: false) }
                }
                .buttonStyle(.bordered).controlSize(.small)
            }
        }
        .accessibilityIdentifier("shield-decision")
    }
}
