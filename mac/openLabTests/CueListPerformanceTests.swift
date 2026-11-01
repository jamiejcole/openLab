// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// NFR-24: a 2,000-cue workspace opens fast and edits without lag. The deadlines are generous (tens
/// of times the measured cost on a laptop) so a slow CI runner does not flake; the scaling test
/// compares the same work at 200 and 2,000 cues, which does not depend on the machine's speed.
@MainActor
final class CueListPerformanceTests: XCTestCase {
    typealias H = CueListHarness

    private func seconds(_ body: () throws -> Void) rethrows -> Double {
        let clock = ContinuousClock()
        let d = try clock.measure(body)
        return Double(d.components.seconds) + Double(d.components.attoseconds) / 1e18
    }

    private func bigHarness(_ n: Int) async throws -> CueListHarness {
        try await H(snapshot: TestSnapshots.bigSnapshot(cues: n), collapsed: false)
    }

    /// `count` renames spread across the list, as decoded messages (revision filled in on send).
    private func renames(_ h: CueListHarness, count: Int) throws -> [String] {
        let cues = h.store.model?.cueLists.first?.cues ?? []
        return try (0..<count).map { i in
            let c = cues[(i * 7 + 3) % cues.count]
            let json = TestSnapshots.cueJSON(
                id: c.id.raw, number: c.number, name: "Renamed \(i)", kind: TestSnapshots.audioKind("media/x.wav"))
            return try H.upsertJSON(json, index: (i * 7 + 3) % cues.count)
        }
    }

    private func send(_ h: CueListHarness, _ upserts: [String]) throws {
        for u in upserts { try h.change(upserts: [u]) }
    }

    func testWorkspaceOf2000CuesBuilds() async throws {
        let h = try await bigHarness(2000)
        XCTAssertEqual(h.vc.nodeCount, 2000)
        XCTAssertEqual(h.spy.numberOfRows, 2000)  // groups open expanded
    }

    func testSnapshotDecodeAndApplyOf2000CuesAreFast() async throws {
        let h = try await bigHarness(2000)
        var snap = TestSnapshots.bigSnapshot(cues: 2000)
        snap.set("revision", .int(1000))
        let text = try JSONValue.object(["type": .string("snapshot"), "snapshot": snap]).text()
        var message: ServerMessage?
        let decode = try seconds { message = try ServerMessage.decode(text) }
        let apply = seconds { h.store.receive(message ?? .unknown(type: "")) }
        print(
            "perf: 2000-cue snapshot, \(text.utf8.count / 1024) KiB: decode \(decode * 1000) ms, store+list apply \(apply * 1000) ms"
        )
        // Debug-build decode on a shared CI runner measured 2.2 s; this guards against a gross
        // regression only. Real decode cost is tracked by the off-main-actor decode task (M5.4).
        XCTAssertLessThan(decode, 8.0)
        XCTAssertLessThan(apply, 1.0)
        XCTAssertEqual(h.vc.nodeCount, 2000)
        XCTAssertEqual(h.spy.wholeReloads, 1)
    }

    func testHundredSingleCueUpsertsAt2000CuesAreFast() async throws {
        let h = try await bigHarness(2000)
        let upserts = try renames(h, count: 100)
        let total = try seconds { try send(h, upserts) }
        print("perf: 100 single-cue upserts at 2000 cues (decode + store + list): \(total * 1000) ms")
        XCTAssertLessThan(total, 2.0)
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }

    /// An edit in place looks at the same number of cues however long the list is. Counted, not
    /// timed, so it holds on any machine.
    func testEditInPlaceVisitsTheSameNumberOfCuesAtAnyListLength() async throws {
        func visits(_ n: Int) async throws -> Int {
            let h = try await bigHarness(n)
            let upserts = try renames(h, count: 50)
            WorkspaceStore.cueVisits = 0
            try send(h, upserts)
            return WorkspaceStore.cueVisits
        }
        let small = try await visits(200)
        let large = try await visits(2000)
        XCTAssertEqual(large, small)
        XCTAssertLessThanOrEqual(large, 50 * 2)
    }

    func testRuntimeAndProgressTicksAt2000CuesTouchOnlyTheirRows() async throws {
        let h = try await bigHarness(2000)
        let ids = (h.store.model?.cueLists.first?.cues ?? []).prefix(5).map(\.id)
        let items = ids.map { #"{"cue":"\#($0)","elapsed":1.0,"remaining":2.0}"# }.joined(separator: ",")
        let msg = try ServerMessage.decode(#"{"type":"progress","items":[\#(items)]}"#)
        h.spy.resetCounts()
        let t = seconds { for _ in 0..<100 { h.store.receive(msg) } }
        print("perf: 100 progress ticks of 5 rows at 2000 cues: \(t * 1000) ms")
        XCTAssertLessThan(t, 1.0)
        XCTAssertEqual(h.spy.rowsReloaded, 500)
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }
}
