// Compile alongside ShieldWindows.swift and run the resulting host executable.
import Foundation

@main
enum ShieldRelayEvidenceTest {
    static func main() {
        let track = AppTrack(pid: 1, name: "test", sha8: "12345678")
        for (kind, detail) in [("SOCK_CONN", "Example.test:443"), ("SOCK_TX", "16B")] {
            track.add(SentinelRecord(line: "1\t1\ttest\t12345678\t\(kind)\t0\t0\t\(detail)\tExample.test:443")!)
        }
        let unknown = track.window(unreported: []) { _ in nil }
        precondition(!unknown.canonicalText().contains("SOCKS5_SEEN="))
        precondition(unknown.sideFeatures(minCosineDistance: 0)[6] == 0)

        let missing = track.window(unreported: []) { _ in false }
        precondition(missing.events.allSatisfy { $0.socksSeen == false })
        precondition(missing.sideFeatures(minCosineDistance: 0)[6] == 2)

        let observed = track.window(unreported: []) { $0.target.lowercased() == "example.test:443" }
        precondition(observed.events.allSatisfy { $0.socksSeen == true })
        precondition(observed.sideFeatures(minCosineDistance: 0)[6] == 0)
    }
}
