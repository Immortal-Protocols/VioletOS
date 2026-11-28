// Compile alongside CodexLogin.swift and run the resulting host executable.
import Foundation

@main
enum CodexLoginHostTest {
    private enum Outcome { case signedIn, canceled, restarted }

    static func main() throws {
        let fm = FileManager.default
        let root = fm.temporaryDirectory.appendingPathComponent("Violet Agent's test \(UUID())")
        let home = root.appendingPathComponent("Codex profile")
        let cli = root.appendingPathComponent("test's codex")
        let login = CodexLogin(home: home, binary: cli.path)
        try login.prepare()
        defer { try? fm.removeItem(at: root) }
        precondition(login.environment["CODEX_HOME"] == home.path)
        precondition(login.environment["CODEX_SQLITE_HOME"] == nil)

        try ("""
        #!/bin/sh
        [ "$1" = login ] || exit 2
        [ -z "${CODEX_SQLITE_HOME+x}" ] || exit 3
        [ "$VIOLET_TEST_LOGIN_CANCELED" = 1 ] && exit 130
        if [ "$VIOLET_TEST_LOGIN_WAIT" = 1 ]; then
          touch "$CODEX_HOME/login-started"
          exec sleep 10
        fi
        touch "$CODEX_HOME/signed-in"
        """ + "\n").write(to: cli, atomically: true, encoding: .utf8)
        try fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: cli.path)

        let receipt = home.appendingPathComponent("signed-in")
        try run(login, outcome: .canceled)
        precondition(!fm.fileExists(atPath: receipt.path))
        try run(login, outcome: .restarted)
        precondition(!fm.fileExists(atPath: receipt.path))
        try run(login, outcome: .signedIn)
        precondition(fm.fileExists(atPath: receipt.path))
    }

    private static func run(_ login: CodexLogin, outcome: Outcome) throws {
        let command = try login.terminalCommand()
        defer { command.cleanUp() }
        precondition(command.exitStatus == nil)

        let process = Process()
        process.executableURL = command.script
        var environment = ProcessInfo.processInfo.environment
        environment["CODEX_HOME"] = "/outside-violet"
        environment["CODEX_SQLITE_HOME"] = "/outside-violet"
        environment["VIOLET_TEST_LOGIN_CANCELED"] = outcome == .canceled ? "1" : "0"
        environment["VIOLET_TEST_LOGIN_WAIT"] = outcome == .restarted ? "1" : "0"
        process.environment = environment
        try process.run()
        if outcome == .restarted {
            let started = login.home.appendingPathComponent("login-started")
            let deadline = Date().addingTimeInterval(3)
            while !FileManager.default.fileExists(atPath: started.path), process.isRunning, Date() < deadline {
                Thread.sleep(forTimeInterval: 0.05)
            }
            let running = process.isRunning && FileManager.default.fileExists(atPath: started.path)
            try command.cancel()
            precondition(running)
        }
        process.waitUntilExit()
        precondition(process.terminationStatus == (outcome == .signedIn ? 0 : 130))
        precondition(command.exitStatus == Int(process.terminationStatus))
    }
}
