import AppKit
import ApplicationServices
import Foundation
import Security

/// Adapter for the signed 0.1.14 Mac release. Locomo owns its models, settings and library.
enum LocomoPlugin {
    static let maxBytes = 4 * 1024 * 1024
    private static let bundleID = "com.locomomusic.mac"

    struct Song {
        let title: String
        let durationMs: Int
        let audio: Data
    }

    enum Failure: LocalizedError {
        case release, library, timeout, encoder
        var errorDescription: String? {
            switch self {
            case .release: return "Select the signed Locomo Music 0.1.14 release."
            case .library: return "Open Locomo Music and finish its first-run setup."
            case .timeout: return "No new Cool Jazz track arrived from Locomo."
            case .encoder: return "Could not encode the Locomo track as MP3. Check that ffmpeg is installed."
            }
        }
    }

    static func verify(_ app: URL) throws {
        guard let bundle = Bundle(url: app),
              bundle.bundleIdentifier == bundleID,
              bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String == "0.1.14" else {
            throw Failure.release
        }
        var code: SecStaticCode?
        var requirement: SecRequirement?
        let rule = #"anchor apple generic and identifier "com.locomomusic.mac" and certificate leaf[subject.OU] = "KP7VE59L3R""#
        guard SecStaticCodeCreateWithPath(app as CFURL, [], &code) == errSecSuccess,
              SecRequirementCreateWithString(rule as CFString, [], &requirement) == errSecSuccess,
              let code, let requirement,
              SecStaticCodeCheckValidity(code, [], requirement) == errSecSuccess else {
            throw Failure.release
        }
    }

    static func generate(app: URL, status: @escaping (String) -> Void) async throws -> Song {
        try verify(app)
        let library = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(bundleID).appendingPathComponent("library")
        let db = library.appendingPathComponent("music.sqlite")
        guard FileManager.default.fileExists(atPath: db.path) else { throw Failure.library }
        let started = Int64(Date().timeIntervalSince1970 * 1000)
        let running = try await NSWorkspace.shared.openApplication(at: app, configuration: .init())
        guard running.bundleURL?.resolvingSymlinksInPath() == app.resolvingSymlinksInPath() else {
            throw Failure.release
        }
        let triggered = await startStation(pid: running.processIdentifier)
        status(triggered ? "Locomo is making a Cool Jazz song…" : "On the Mac, start one Cool Jazz song in Locomo.")
        return try await Task.detached(priority: .utility) {
            let deadline = Date().addingTimeInterval(600)
            while Date() < deadline {
                if let song = try nextSong(db: db, library: library, since: started) { return song }
                try await Task.sleep(for: .seconds(1))
            }
            throw Failure.timeout
        }.value
    }

    private static func startStation(pid: pid_t) async -> Bool {
        guard AXIsProcessTrusted() else { return false }
        let app = AXUIElementCreateApplication(pid)
        let elements = controls(app)
        // Selecting another station can start its own automatic run. Require the open station.
        guard let station = elements.first(where: { label($0) == "Cool Jazz, Jazz" }),
              value(station, kAXValueAttribute) as? NSNumber == 1,
              elements.contains(where: { label($0) == "Make a new song" }),
              let start = elements.first(where: { label($0) == "Start continuous generation" }),
              AXUIElementPerformAction(start, kAXPressAction as CFString) == .success else { return false }

        // Locomo has already started the first job. Stop clears its pending automatic queue.
        for _ in 0..<20 {
            if let stop = controls(app).first(where: { label($0) == "Stop continuous generation" }),
               AXUIElementPerformAction(stop, kAXPressAction as CFString) == .success { break }
            try? await Task.sleep(for: .milliseconds(100))
        }
        return true
    }

    private static func value(_ element: AXUIElement, _ attribute: String) -> CFTypeRef? {
        var result: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, attribute as CFString, &result) == .success ? result : nil
    }

    private static func controls(_ app: AXUIElement) -> [AXUIElement] {
        // Electron exposes the station and generation controls in its accessible document.
        var pending = value(app, kAXWindowsAttribute) as? [AXUIElement] ?? []
        var found: [AXUIElement] = []
        while let element = pending.popLast(), found.count < 1000 {
            found.append(element)
            pending.append(contentsOf: value(element, kAXChildrenAttribute) as? [AXUIElement] ?? [])
        }
        return found
    }

    private static func label(_ element: AXUIElement) -> String? {
        for attribute in [kAXDescriptionAttribute, kAXTitleAttribute, kAXValueAttribute] {
            if let name = value(element, attribute) as? String, !name.isEmpty { return name }
        }
        return nil
    }

    private struct Row: Decodable {
        let id: String
        let title: String?
        let duration: Double
    }

    private static func nextSong(db: URL, library: URL, since: Int64) throws -> Song? {
        let query = """
            SELECT id, title, duration FROM generation
            WHERE category_key = 'existing/cool-jazz' AND created_at >= \(since)
            AND id NOT IN (SELECT track_id FROM bundled_seed_receipt)
            ORDER BY created_at LIMIT 1
            """
        let child = Process()
        child.executableURL = URL(fileURLWithPath: "/usr/bin/sqlite3")
        child.arguments = ["-readonly", "-json", "-cmd", ".timeout 250", db.path, query]
        let pipe = Pipe()
        child.standardOutput = pipe
        child.standardError = FileHandle.nullDevice
        try child.run()
        let result = pipe.fileHandleForReading.readDataToEndOfFile()
        child.waitUntilExit()
        guard child.terminationStatus == 0 else {
            if child.terminationStatus == 5 { return nil }
            throw Failure.library
        }
        guard !result.isEmpty, let row = try JSONDecoder().decode([Row].self, from: result).first,
              let id = UUID(uuidString: row.id), row.duration.isFinite,
              row.duration > 0, row.duration <= 600 else { return nil }
        let source = library.appendingPathComponent("generations").appendingPathComponent(id.uuidString.lowercased() + ".m4a")
        guard FileManager.default.fileExists(atPath: source.path) else { return nil }
        return Song(title: String((row.title ?? "Cool Jazz").prefix(100)),
                    durationMs: max(1, Int(row.duration * 1000)), audio: try encode(source))
    }

    private static func encode(_ source: URL) throws -> Data {
        let candidates = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
        guard let ffmpeg = candidates.first(where: FileManager.default.isExecutableFile(atPath:)) else { throw Failure.encoder }
        let child = Process()
        child.executableURL = URL(fileURLWithPath: ffmpeg)
        child.arguments = ["-v", "error", "-nostdin", "-i", source.path, "-map", "0:a:0",
                           "-t", "600", "-codec:a", "libmp3lame", "-b:a", "48k", "-f", "mp3", "-fs", String(maxBytes), "pipe:1"]
        let pipe = Pipe()
        child.standardOutput = pipe
        child.standardError = FileHandle.nullDevice
        try child.run()
        let audio = pipe.fileHandleForReading.readDataToEndOfFile()
        child.waitUntilExit()
        guard child.terminationStatus == 0, !audio.isEmpty, audio.count < maxBytes else { throw Failure.encoder }
        return audio
    }
}
