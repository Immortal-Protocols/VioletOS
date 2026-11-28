// Compile alongside VioletAgent/AppManifest.swift and run the resulting host executable.
import Foundation

@main
enum AppManifestTest {
    static func main() throws {
        let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: source, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: source) }
        let file = source.appendingPathComponent("manifest.txt")
        let raw = Data("program".utf8)
        let manifest = Data("caps: net mic\n".utf8)
        let packed = raw + manifest + Data([UInt8(manifest.count), 0]) + Data("VMF1".utf8)

        try AppManifest.check(program: raw, source: source)
        precondition((try? AppManifest.check(program: packed, source: source)) == nil)
        try "caps: net, mic # comment\n".write(to: file, atomically: true, encoding: .utf8)
        try AppManifest.check(program: packed, source: source)
        precondition((try? AppManifest.check(program: raw, source: source)) == nil)
        let malformed = raw + Data("caps  net mic\n".utf8) + Data([UInt8(manifest.count), 0]) + Data("VMF1".utf8)
        precondition((try? AppManifest.check(program: malformed, source: source)) == nil)
        try "caps: net\n".write(to: file, atomically: true, encoding: .utf8)
        precondition((try? AppManifest.check(program: packed, source: source)) == nil)
    }
}
