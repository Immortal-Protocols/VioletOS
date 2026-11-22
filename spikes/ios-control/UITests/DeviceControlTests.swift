import XCTest

/// Hands-free device control used by the relay tests: lock, unlock and screenshots.
final class DeviceControlTests: XCTestCase {
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")

    private func lock() {
        // XCUIDevice has no public lock API; the private selector presses the side button.
        XCUIDevice.shared.perform(NSSelectorFromString("pressLockButton"))
    }

    private func unlock() {
        XCUIDevice.shared.press(.home)            // wake the screen
        sleep(1)
        XCUIDevice.shared.press(.home)            // dismiss the lock screen (no passcode)
    }

    private func attachScreenshot(_ name: String) {
        let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    func testLaunchLockUnlock() throws {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.staticTexts["launch-time"].waitForExistence(timeout: 10))
        attachScreenshot("1-launched")

        lock()
        let left = NSPredicate { _, _ in app.state != .runningForeground }
        let leftForeground = expectation(for: left, evaluatedWith: nil)
        wait(for: [leftForeground], timeout: 15)
        attachScreenshot("2-locked")

        unlock()
        app.activate()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 10))
        attachScreenshot("3-unlocked")
    }
}
