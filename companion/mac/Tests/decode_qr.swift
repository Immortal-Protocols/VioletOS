// Decodes a picture of the phone's pairing screen the way Violet Agent's
// scanner does (Vision, QR only) and checks the invitation parses.
// swift companion/mac/Tests/decode_qr.swift IMAGE
import AppKit
import Vision

let path = CommandLine.arguments[1]
guard let image = NSImage(contentsOfFile: path), let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    print("cannot read \(path)"); exit(1)
}
let req = VNDetectBarcodesRequest()
req.symbologies = [.qr]
try VNImageRequestHandler(cgImage: cg).perform([req])
guard let text = req.results?.first?.payloadStringValue else { print("no code found"); exit(1) }
guard text.hasPrefix("violet-agent:1?"), let q = URLComponents(string: "x://y?" + text.dropFirst(15))?.queryItems,
      q.contains(where: { $0.name == "k" }), q.contains(where: { $0.name == "s" }), q.contains(where: { $0.name == "r" }) else {
    print("not a pairing code: \(text)"); exit(1)
}
print("decoded: \(text)")
