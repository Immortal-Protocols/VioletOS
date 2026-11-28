import Foundation

/// Check the file handed to install_app, including when its builder comes from an older checkout.
enum AppManifest {
    static func check(program: Data, source: URL) throws {
        let file = source.appendingPathComponent("manifest.txt")
        let hasTrailer = program.suffix(4) == Data("VMF1".utf8)
        guard FileManager.default.fileExists(atPath: file.path) else {
            if hasTrailer { throw CocoaError(.fileReadCorruptFile) }
            return
        }
        let expected = try String(contentsOf: file, encoding: .utf8)
        guard program.count >= 6, hasTrailer else {
            throw CocoaError(.fileReadCorruptFile)
        }
        let end = program.count - 6
        let length = Int(program[end]) | (Int(program[end + 1]) << 8)
        guard length <= end, let canonical = canonicalManifest(expected),
              program[(end - length)..<end].elementsEqual(canonical.utf8) else {
            throw CocoaError(.fileReadCorruptFile)
        }
    }

    // The packager strips comments and normalizes whitespace and comma-separated words.
    private static func canonicalManifest(_ text: String) -> String? {
        var result = ""
        for line in text.split(whereSeparator: \.isNewline) {
            let field = line.prefix { $0 != "#" }.trimmingCharacters(in: .whitespaces)
            if field.isEmpty { continue }
            guard let colon = field.firstIndex(of: ":") else { return nil }
            let key = field[..<colon].trimmingCharacters(in: .whitespaces)
            let words = field[field.index(after: colon)...].split { $0.isWhitespace || $0 == "," }
            result += key + ": " + words.joined(separator: " ") + "\n"
        }
        return result
    }
}
