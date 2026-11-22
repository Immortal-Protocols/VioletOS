import AVFoundation
import MediaPlayer
import UIKit

/// Keeps Violet Relay running in the background and with the screen locked by playing silence.
///
/// iOS suspends an app shortly after it leaves the screen, which stops the SOCKS5 listener.
/// An app that is playing audio (UIBackgroundModes: audio) keeps running. The session is a
/// normal playback session (not mixed with others) so iOS lists Violet Relay under Now Playing
/// on the lock screen and in Control Center, where pause turns the keep-alive off and play
/// turns it back on. It restarts itself after interruptions such as a phone call or Siri.
final class KeepAlive {
    private let engine = AVAudioEngine()
    private var observers: [NSObjectProtocol] = []
    private var wanted = true
    private(set) var state = "off"
    /// Recent lifecycle and audio events, newest last, for status.json.
    private(set) var events: [String] = []

    func start() {
        if observers.isEmpty { setUp() }
        wanted = true
        resume(reason: "start")
    }

    private func log(_ text: String) {
        let stamp = ISO8601DateFormatter().string(from: Date())
        events.append("\(stamp) \(text)")
        if events.count > 30 { events.removeFirst(events.count - 30) }
    }

    private func setUp() {
        let session = AVAudioSession.sharedInstance()
        let center = NotificationCenter.default
        func watch(_ name: Notification.Name, _ object: Any? = nil, _ handler: @escaping (Notification) -> Void) {
            observers.append(center.addObserver(forName: name, object: object, queue: .main, using: handler))
        }
        watch(AVAudioSession.interruptionNotification, session) { [weak self] note in
            guard let self else { return }
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            if raw == AVAudioSession.InterruptionType.ended.rawValue {
                self.log("interruption ended")
                if self.wanted { self.resume(reason: "after interruption") }
            } else {
                self.log("interrupted")
                self.state = "paused by another app's audio"
            }
        }
        watch(AVAudioSession.mediaServicesWereResetNotification, session) { [weak self] _ in
            self?.log("media services reset")
            if self?.wanted == true { self?.resume(reason: "after media reset") }
        }
        watch(.AVAudioEngineConfigurationChange, engine) { [weak self] _ in
            self?.log("audio route changed")
            if self?.wanted == true { self?.resume(reason: "after route change") }
        }
        watch(UIApplication.didEnterBackgroundNotification) { [weak self] _ in
            self?.log("app in background (engine running: \(self?.engine.isRunning == true))")
        }
        watch(UIApplication.willEnterForegroundNotification) { [weak self] _ in self?.log("app in foreground") }
        watch(UIApplication.protectedDataWillBecomeUnavailableNotification) { [weak self] _ in self?.log("screen locked") }
        watch(UIApplication.protectedDataDidBecomeAvailableNotification) { [weak self] _ in self?.log("screen unlocked") }

        // Zeros without the isSilence hint: iOS treats this as real playback.
        let format = engine.outputNode.inputFormat(forBus: 0)
        let silence = AVAudioSourceNode(format: format) { _, _, _, buffers in
            for buffer in UnsafeMutableAudioBufferListPointer(buffers) {
                if let data = buffer.mData { memset(data, 0, Int(buffer.mDataByteSize)) }
            }
            return noErr
        }
        engine.attach(silence)
        engine.connect(silence, to: engine.mainMixerNode, format: format)

        let commands = MPRemoteCommandCenter.shared()
        commands.playCommand.addTarget { [weak self] _ in
            self?.wanted = true
            self?.resume(reason: "from Now Playing")
            return .success
        }
        commands.pauseCommand.addTarget { [weak self] _ in
            self?.pause()
            return .success
        }
        commands.togglePlayPauseCommand.addTarget { [weak self] _ in
            guard let self else { return .commandFailed }
            if self.engine.isRunning { self.pause() } else { self.wanted = true; self.resume(reason: "from Now Playing") }
            return .success
        }
    }

    private func resume(reason: String) {
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .default, options: [])
            try session.setActive(true)
            if !engine.isRunning { try engine.start() }
            state = reason == "start" ? "on" : "on (restarted \(reason))"
            log("playing (\(reason))")
        } catch {
            state = "audio failed: \(error.localizedDescription)"
            log(state)
        }
        publish()
    }

    private func pause() {
        wanted = false
        engine.pause()
        state = "paused from Now Playing - relay stops when the app is in the background"
        log("paused from Now Playing")
        publish()
    }

    private func publish() {
        let running = engine.isRunning
        MPNowPlayingInfoCenter.default().nowPlayingInfo = [
            MPMediaItemPropertyTitle: running ? "Relaying for VioletOS" : "Relay paused",
            MPMediaItemPropertyArtist: "Violet Relay",
            MPNowPlayingInfoPropertyIsLiveStream: true,
            MPNowPlayingInfoPropertyPlaybackRate: running ? 1.0 : 0.0,
        ]
        MPNowPlayingInfoCenter.default().playbackState = running ? .playing : .paused
    }
}
