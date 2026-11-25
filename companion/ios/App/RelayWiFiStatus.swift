enum RelayWiFiStatus: Equatable {
    case connected
    case awaitingAddress
    case waitingForNetwork

    static let violetLease = "192.168.77.2"

    static func observed(targetSSID: String, currentSSID: String?, wifiIPv4: [String],
                         requiresVioletLease: Bool) -> Self {
        if let currentSSID, currentSSID != targetSSID { return .waitingForNetwork }
        if requiresVioletLease {
            if wifiIPv4.contains(violetLease) { return .connected }
            if currentSSID == targetSSID { return .awaitingAddress }
        } else if currentSSID == targetSSID {
            return .connected
        }
        return .waitingForNetwork
    }
}
