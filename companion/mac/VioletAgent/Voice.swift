// Speech on the Mac: on-device recognition of the phone's microphone stream,
// and synthesis of the replies as 24 kHz PCM for the phone's speaker.
import AVFoundation
import Foundation
import Speech

/// Recognizes the 16 kHz mono stream from the phone. A turn ends after
/// `pause` seconds without new words (or when the recognizer says it is final).
final class Listener {
    var onPartial: (String) -> Void = { _ in }
    var onFinal: (String) -> Void = { _ in }
    var pause: TimeInterval = 0.75

    private let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "en-US"))
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var text = ""
    private var changed = Date()
    private var started = Date()
    private var timer: Timer?
    private let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16000, channels: 1, interleaved: false)!
    private(set) var authorized = false
    /// The latest chunk's peak, 0...1.
    private(set) var level: Float = 0

    func start(_ ready: @escaping (Bool) -> Void) {
        SFSpeechRecognizer.requestAuthorization { status in
            DispatchQueue.main.async {
                self.authorized = status == .authorized
                if self.authorized { self.begin() }
                self.timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in self?.tick() }
                ready(self.authorized)
            }
        }
    }

    private func begin() {
        guard let recognizer, recognizer.isAvailable else { return }
        let r = SFSpeechAudioBufferRecognitionRequest()
        r.shouldReportPartialResults = true
        if recognizer.supportsOnDeviceRecognition { r.requiresOnDeviceRecognition = true }
        r.taskHint = .dictation
        r.addsPunctuation = true
        request = r
        text = ""
        started = Date()
        let me = r
        task = recognizer.recognitionTask(with: r) { [weak self] result, error in
            DispatchQueue.main.async {
                guard let self, self.request === me else { return }
                if let result {
                    let t = result.bestTranscription.formattedString
                    if t != self.text {
                        self.text = t
                        self.changed = Date()
                        self.onPartial(t)
                    }
                    if result.isFinal { self.commit() }
                } else if error != nil {
                    self.restart()
                }
            }
        }
    }

    private func restart() {
        task?.cancel()
        task = nil
        request = nil
        begin()
    }

    private func commit() {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        restart()
        if !t.isEmpty { onFinal(t) }
    }

    /// Drop what was heard so far (it was the agent's own voice).
    func discard() {
        restart()
    }

    private func tick() {
        guard authorized else { return }
        if !text.isEmpty && Date().timeIntervalSince(changed) > pause {
            commit()
        } else if text.isEmpty && Date().timeIntervalSince(started) > 50 {
            restart() // tasks have a time limit; start a fresh one while it is quiet
        }
    }

    func feed(_ pcm: Data) {
        let n = pcm.count / 2
        guard n > 0, let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(n)) else { return }
        buf.frameLength = AVAudioFrameCount(n)
        var peak: Float = 0
        pcm.withUnsafeBytes { raw in
            let s = raw.bindMemory(to: Int16.self)
            let out = buf.floatChannelData![0]
            for i in 0..<n {
                let v = Float(Int16(littleEndian: s[i])) / 32768
                out[i] = v
                peak = max(peak, abs(v))
            }
        }
        level = peak
        request?.append(buf)
    }
}

/// Turns reply text into 24 kHz s16 mono, one sentence at a time.
final class Speaker {
    var onPCM: (UInt32, Data) -> Void = { _, _ in }
    var onEnd: (UInt32) -> Void = { _ in }

    private let synth = AVSpeechSynthesizer()
    private var queue: [(UInt32, String)] = []
    private var busy = false
    private var nextID: UInt32 = 1
    private var generation = 0
    private let target = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 24000, channels: 1, interleaved: true)!
    private let voice: AVSpeechSynthesisVoice? = {
        let english = AVSpeechSynthesisVoice.speechVoices().filter { $0.language == "en-US" }
        let ranked = english.sorted { a, b in
            func score(_ v: AVSpeechSynthesisVoice) -> Int {
                var s = v.quality == .premium ? 20 : v.quality == .enhanced ? 10 : 0
                if ["Zoe", "Ava", "Evan", "Nathan", "Samantha"].contains(where: { v.name.hasPrefix($0) }) { s += 3 }
                if v.voiceTraits.contains(.isNoveltyVoice) { s -= 50 }
                return s
            }
            return score(a) > score(b)
        }
        return ranked.first ?? AVSpeechSynthesisVoice(language: "en-US")
    }()

    var voiceName: String { voice?.name ?? "default" }
    var speaking: Bool { busy || !queue.isEmpty }

    /// Queue a sentence; returns its utterance id.
    @discardableResult
    func say(_ text: String) -> UInt32 {
        let id = nextID
        nextID += 1
        queue.append((id, text))
        pump()
        return id
    }

    func stop() {
        queue.removeAll()
        generation += 1
        synth.stopSpeaking(at: .immediate)
        busy = false
    }

    private func pump() {
        guard !busy, !queue.isEmpty else { return }
        let (id, text) = queue.removeFirst()
        busy = true
        let gen = generation
        let u = AVSpeechUtterance(string: text)
        u.voice = voice
        u.rate = AVSpeechUtteranceDefaultSpeechRate * 1.05
        var converter: AVAudioConverter?
        synth.write(u) { [weak self] buffer in
            guard let self else { return }
            guard let pcm = buffer as? AVAudioPCMBuffer else { return }
            if pcm.frameLength == 0 {
                DispatchQueue.main.async {
                    guard gen == self.generation else { return }
                    self.onEnd(id)
                    self.busy = false
                    self.pump()
                }
                return
            }
            if converter == nil || converter?.inputFormat != pcm.format {
                converter = AVAudioConverter(from: pcm.format, to: self.target)
            }
            guard let conv = converter else { return }
            let ratio = 24000 / pcm.format.sampleRate
            let cap = AVAudioFrameCount(Double(pcm.frameLength) * ratio + 64)
            guard let out = AVAudioPCMBuffer(pcmFormat: self.target, frameCapacity: cap) else { return }
            var fed = false
            var err: NSError?
            conv.convert(to: out, error: &err) { _, status in
                if fed { status.pointee = .noDataNow; return nil }
                fed = true
                status.pointee = .haveData
                return pcm
            }
            let bytes = Int(out.frameLength) * 2
            guard bytes > 0, let p = out.int16ChannelData?[0] else { return }
            let data = Data(bytes: p, count: bytes)
            DispatchQueue.main.async {
                guard gen == self.generation else { return }
                self.onPCM(id, data)
            }
        }
    }
}
