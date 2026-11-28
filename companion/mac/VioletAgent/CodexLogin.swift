import Foundation

/// The companion uses one Codex login across phone profiles. Rose supplies its own
/// permission profile, which can conflict with the user's regular Codex settings.
struct CodexLogin {
    let home: URL
    let binary: String

    private static let path = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"

    var environment: [String: String] {
        var environment = ProcessInfo.processInfo.environment
        environment["CODEX_HOME"] = home.path
        environment.removeValue(forKey: "CODEX_SQLITE_HOME")
        environment["PATH"] = [Self.path, environment["PATH"]].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: ":")
        return environment
    }

    func prepare() throws {
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
    }

    struct TerminalCommand {
        let directory: URL
        var script: URL { directory.appendingPathComponent("Sign in to Violet Agent.command") }
        fileprivate var result: URL { directory.appendingPathComponent("exit-status") }
        fileprivate var cancellation: URL { directory.appendingPathComponent("cancel") }

        var exitStatus: Int? {
            guard let status = try? String(contentsOf: result, encoding: .utf8) else { return nil }
            return Int(status.trimmingCharacters(in: .whitespacesAndNewlines))
        }

        func cancel() throws { try Data().write(to: cancellation, options: .atomic) }

        func cleanUp() { try? FileManager.default.removeItem(at: directory) }
    }

    func terminalCommand() throws -> TerminalCommand {
        let fm = FileManager.default
        let directory = fm.temporaryDirectory.appendingPathComponent("violet-codex-\(UUID().uuidString)")
        try fm.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        let command = TerminalCommand(directory: directory)
        let script = """
        #!/bin/sh
        export PATH=\(Self.path):"$PATH"
        export CODEX_HOME=\(Self.quote(home.path))
        unset CODEX_SQLITE_HOME
        result=\(Self.quote(command.result.path))
        cancel=\(Self.quote(command.cancellation.path))
        login_pid=
        finish() {
          status=$?
          if [ -n "$login_pid" ]; then
            kill "$login_pid" 2>/dev/null
            wait "$login_pid" 2>/dev/null
          fi
          printf '%s\\n' "$status" > "$result.tmp" && mv -f "$result.tmp" "$result"
        }
        trap finish EXIT
        trap 'exit 130' HUP INT TERM
        [ ! -e "$cancel" ] || exit 130
        # Keep Terminal input attached while checking for a retry from the app.
        exec 3<&0
        \(Self.quote(binary)) login <&3 3<&- &
        login_pid=$!
        exec 3<&-
        while kill -0 "$login_pid" 2>/dev/null; do
          [ ! -e "$cancel" ] || exit 130
          sleep 0.2
        done
        wait "$login_pid"
        status=$?
        login_pid=
        exit "$status"
        """ + "\n"
        do {
            try script.write(to: command.script, atomically: true, encoding: .utf8)
            try fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: command.script.path)
        } catch {
            command.cleanUp()
            throw error
        }
        return command
    }

    private static func quote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }
}
