import AVFoundation

/// Keeps Violet Relay running in the background by playing silence.
///
/// iOS suspends an app shortly after it leaves the screen, which stops the SOCKS5 listener.
/// An app that is playing audio (UIBackgroundModes: audio) keeps running, so this plays an
/// endless stream of zeros. It mixes with other audio, so music and calls are unaffected,
/// and restarts itself after interruptions such as a phone call or Siri.
final class KeepAlive {
    private let engine = AVAudioEngine()
    private var observers: [NSObjectProtocol] = []
    private(set) var state = "off"

    func start() {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
            try session.setActive(true)
        } catch {
            state = "audio session failed: \(error.localizedDescription)"
            return
        }
        if observers.isEmpty {
            let center = NotificationCenter.default
            observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification,
                                                object: session, queue: .main) { [weak self] note in
                let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
                if raw == AVAudioSession.InterruptionType.ended.rawValue {
                    self?.resume(reason: "after interruption")
                } else {
                    self?.state = "paused by interruption"
                }
            })
            observers.append(center.addObserver(forName: AVAudioSession.mediaServicesWereResetNotification,
                                                object: session, queue: .main) { [weak self] _ in
                self?.resume(reason: "after media reset")
            })
            observers.append(center.addObserver(forName: .AVAudioEngineConfigurationChange,
                                                object: engine, queue: .main) { [weak self] _ in
                self?.resume(reason: "after route change")
            })
            let format = engine.outputNode.inputFormat(forBus: 0)
            let silence = AVAudioSourceNode(format: format) { isSilence, _, _, buffers in
                for buffer in UnsafeMutableAudioBufferListPointer(buffers) {
                    if let data = buffer.mData { memset(data, 0, Int(buffer.mDataByteSize)) }
                }
                isSilence.pointee = true
                return noErr
            }
            engine.attach(silence)
            engine.connect(silence, to: engine.mainMixerNode, format: format)
            engine.mainMixerNode.outputVolume = 0
        }
        resume(reason: "")
    }

    private func resume(reason: String) {
        do {
            try AVAudioSession.sharedInstance().setActive(true)
            if !engine.isRunning { try engine.start() }
            state = reason.isEmpty ? "on" : "on (restarted \(reason))"
        } catch {
            state = "audio failed: \(error.localizedDescription)"
        }
    }
}
