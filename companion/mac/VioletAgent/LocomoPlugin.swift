import AppKit
import ApplicationServices
import Foundation
import Security

/// Adapter for the signed 0.1.14 Mac release. Locomo owns its models, settings and library.
enum LocomoPlugin {
    static let maxBytes = 4 * 1024 * 1024
    private static let bundleID = "com.locomomusic.mac"
    static var encoder: String? {
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
            .first(where: FileManager.default.isExecutableFile(atPath:))
    }

    struct Song {
        let title: String
        let durationMs: Int
        let audio: Data
        let station: String
    }

    private static let stations = [
        "Future French House", "Latin House", "House", "Synth-Pop", "Indie Pop", "Classic Rock",
        "Punk Rock", "Boom Bap", "Lo-Fi Hip-Hop", "Country Pop", "Cool Jazz", "Jazz Fusion",
        "Techno", "Latin Techno", "Indie Folk", "Thrash Metal", "Progressive Metal", "Cinematic Hip-Hop"
    ]
    private static let durations = [0, 60, 90, 120, 150, 180, 240, 300, 360, 420, 480, 540, 600]

    struct Options {
        let station: String
        let length: Int?
        let count: Int
        let overnight: Bool

        init?(_ request: [String: Any]) {
            station = request["station"] as? String ?? "Cool Jazz"
            length = request["length"] as? Int
            count = request["count"] as? Int ?? 1
            overnight = request["overnight"] as? Bool ?? false
            guard stations.contains(station), (1...100).contains(count),
                  length.map(durations.contains) ?? true else { return nil }
        }

        var key: String { "existing/" + station.lowercased().replacingOccurrences(of: " ", with: "-") }
        var lengthLabel: String? {
            length.map { seconds in
                if seconds == 0 { return "Auto" }
                let minutes = seconds / 60
                return seconds % 60 == 0 ? "\(minutes) min." : "\(minutes).5 min."
            }
        }
    }

    private static var library: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(bundleID).appendingPathComponent("library")
    }

    enum Failure: LocalizedError {
        case release, library, timeout, encoder
        var errorDescription: String? {
            switch self {
            case .release: return "Select the signed Locomo Music 0.1.14 release."
            case .library: return "Open Locomo Music and finish its first-run setup."
            case .timeout: return "No new track arrived from Locomo."
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
              SecStaticCodeCheckValidity(code, SecCSFlags(rawValue: kSecCSCheckNestedCode | kSecCSStrictValidate), requirement) == errSecSuccess else {
            throw Failure.release
        }
    }

    @MainActor
    static func run(app: URL, options: Options, status: @escaping (String) -> Void) async throws -> Song? {
        try verify(app)
        let started = Int64(Date().timeIntervalSince1970 * 1000)
        let running = try await NSWorkspace.shared.openApplication(at: app, configuration: .init())
        guard running.bundleURL?.resolvingSymlinksInPath() == app.resolvingSymlinksInPath() else {
            throw Failure.release
        }
        let application = AXUIElementCreateApplication(running.processIdentifier)
        if options.overnight {
            guard AXIsProcessTrusted(), await openSettings(application),
                  let arm = controls(application).first(where: { label($0) == "Arm" }),
                  press(arm) else {
                status("In Locomo Settings, use Overnight Batch → Arm.")
                return nil
            }
            status("Overnight batch armed on your Mac.")
            return nil
        }

        let triggered = AXIsProcessTrusted() ? await startStation(application, options: options) : false
        if triggered {
            status("Locomo is making \(options.station) music…")
        } else {
            let preferences = [options.lengthLabel, options.count > 1 ? "\(options.count) automatic songs" : nil]
                .compactMap { $0 }.joined(separator: ", ")
            let lead = preferences.isEmpty ? "" : "Set \(preferences) in Mac Settings. "
            status(lead + "Start \(options.station) on the Mac." + (options.count == 1 ? " Stop after it starts." : ""))
        }
        return try await Task.detached(priority: .utility) {
            let deadline = Date().addingTimeInterval(900)
            while Date() < deadline {
                let filter = "category_key = " + literal(options.key) + " AND created_at >= \(started)"
                if let row = try rows("WHERE " + filter + " AND id NOT IN (SELECT track_id FROM bundled_seed_receipt) ORDER BY created_at LIMIT 1").first,
                   let song = try audio(row) {
                    return song
                }
                try await Task.sleep(for: .seconds(1))
            }
            throw Failure.timeout
        }.value
    }

    @MainActor
    static func control(app: URL, action: String) async throws -> String {
        try verify(app)
        let running = try await NSWorkspace.shared.openApplication(at: app, configuration: .init())
        guard running.bundleURL?.resolvingSymlinksInPath() == app.resolvingSymlinksInPath() else {
            throw Failure.release
        }
        let application = AXUIElementCreateApplication(running.processIdentifier)
        if action == "stop" {
            if AXIsProcessTrusted(),
               let stop = controls(application).first(where: { label($0) == "Stop continuous generation" }),
               press(stop) {
                return "Automatic generation stopped. The current song can finish."
            }
            return "If a station is still generating, click Stop in Locomo on the Mac."
        }
        if AXIsProcessTrusted(), await openSettings(application),
           let disarm = controls(application).first(where: { label($0) == "Disarm" }),
           press(disarm) {
            return "Overnight batch disarmed."
        }
        return "If an overnight batch is armed, click Disarm in Locomo Settings."
    }

    @MainActor
    private static func openSettings(_ app: AXUIElement) async -> Bool {
        if controls(app).contains(where: { label($0) == "Song length" }) { return true }
        guard let barValue = value(app, kAXMenuBarAttribute) else { return false }
        let bar = barValue as! AXUIElement
        guard let menu = (value(bar, kAXChildrenAttribute) as? [AXUIElement])?.first(where: { label($0) == "Locomo Music" }) else {
            return false
        }
        let settings = descendants([menu]).first { label($0)?.hasPrefix("Settings…") == true }
        guard let settings, press(settings) else { return false }
        for _ in 0..<15 {
            if controls(app).contains(where: { label($0) == "Song length" }) { return true }
            try? await Task.sleep(for: .milliseconds(100))
        }
        return false
    }

    @MainActor
    private static func configure(_ app: AXUIElement, options: Options) async -> Bool {
        if options.length == nil && options.count == 1 { return true }
        guard await openSettings(app) else { return false }
        let fields = controls(app)
        if let length = options.lengthLabel {
            guard let popup = fields.first(where: { label($0) == "Song length" }) else { return false }
            if value(popup, kAXValueAttribute) as? String != length {
                guard press(popup) else { return false }
                try? await Task.sleep(for: .milliseconds(150))
                let nodes = descendants([popup]) + controls(app)
                guard let item = nodes.first(where: { label($0) == length }),
                      press(item) else {
                    AXUIElementPerformAction(popup, kAXCancelAction as CFString)
                    return false
                }
                try? await Task.sleep(for: .milliseconds(120))
                guard value(popup, kAXValueAttribute) as? String == length else { return false }
            }
        }
        if options.count > 1 {
            guard let stepper = fields.first(where: { label($0) == "Automatic songs per category selection" }) else { return false }
            if value(stepper, kAXValueAttribute) as? NSNumber != NSNumber(value: options.count) {
                guard AXUIElementSetAttributeValue(stepper, kAXValueAttribute as CFString,
                    NSNumber(value: options.count)) == .success,
                    value(stepper, kAXValueAttribute) as? NSNumber == NSNumber(value: options.count) else { return false }
            }
        }
        return true
    }

    @MainActor
    private static func startStation(_ app: AXUIElement, options: Options) async -> Bool {
        // Leave a desktop run alone if it was already in progress.
        guard !controls(app).contains(where: { label($0) == "Stop continuous generation" }),
              await configure(app, options: options) else { return false }
        var elements = controls(app)
        guard let station = elements.first(where: { label($0)?.hasPrefix(options.station + ",") == true }) else { return false }
        if value(station, kAXValueAttribute) as? NSNumber != 1 {
            guard press(station) else { return false }
            try? await Task.sleep(for: .milliseconds(350))
            elements = controls(app)
        }
        guard elements.contains(where: { label($0) == options.station }) else { return false }
        if let start = elements.first(where: { label($0) == "Start continuous generation" }) {
            guard press(start) else { return false }
        } else if !elements.contains(where: { label($0) == "Stop continuous generation" }) {
            return false
        }

        // Locomo keeps the in-flight song and clears the pending automatic queue.
        if options.count == 1 {
            for _ in 0..<20 {
                if let stop = controls(app).first(where: { label($0) == "Stop continuous generation" }),
                   press(stop) { break }
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
        return true
    }

    private static func press(_ element: AXUIElement) -> Bool {
        AXUIElementPerformAction(element, kAXPressAction as CFString) == .success
    }

    private static func value(_ element: AXUIElement, _ attribute: String) -> CFTypeRef? {
        var result: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, attribute as CFString, &result) == .success ? result : nil
    }

    private static func controls(_ app: AXUIElement) -> [AXUIElement] {
        // Electron exposes the station and generation controls in its accessible document.
        descendants(value(app, kAXWindowsAttribute) as? [AXUIElement] ?? [])
    }

    private static func descendants(_ roots: [AXUIElement]) -> [AXUIElement] {
        var pending = roots
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

    struct LibraryEntry: Codable {
        let id: String
        let title: String
        let station: String
        let duration_ms: Int
        let favorite: Bool
    }

    private struct Row: Decodable {
        let id: String
        let title: String?
        let category_key: String?
        let duration: Double
        let is_favorite: Int

        var entry: LibraryEntry? {
            guard let uuid = UUID(uuidString: id), duration.isFinite, duration > 0, duration <= 600 else { return nil }
            let slug = category_key?.split(separator: "/").last.map(String.init) ?? ""
            let station = stations.first {
                $0.lowercased().replacingOccurrences(of: " ", with: "-") == slug
            } ?? "Locomo Music"
            return LibraryEntry(id: uuid.uuidString.lowercased(),
                title: String((title ?? "Untitled song").prefix(100)), station: station,
                duration_ms: max(1, Int(duration * 1000)), favorite: is_favorite != 0)
        }
    }

    static func browse(query: String, page: Int, favorites: Bool) throws -> ([LibraryEntry], Bool) {
        let term = literal(String(query.prefix(70)).lowercased())
        let whereClause = "WHERE duration > 0 AND duration <= 600 "
            + (favorites ? "AND is_favorite != 0 " : "")
            + "AND (instr(lower(coalesce(title, '')), \(term)) > 0 OR instr(replace(lower(coalesce(category_key, '')), '-', ' '), \(term)) > 0) "
        let result = try rows(whereClause + "ORDER BY created_at DESC, id LIMIT 9 OFFSET \(max(0, min(100_000, page)) * 8)")
        return (result.prefix(8).compactMap(\.entry), result.count > 8)
    }

    static func read(_ id: UUID) throws -> Song {
        guard let row = try rows("WHERE lower(id) = " + literal(id.uuidString.lowercased()) + " LIMIT 1").first,
              let song = try audio(row) else { throw Failure.library }
        return song
    }

    private static func literal(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "''").replacingOccurrences(of: "\0", with: "") + "'"
    }

    private static func rows(_ clause: String) throws -> [Row] {
        let db = library.appendingPathComponent("music.sqlite")
        let child = Process()
        child.executableURL = URL(fileURLWithPath: "/usr/bin/sqlite3")
        child.arguments = ["-readonly", "-json", "-cmd", ".timeout 750", db.path,
                           "SELECT id, title, category_key, duration, is_favorite FROM generation " + clause]
        let pipe = Pipe()
        child.standardOutput = pipe
        child.standardError = FileHandle.nullDevice
        try child.run()
        let result = pipe.fileHandleForReading.readDataToEndOfFile()
        child.waitUntilExit()
        guard child.terminationStatus == 0 else { throw Failure.library }
        return result.isEmpty ? [] : try JSONDecoder().decode([Row].self, from: result)
    }

    private static func audio(_ row: Row) throws -> Song? {
        guard let entry = row.entry else { return nil }
        let source = library.appendingPathComponent("generations").appendingPathComponent(entry.id + ".m4a")
        guard FileManager.default.fileExists(atPath: source.path) else { return nil }
        return Song(title: entry.title, durationMs: entry.duration_ms, audio: try encode(source), station: entry.station)
    }

    private static func encode(_ source: URL) throws -> Data {
        guard let ffmpeg = encoder else { throw Failure.encoder }
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
