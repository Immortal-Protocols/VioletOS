// The same Mac runs QEMU's socket bridge and this companion. Only that bridge
// receives the SOCKS credentials. The Agent still pairs with signed keys.
import AppKit
import Darwin
import Foundation

final class LocalCompanion {
    static let relay = "http://agent-relay.violet"

    private struct Connection: Encodable {
        let pid = ProcessInfo.processInfo.processIdentifier
        let relayPort: UInt16
        let socksPort: UInt16
        let password: String
    }

    private let repo: URL
    private let directory: URL
    private let invitation: () -> String?
    private let status: (UInt16?) -> Void
    private let password = UUID().uuidString + UUID().uuidString
    private var server: RelayServer?
    private var relay: Process?
    private var pipe: Pipe?
    private var readyBuffer = Data()
    private var lockFD: Int32 = -1
    private var observer: NSObjectProtocol?

    private var manifest: URL { directory.appendingPathComponent("connection.json") }

    init(repo: URL, invitation: @escaping () -> String?, status: @escaping (UInt16?) -> Void) {
        self.repo = repo
        directory = repo.appendingPathComponent("build/local-companion")
        self.invitation = invitation
        self.status = status
    }

    func start() {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            let lockPath = directory.appendingPathComponent("listener.lock").path
            let fd = open(lockPath, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, 0o600)
            guard fd >= 0 else { throw POSIXError(.EACCES) }
            guard flock(fd, LOCK_EX | LOCK_NB) == 0 else {
                close(fd)
                throw POSIXError(.EADDRINUSE)
            }
            lockFD = fd
            try? FileManager.default.removeItem(at: manifest)

            let output = Pipe()
            let child = Process()
            child.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
            child.arguments = [repo.appendingPathComponent("tools/chariot-relay/relay.py").path,
                               "--host", "127.0.0.1", "--port", "0"]
            child.standardOutput = output
            child.standardError = FileHandle.nullDevice
            pipe = output
            relay = child
            output.fileHandleForReading.readabilityHandler = { [weak self] handle in
                let bytes = handle.availableData
                if bytes.isEmpty { handle.readabilityHandler = nil; return }
                DispatchQueue.main.async { self?.relayOutput(bytes) }
            }
            child.terminationHandler = { [weak self] _ in
                DispatchQueue.main.async { self?.stop() }
            }
            try child.run()
            observer = NotificationCenter.default.addObserver(forName: NSApplication.willTerminateNotification,
                                                               object: nil, queue: .main) { [weak self] _ in
                self?.stop()
            }
        } catch {
            NSLog("Violet Agent local companion: \(error)")
            stop()
        }
    }

    private func relayOutput(_ bytes: Data) {
        guard lockFD >= 0, server == nil else { return }
        readyBuffer.append(bytes)
        guard let newline = readyBuffer.firstIndex(of: 10) else { return }
        let line = String(decoding: readyBuffer.prefix(upTo: newline), as: UTF8.self)
        guard line.contains("Violet Agent relay on 127.0.0.1:"),
              let field = line.split(separator: ":").last, let port = UInt16(field) else { return }

        let listener = RelayServer(port: 0, username: "violet", password: password, loopbackOnly: true) { [weak self] respond in
            DispatchQueue.main.async { respond(self?.invitation()) }
        }
        listener.onReady = { [weak self] socksPort in
            DispatchQueue.main.async { self?.publish(relayPort: port, socksPort: socksPort) }
        }
        server = listener
        listener.start()
    }

    private func publish(relayPort: UInt16, socksPort: UInt16) {
        guard lockFD >= 0, relay?.isRunning == true, socksPort != 0 else { return }
        let temporary = directory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: temporary) }
        do {
            let config = Connection(relayPort: relayPort, socksPort: socksPort, password: password)
            let data = try JSONEncoder().encode(config)
            guard FileManager.default.createFile(atPath: temporary.path, contents: data,
                                                 attributes: [.posixPermissions: 0o600]),
                  rename(temporary.path, manifest.path) == 0 else { throw POSIXError(.EIO) }
            status(relayPort)
        } catch {
            NSLog("Violet Agent local companion: \(error)")
            stop()
        }
    }

    private func stop() {
        if let observer { NotificationCenter.default.removeObserver(observer); self.observer = nil }
        pipe?.fileHandleForReading.readabilityHandler = nil
        pipe = nil
        relay?.terminationHandler = nil
        if relay?.isRunning == true { relay?.terminate() }
        relay = nil
        server?.stop()
        server = nil
        if lockFD >= 0 {
            try? FileManager.default.removeItem(at: manifest)
            close(lockFD)
            lockFD = -1
        }
        status(nil)
    }
}
