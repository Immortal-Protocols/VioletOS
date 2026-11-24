import UIKit

/// Demo recordings: with `-shieldSnapshots 1`, the app saves its own window as
/// Documents/snapshots/<seconds since 1970>.png about once a second, so a demo can
/// show the companion's screen where no screen recorder runs (an iPhone app running
/// on a Mac, or a device). Off unless asked for.
enum Snapshots {
    static func startIfAsked() {
        guard UserDefaults.standard.bool(forKey: "shieldSnapshots") else { return }
        let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("snapshots")
        try? FileManager.default.removeItem(at: dir)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
            guard let window = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene })
                .flatMap(\.windows).first(where: \.isKeyWindow) else { return }
            let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in
                window.drawHierarchy(in: window.bounds, afterScreenUpdates: false)
            }
            let name = String(format: "%.2f.png", Date().timeIntervalSince1970)
            if let png = image.pngData() {
                try? png.write(to: dir.appendingPathComponent(name))
                DemoReport.send("snapshots/" + name, png)
            }
        }
    }
}

/// Demo runs: with `-demoReportURL http://127.0.0.1:PORT`, the app also POSTs each
/// status file and snapshot to PORT/NAME, for hosts that cannot read the app's
/// container (an iPhone app running on a Mac is behind macOS's app data protection).
enum DemoReport {
    static let base: URL? = UserDefaults.standard.string(forKey: "demoReportURL").flatMap(URL.init(string:))

    static func send(_ name: String, _ data: Data) {
        guard let base else { return }
        var request = URLRequest(url: base.appendingPathComponent(name))
        request.httpMethod = "POST"
        request.timeoutInterval = 5
        URLSession.shared.uploadTask(with: request, from: data).resume()
    }
}
