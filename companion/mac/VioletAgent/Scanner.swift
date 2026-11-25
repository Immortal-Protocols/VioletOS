// Reads the phone's pairing code with the Mac's camera.
import AVFoundation
import SwiftUI
import Vision

final class ScannerController: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
    let session = AVCaptureSession()
    var onCode: (String) -> Void = { _ in }
    var onError: (String) -> Void = { _ in }
    private let queue = DispatchQueue(label: "scanner")
    private var found = false
    private var lastLook = Date.distantPast

    func start() {
        AVCaptureDevice.requestAccess(for: .video) { ok in
            DispatchQueue.main.async {
                guard ok else { return self.onError("Allow the camera for Violet Agent in System Settings › Privacy") }
                self.configure()
            }
        }
    }

    private func configure() {
        let kinds: [AVCaptureDevice.DeviceType] = [.builtInWideAngleCamera, .external, .continuityCamera]
        let devices = AVCaptureDevice.DiscoverySession(deviceTypes: kinds, mediaType: .video, position: .unspecified).devices
        guard let device = AVCaptureDevice.default(for: .video) ?? devices.first,
              let input = try? AVCaptureDeviceInput(device: device) else {
            return onError("No camera found. Connect one, or use an iPhone as a Continuity Camera.")
        }
        session.beginConfiguration()
        session.sessionPreset = .high
        if session.canAddInput(input) { session.addInput(input) }
        let out = AVCaptureVideoDataOutput()
        out.setSampleBufferDelegate(self, queue: queue)
        out.alwaysDiscardsLateVideoFrames = true
        if session.canAddOutput(out) { session.addOutput(out) }
        session.commitConfiguration()
        queue.async { self.session.startRunning() }
    }

    func stop() {
        queue.async { self.session.stopRunning() }
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sample: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard !found, Date().timeIntervalSince(lastLook) > 0.15, let pixels = CMSampleBufferGetImageBuffer(sample) else { return }
        lastLook = Date()
        let req = VNDetectBarcodesRequest()
        req.symbologies = [.qr]
        try? VNImageRequestHandler(cvPixelBuffer: pixels, options: [:]).perform([req])
        for r in req.results ?? [] {
            if let s = r.payloadStringValue, s.hasPrefix("violet-agent:") {
                found = true
                DispatchQueue.main.async { self.onCode(s) }
                return
            }
        }
    }
}

struct CameraPreview: NSViewRepresentable {
    let session: AVCaptureSession
    func makeNSView(context: Context) -> NSView {
        let v = NSView()
        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        v.layer = layer
        v.wantsLayer = true
        return v
    }
    func updateNSView(_ nsView: NSView, context: Context) {}
}

struct ScannerView: View {
    let onCode: (String) -> Void
    let onCancel: () -> Void
    @State private var controller = ScannerController()
    @State private var error: String?

    var body: some View {
        VStack(spacing: 14) {
            Text("Show the phone's code to the camera").font(.title2.bold())
            Text("Open Agent on the phone. Its code works once.").foregroundStyle(.secondary)
            ZStack {
                CameraPreview(session: controller.session)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                RoundedRectangle(cornerRadius: 16).stroke(.purple.opacity(0.7), lineWidth: 3).padding(60)
                if let error { Text(error).padding().background(.thinMaterial, in: RoundedRectangle(cornerRadius: 10)) }
            }
            .frame(width: 560, height: 420)
            Button("Cancel", action: onCancel).keyboardShortcut(.cancelAction)
        }
        .padding(24)
        .onAppear {
            controller.onCode = { s in controller.stop(); onCode(s) }
            controller.onError = { e in error = e }
            controller.start()
        }
        .onDisappear { controller.stop() }
    }
}
