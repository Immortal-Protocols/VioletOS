import XCTest

/// Hands-free driver: launches Violet Relay and taps through any system prompts
/// (Local Network "Allow", Wi-Fi "Join"). Parameters come from the test runner environment;
/// `TEST_RUNNER_<NAME>` set on the xcodebuild command line arrives here as `<NAME>`.
///   RELAY_SECRET       pairing secret passed to the app (-pairingSecret)
///   JOIN_SSID          network to join via NEHotspotConfiguration (-joinSSID)
///   JOIN_PASSPHRASE    WPA2 passphrase, empty = open network (-joinPassphrase)
///   FORGET_SSID        remove a stored hotspot configuration (-forgetSSID)
///   RELAY_HOLD_SECONDS keep the app in the foreground this long before the test ends
final class RelayUITests: XCTestCase {
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    private let env = ProcessInfo.processInfo.environment
    private let acceptLabels = ["Allow", "Join", "OK", "Allow While Using App", "Continue"]

    override func setUp() {
        continueAfterFailure = true
    }

    private func launchArguments() -> [String] {
        var args: [String] = []
        if let s = env["RELAY_SECRET"], !s.isEmpty { args += ["-pairingSecret", s] }
        if let s = env["JOIN_SSID"], !s.isEmpty { args += ["-joinSSID", s, "-joinPassphrase", env["JOIN_PASSPHRASE"] ?? ""] }
        if let s = env["FORGET_SSID"], !s.isEmpty { args += ["-forgetSSID", s] }
        return args
    }

    /// Tap the first accept-style button on any springboard alert. Returns the alert text it handled.
    @discardableResult
    private func acceptSystemAlerts() -> [String] {
        var handled: [String] = []
        for _ in 0..<5 {
            let alert = springboard.alerts.firstMatch
            guard alert.exists else { break }
            let text = alert.label
            guard let label = acceptLabels.first(where: { alert.buttons[$0].exists }) else {
                XCTFail("unhandled alert '\(text)' buttons: \(alert.buttons.allElementsBoundByIndex.map(\.label))")
                break
            }
            alert.buttons[label].tap()
            handled.append("\(text) -> \(label)")
            sleep(1)
        }
        return handled
    }

    private func attachScreenshot(_ name: String) {
        let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    func testLaunchAndGrant() throws {
        let app = XCUIApplication()
        app.launchArguments = launchArguments()
        app.launch()

        let listener = app.staticTexts["listener-state"]
        XCTAssertTrue(listener.waitForExistence(timeout: 15))
        let joining = env["JOIN_SSID"].map { !$0.isEmpty } ?? false
        let deadline = Date().addingTimeInterval(joining ? 45 : 20)
        var handled: [String] = []
        while Date() < deadline {
            handled += acceptSystemAlerts()
            let hotspot = app.staticTexts["hotspot"].label
            let joinSettled = !joining || hotspot.hasPrefix("joined") || hotspot.contains("failed")
            if listener.label.hasPrefix("listening"), joinSettled, Date() > deadline.addingTimeInterval(-12) { break }
            sleep(1)
        }
        print("RELAY handled alerts: \(handled)")
        print("RELAY listener: \(listener.label) wifi: \(app.staticTexts["wifi-ip"].label) hotspot: \(app.staticTexts["hotspot"].label)")
        attachScreenshot("relay-running")
        XCTAssertTrue(listener.label.hasPrefix("listening"), "listener: \(listener.label)")

        if let hold = env["RELAY_HOLD_SECONDS"].flatMap(Double.init), hold > 0 {
            let end = Date().addingTimeInterval(hold)
            while Date() < end { acceptSystemAlerts(); sleep(2) }
        }
    }
}
