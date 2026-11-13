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
        var s = LevelSet(kind: TestSnapshots.audioKind("a.wav"))!
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

    func testKindFieldsCommandEnvelopeMatchesCoreFixture() throws {
        let fixture = try Fixtures.json("client", "command_set_kind_fields")["command"]
        let fields = try XCTUnwrap(fixture?["property"]?["value"]?.objectValue)
        let command = Command.setProperty("00000000-0000-0000-0000-00000000000a", .kindFields(fields))
        XCTAssertEqual(JSONValue.parse(try command.json.text()), fixture)
    }

    func testAudioRoutingRoundTrip() throws {
        let k = TestSnapshots.audioKind("a.wav")
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

    /// Cue outputs go to 128 and file channels to 64. A level or route the set cannot hold (beyond
    /// those, or not well formed) is written back untouched by any edit; only the whole-matrix
    /// actions (set all silent, set defaults) drop it.
    func testLevelsBeyondTheBoundsAreWrittenBackUntouched() throws {
        var k = TestSnapshots.audioKind("a.wav")
        let route = { (i: Int64, o: Int64, db: Double) -> JSONValue in
            .object(["input": .int(i), "output": .int(o), "db": .double(db), "mute": .bool(false)])
        }
        let beyond: [JSONValue] = [
            route(70, 0, -3), route(0, 200, -4), .object(["input": .int(1), "output": .int(1)]),
        ]
        var levels = try XCTUnwrap(k["levels"])
        levels.set("crosspoints", .array([route(0, 127, -6), route(63, 0, -9)] + beyond))
        levels.set("outputs", .array(Array(repeating: .object(["db": .double(-1), "mute": .bool(false)]), count: 128)))
        k.set("levels", levels)
        var set = try XCTUnwrap(LevelSet(kind: k))
        XCTAssertEqual(set.crosspoints[.init(input: 0, output: 127)]?.db, -6, "output 128 is held")
        XCTAssertEqual(set.crosspoints[.init(input: 63, output: 0)]?.db, -9, "channel 64 is held")
        XCTAssertEqual(set.unread, beyond)
        set.set(.output(127), db: -2)
        set.set(.output(128), db: -2)
        set.set(.cross(input: 64, output: 0), db: -2)
        XCTAssertEqual(set.outputs.count, 128, "nothing beyond output 128 is made")
        XCTAssertNil(set.crosspoints[.init(input: 64, output: 0)])
        set.set(.master, db: -6)
        let written = set.json(into: k)["levels"]
        XCTAssertEqual(written?["outputs"]?.arrayValue?.count, 128)
        XCTAssertEqual(written?["outputs"]?.arrayValue?[127]["db"], .double(-2))
        let points = written?["crosspoints"]?.arrayValue ?? []
        XCTAssertEqual(points.count, 5)
        XCTAssertEqual(Array(points.suffix(3)), beyond, "what the set cannot hold goes back as it came")
        set.silenceAll()
        XCTAssertEqual(set.json(into: k)["levels"]?["crosspoints"], .array([]))
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

    /// Regression: a ganged member at -inf is at the fader's floor (-60), not 120 dB under it.
    func testGangDragFromSilenceMovesPartnersByWhatTheControlMoved() {
        var s = audio()
        s.set(.output(0), db: -10)
        s.set(.output(1), db: LevelText.silence)
        s.setGang(of: 0, letter: "A")
        s.setGang(of: 1, letter: "A")
        s.set(.output(1), db: -50, drag: true)  // up 10 from the floor
        XCTAssertEqual(s.db(.output(0)), 0, "the partner moves by 10, not to +12")
        s.set(.output(1), db: LevelText.silence, drag: true)  // back to the floor: down 10
        XCTAssertEqual(s.db(.output(0)), -10, "not to silence")
        s.set(.output(0), db: -20, drag: true)  // down 10: the other is at the floor already
        XCTAssertEqual(s.db(.output(1)), LevelText.silence)
        s.set(.output(0), db: -10, drag: true)
        XCTAssertEqual(s.db(.output(1)), -50, "and comes back up with it")

        var t = audio()
        t.set(.output(0), db: -6)
        t.set(.output(1), db: -2)
        t.setGang(of: 0, letter: "A")
        t.setGang(of: 1, letter: "A")
        t.set(.output(0), db: 0, drag: true)
        XCTAssertEqual(t.db(.output(1)), 0, "a drag holds partners at the 0 dB ceiling")
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

    /// Cue outputs go to 128 (a patch's most) and file channels to 64: nothing beyond either is
    /// made by an edit, and an entry read beyond them is not trusted as a level (it is only written
    /// back as it came).
    func testChannelLimitsHold() {
        var s = audio()
        s.set(.output(128), db: 0)
        s.set(.cross(input: 64, output: 0), db: 0)
        s.set(.cross(input: 0, output: 128), db: 0)
        XCTAssertEqual(s.outputs.count, 4, "nothing made beyond output 128")
        XCTAssertTrue(s.crosspoints.isEmpty, "nothing made beyond channel 64 or output 128")
        s.set(.output(127), db: 0)
        s.set(.cross(input: 63, output: 127), db: 0)
        XCTAssertEqual(s.outputs.count, 128, "output 128 is held")
        XCTAssertEqual(Array(s.crosspoints.keys), [.init(input: 63, output: 127)], "channel 64 to output 128 is held")
        var raw = TestSnapshots.audioKind("a.wav")
        let damaged = JSONValue.object(["input": .int(70), "output": .int(0), "db": .double(0), "mute": .bool(false)])
        raw["levels"]?.set("crosspoints", .array([damaged]))
        let read = LevelSet(kind: raw)
        XCTAssertEqual(read?.crosspoints.count, 0, "a damaged entry is ignored, not trusted")
        XCTAssertEqual(read?.unread, [damaged], "but kept to be written back")
    }

    /// "Set defaults" on a cue of 100 outputs routes file channel n to output n only for the
    /// channels a file can have (64): a route from channel 65 or more is one the engine refuses,
    /// and with it the whole edit.
    func testSetDefaultsRoutesOnlyTheChannelsAFileCanHave() {
        var s = audio()
        s.set(.output(99), db: -6)
        XCTAssertEqual(s.outputs.count, 100)
        s.setDefaults()
        XCTAssertEqual(s.crosspoints.count, 64)
        XCTAssertTrue(s.crosspoints.keys.allSatisfy { $0.input == $0.output && $0.input < 64 })
        XCTAssertTrue(s.outputs.allSatisfy { $0.db == 0 })
    }
}
