import XCTest
@testable import VioletRelay

final class RelayFlowSnapshotTests: XCTestCase {
    func testFlowLifetimeAndCoverage() throws {
        let host = String(repeating: "a", count: 45) + ".test"
        let flow = RelayFlowSnapshot.Flow(id: 7, target: host + ":443", ageMs: 12_000,
                                          up: 128, down: 64, endedMsAgo: 6000)
        let key = String(host.prefix(40)) + ":443"
        for source in ["Mac", "iPhone"] {
            let original = RelayFlowSnapshot(source: source, complete: true, flows: [flow])
            let snapshot = try JSONDecoder().decode(RelayFlowSnapshot.self, from: JSONEncoder().encode(original))
            func seen(_ time: Int64) -> Bool? {
                snapshot.observation(for: key, eventMs: time, sessionMs: 2000, sampleMs: 20_000)
            }
            XCTAssertNil(seen(1000))
            XCTAssertEqual(seen(3000), false)
            XCTAssertEqual(seen(9000), true)
            XCTAssertEqual(seen(17_000), false)
            XCTAssertNil(seen(19_500))
            XCTAssertNil(seen(25_000))

            let partial = RelayFlowSnapshot(source: source, complete: false, flows: [flow])
            XCTAssertEqual(partial.observation(for: key, eventMs: 9000, sessionMs: 2000, sampleMs: 20_000), true)
            XCTAssertNil(partial.observation(for: "another.test:443", eventMs: 9000, sessionMs: 2000, sampleMs: 20_000))
        }
    }
}
