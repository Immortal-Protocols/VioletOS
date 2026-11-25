import XCTest
@testable import VioletRelay

final class RelayWiFiStatusTests: XCTestCase {
    private let violetSSID = "VioletRelayOpen-ABCD"

    func testMatchingVioletNetworkWaitsForItsAddress() {
        XCTAssertEqual(RelayWiFiStatus.observed(
            targetSSID: violetSSID, currentSSID: violetSSID,
            wifiIPv4: ["169.254.11.24"], requiresVioletLease: true
        ), .awaitingAddress)
        XCTAssertEqual(RelayWiFiStatus.observed(
            targetSSID: violetSSID, currentSSID: violetSSID,
            wifiIPv4: ["192.168.77.2"], requiresVioletLease: true
        ), .connected)
    }

    func testLateLeaseConfirmsConnectionWithSSIDUnavailable() {
        XCTAssertEqual(RelayWiFiStatus.observed(
            targetSSID: violetSSID, currentSSID: nil, wifiIPv4: [], requiresVioletLease: true
        ), .waitingForNetwork)
        XCTAssertEqual(RelayWiFiStatus.observed(
            targetSSID: violetSSID, currentSSID: nil,
            wifiIPv4: ["192.168.77.2"], requiresVioletLease: true
        ), .connected)
    }

    func testDifferentSSIDWithSameAddressIsNotThePairedNetwork() {
        XCTAssertEqual(RelayWiFiStatus.observed(
            targetSSID: violetSSID, currentSSID: "VioletRelayOpen-1234",
            wifiIPv4: ["192.168.77.2"], requiresVioletLease: true
        ), .waitingForNetwork)
    }

    func testGenericNetworkRequiresMatchingSSIDButNoVioletLease() {
        XCTAssertEqual(RelayWiFiStatus.observed(
            targetSSID: "Test Wi-Fi", currentSSID: "Test Wi-Fi",
            wifiIPv4: [], requiresVioletLease: false
        ), .connected)
        XCTAssertEqual(RelayWiFiStatus.observed(
            targetSSID: "Test Wi-Fi", currentSSID: nil,
            wifiIPv4: ["192.168.77.2"], requiresVioletLease: false
        ), .waitingForNetwork)
    }
}
