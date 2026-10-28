// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

final class LevelTextTests: XCTestCase {
    func testFormat() {
        XCTAssertEqual(LevelText.format(0), "0.0")
        XCTAssertEqual(LevelText.format(-6.04), "-6.0")
        XCTAssertEqual(LevelText.format(12), "12.0")
        XCTAssertEqual(LevelText.format(-120), "-inf")
        XCTAssertEqual(LevelText.format(-200), "-inf")
        XCTAssertEqual(LevelText.format(.nan), "-inf")
        XCTAssertEqual(LevelText.format(-0.01), "0.0", "no negative zero")
    }

    /// QLab 5 manual pp. 181-182 and 200.
    func testParseFollowsQLab() {
        XCTAssertEqual(LevelText.parse("6"), -6, "an unsigned number is negative")
        XCTAssertEqual(LevelText.parse("-6.5"), -6.5)
        XCTAssertEqual(LevelText.parse("+6"), 6)
        XCTAssertEqual(LevelText.parse("0"), 0)
        XCTAssertEqual(LevelText.parse("6,5"), -6.5, "a decimal comma")
        XCTAssertEqual(LevelText.parse("-3 dB"), -3)
        XCTAssertEqual(LevelText.parse("+30"), 12, "held at the maximum")
        XCTAssertEqual(LevelText.parse("-60"), LevelText.silence, "at the lower limit is silent")
        XCTAssertEqual(LevelText.parse("-80"), LevelText.silence)
        XCTAssertEqual(LevelText.parse("-inf"), LevelText.silence)
        XCTAssertEqual(LevelText.parse(""), LevelText.silence, "non-numeric text is -inf")
        XCTAssertEqual(LevelText.parse("loud"), LevelText.silence)
        XCTAssertEqual(LevelText.parse("nan"), LevelText.silence)
        XCTAssertEqual(LevelText.parse("-1e999"), LevelText.silence)
        XCTAssertEqual(LevelText.parse("+1e999"), 12)
    }

    func testClamp() {
        XCTAssertEqual(LevelText.clamp(-59.9), -59.9)
        XCTAssertEqual(LevelText.clamp(-60), -120)
        XCTAssertEqual(LevelText.clamp(-.infinity), -120)
        XCTAssertEqual(LevelText.clamp(.infinity), 12)
        XCTAssertEqual(LevelText.clamp(.nan), -120)
    }
}

@MainActor
final class LevelSetTests: XCTestCase {
    private func kind(_ name: String) throws -> JSONValue {
        let snapshot = try Fixtures.json("server", "welcome")["snapshot"]
        let cues = try XCTUnwrap(snapshot?["workspace"]?["cue_lists"]?.arrayValue?.first?["cues"]?.arrayValue)
        return try XCTUnwrap(cues.first { $0["kind"]?["type"]?.stringValue == name }?["kind"])
    }

    private func audio() -> LevelSet {
        var s = LevelSet(kind: FakeEngine.audioKind("a.wav"))!
        s.outputs = (0..<4).map { _ in LevelSet.Output(db: 0) }
        return s
    }

    /// The kinds the engine wrote for the protocol fixtures survive a read and write unchanged: the
    /// encoding is checked against core's real output, not hand-written JSON.
    func testRoundTripsCoreFixtures() throws {
        for name in ["audio", "fade"] {
            let k = try kind(name)
            let set = try XCTUnwrap(LevelSet(kind: k), name)
            XCTAssertEqual(normalised(set.json(into: k)), normalised(k), name)
        }
        XCTAssertNil(LevelSet(kind: .object(["type": .string("memo")])))
        XCTAssertNil(LevelSet(kind: .object(["type": .string("audio")])), "no levels")
    }

    func testKindCommandEnvelopeMatchesCoreFixture() throws {
        let fixture = try Fixtures.json("client", "command_set_devamp_kind")["command"]
        let k = try XCTUnwrap(fixture?["property"]?["value"])
        let command = Command.setProperty("00000000-0000-0000-0000-00000000000c", .kind(k))
        XCTAssertEqual(JSONValue.parse(try command.json.text()), fixture)
    }

    func testAudioRoutingRoundTrip() throws {
        let k = FakeEngine.audioKind("a.wav")
        var set = try XCTUnwrap(LevelSet(kind: k))
        set.set(.cross(input: 1, output: 0), db: -6)
        set.set(.cross(input: 0, output: 1), db: -3)
        set.setMute(.cross(input: 0, output: 1), true)
        set.setGang(of: 0, letter: "a")
        set.setGang(of: 1, letter: "A")
        let again = try XCTUnwrap(LevelSet(kind: set.json(into: k)))
        XCTAssertEqual(again, set)
        let points = set.json(into: k)["levels"]?["crosspoints"]?.arrayValue
        XCTAssertEqual(points?.count, 2)
        XCTAssertEqual(points?.first?["input"], .int(0), "sorted by file channel, then output")
        XCTAssertEqual(points?.first?["mute"], .bool(true))
        XCTAssertEqual(set.json(into: k)["levels"]?["gangs"], .array([.array([.int(0), .int(1)])]))
    }

    func testSilentCrosspointIsDroppedUnlessMuted() {
        var s = audio()
        s.set(.cross(input: 0, output: 0), db: -3)
        XCTAssertEqual(s.crosspoints.count, 1)
        s.set(.cross(input: 0, output: 0), db: LevelText.silence)
        XCTAssertTrue(s.crosspoints.isEmpty, "absent is silent")
        s.set(.cross(input: 0, output: 0), db: -3)
        s.setMute(.cross(input: 0, output: 0), true)
        s.set(.cross(input: 0, output: 0), db: LevelText.silence)
        XCTAssertEqual(s.crosspoints.count, 1, "a mute is kept with its route")
        s.setMute(.cross(input: 5, output: 5), true)
        XCTAssertEqual(s.crosspoints.count, 1, "muting a route that is not listed does nothing")
    }

    func testMuteKeepsTheLevel() {
        var s = audio()
        s.set(.output(1), db: -9)
        s.setMute(.output(1), true)
        XCTAssertEqual(s.db(.output(1)), -9)
        s.setMute(.output(1), false)
        XCTAssertEqual(s.db(.output(1)), -9)
    }

    func testGangMovesMembersTogetherAndHoldsEachAtItsLimit() {
        var s = audio()
        s.set(.output(0), db: -10)
        s.set(.output(1), db: -2)
        s.setGang(of: 0, letter: "A")
        s.setGang(of: 1, letter: "A")
        s.setGang(of: 3, letter: "B")
        XCTAssertEqual(s.gangs, [[0, 1], [3]])
        s.set(.output(0), db: -7)  // +3
        XCTAssertEqual(s.db(.output(1)), 1)
        s.set(.output(0), db: 9)  // +16: output 1 stops at the maximum, output 0 goes on
        XCTAssertEqual(s.db(.output(0)), 9)
        XCTAssertEqual(s.db(.output(1)), 12)
        s.set(.output(2), db: -20)
        XCTAssertEqual(s.db(.output(0)), 9, "outputs outside the gang stay")
        s.setGang(of: 0, letter: "")
        XCTAssertEqual(s.gangs, [[1], [3]])
        s.setGang(of: 3, letter: "")
        s.setGang(of: 1, letter: "")
        XCTAssertTrue(s.gangs.isEmpty)
    }

    func testSilenceAll() {
        var s = audio()
        s.set(.master, db: -3)
        s.set(.cross(input: 0, output: 0), db: 0)
        s.silenceAll()
        XCTAssertEqual(s.master, LevelText.silence)
        XCTAssertTrue(s.outputs.allSatisfy { $0.db == LevelText.silence })
        XCTAssertTrue(s.crosspoints.isEmpty)
        XCTAssertEqual(s.outputs.count, 4, "the cue keeps its outputs")
    }

    func testFadeLevelsAreActiveOnlyWhenSet() throws {
        let k = try kind("fade")
        var s = try XCTUnwrap(LevelSet(kind: k))
        s.deactivate(.master)
        XCTAssertFalse(s.isActive(.master))
        XCTAssertEqual(s.json(into: k)["levels"]?["master_db"], .null)
        s.set(.output(2), db: -6)
        XCTAssertEqual(s.json(into: k)["levels"]?["output_db"], .array([.null, .null, .double(-6)]))
        s.set(.cross(input: 1, output: 0), db: LevelText.silence)
        XCTAssertTrue(s.isActive(.cross(input: 1, output: 0)), "a fade to -inf is a fade")
        let cross = s.json(into: k)["levels"]?["crosspoints"]?.arrayValue?.first
        XCTAssertEqual(cross, .object(["input": .int(1), "output": .int(0), "db": .double(-120)]), "no mute in a fade")
        s.deactivate(.output(2))
        XCTAssertEqual(s.json(into: k)["levels"]?["output_db"], .array([]), "trailing inactive entries are trimmed")
        s.setMute(.output(0), true)
        XCTAssertFalse(s.isMuted(.output(0)), "a fade never changes mutes")
        s.silenceAll()
        XCTAssertNil(s.master)
        XCTAssertTrue(s.crosspoints.isEmpty)
    }

    func testChannelLimitsHold() {
        var s = audio()
        s.set(.output(64), db: 0)
        s.set(.cross(input: 64, output: 0), db: 0)
        s.set(.cross(input: 0, output: 64), db: 0)
        XCTAssertEqual(s.outputs.count, 4)
        XCTAssertTrue(s.crosspoints.isEmpty)
        var raw = FakeEngine.audioKind("a.wav")
        raw["levels"]?.set(
            "crosspoints",
            .array([.object(["input": .int(70), "output": .int(0), "db": .double(0), "mute": .bool(false)])]))
        XCTAssertEqual(LevelSet(kind: raw)?.crosspoints.count, 0, "a damaged entry is ignored, not trusted")
    }
}
