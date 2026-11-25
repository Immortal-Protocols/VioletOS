import AppKit
import ApplicationServices
import CryptoKit
import Foundation

enum LocomoSetup {
    private static let download = URL(string: "https://github.com/wonderdawg/locomo-music-releases/releases/download/v0.1.14/Locomo-Music-0.1.14-arm64.zip")!
    private static let checksum = "d5abd8a41f2afef97031962ae97338f0110e39eb25dbd58578fa39c507dfec19"
    static var installedApp: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Applications/Locomo Music.app")
    }

    enum SetupError: LocalizedError {
        case download, checksum, installer, homebrew, models(String)

        var errorDescription: String? {
            switch self {
            case .download: return "Locomo Music could not be downloaded. Check your internet connection and try again."
            case .checksum: return "The Locomo download did not match the expected checksum."
            case .installer: return "Setup failed. Check the Mac’s available storage and try again."
            case .homebrew: return "Install Homebrew so Violet Agent can install ffmpeg, then retry setup."
            case .models(let message): return message
            }
        }
    }

    static func installApp(at destination: URL = installedApp) async throws -> URL {
        let files = FileManager.default
        if files.fileExists(atPath: destination.path) {
            try LocomoPlugin.verify(destination)
            return destination
        }

        let (archive, response) = try await URLSession.shared.download(from: download)
        defer { try? files.removeItem(at: archive) }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw SetupError.download }
        return try await Task.detached(priority: .utility) {
            let archiveData = try Data(contentsOf: archive, options: .mappedIfSafe)
            let digest = SHA256.hash(data: archiveData).map { String(format: "%02x", $0) }.joined()
            guard digest == checksum else { throw SetupError.checksum }
            let parent = destination.deletingLastPathComponent()
            try files.createDirectory(at: parent, withIntermediateDirectories: true)
            let staging = parent.appendingPathComponent(".locomo-" + UUID().uuidString)
            try files.createDirectory(at: staging, withIntermediateDirectories: false)
            defer { try? files.removeItem(at: staging) }
            try run("/usr/bin/ditto", ["-x", "-k", archive.path, staging.path])
            let app = staging.appendingPathComponent("Locomo Music.app")
            try LocomoPlugin.verify(app)
            try files.moveItem(at: app, to: destination)
            return destination
        }.value
    }

    static func installEncoder() async throws {
        if LocomoPlugin.encoder != nil { return }
        try await Task.detached(priority: .utility) {
            let candidates = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
            guard let brew = candidates.first(where: FileManager.default.isExecutableFile(atPath:)) else {
                throw SetupError.homebrew
            }
            try run(brew, ["install", "ffmpeg"])
        }.value
    }

    @MainActor
    static func prepareModels(_ app: URL, progress: (String) -> Void) async throws {
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        _ = try await NSWorkspace.shared.openApplication(at: app, configuration: configuration)
        let state = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("com.locomomusic.mac/runtime-cpp-q8/setup-state.json")
        let deadline = Date().addingTimeInterval(3600)
        while Date() < deadline {
            if let data = try? Data(contentsOf: state),
               let snapshot = try? JSONDecoder().decode(ModelSetup.self, from: data) {
                if snapshot.ready == true { return }
                if snapshot.phase == "error" || snapshot.phase == "failed" {
                    throw SetupError.models(snapshot.message ?? "Open Locomo Music to retry model setup.")
                }
                progress(snapshot.message ?? "Setting up Locomo’s music models…")
            }
            try await Task.sleep(for: .seconds(2))
        }
        throw SetupError.models("Model setup is taking longer than expected. Open Locomo Music to see its progress.")
    }

    static func requestAccessibility() {
        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
        _ = AXIsProcessTrustedWithOptions(options)
    }

    private struct ModelSetup: Decodable {
        let ready: Bool?
        let phase: String?
        let message: String?
    }

    private static func run(_ executable: String, _ arguments: [String]) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { throw SetupError.installer }
    }
}
