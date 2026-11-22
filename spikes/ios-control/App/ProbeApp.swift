import SwiftUI

@main
struct ProbeApp: App {
    var body: some Scene {
        WindowGroup {
            VStack(spacing: 12) {
                Text("Violet Probe").font(.largeTitle)
                Text(Date.now.formatted(date: .omitted, time: .standard))
                    .accessibilityIdentifier("launch-time")
            }
        }
    }
}
