// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The Video cue's stage property encodes exactly as the engine's golden fixtures, a picture that
/// failed mid-show decodes as its own broken reason, the I/O tab's frame rate reads as a frame rate,
/// and Paste Properties offers a duration only between cues whose duration is typed.
final class VideoCuePropertyTests: XCTestCase {
    private let cue: ModelID = "00000000-0000-0000-0000-00000000000f"

    /// Three decimals at most, trailing zeros dropped: the NTSC rates keep their fractions.
    func testTheFrameRateReadsToThreeDecimals() {
        let cases: [(Double, String)] = [
            (25, "25"), (29.97, "29.97"), (30000.0 / 1001, "29.97"), (23.976, "23.976"), (24000.0 / 1001, "23.976"),
            (59.94, "59.94"), (60000.0 / 1001, "59.94"), (24, "24"), (12.5, "12.5"), (.nan, "?"), (0, "?"),
        ]
        for (fps, shown) in cases { XCTAssertEqual(VideoIOControls.fpsText(fps), shown, "\(fps)") }
    }

    private func video(_ id: ModelID, duration: Double?) throws -> Cue {
        var kind = try XCTUnwrap(NewCueKind.video.kindJSON(patch: nil, stage: "00000000-0000-0000-0000-000000000002"))
        kind.set("duration", duration.map(JSONValue.double) ?? .null)
        return try Cue(json: TestSnapshots.cueJSON(id: id.raw, number: nil, name: "", kind: kind))
    }

    private func media(_ id: ModelID, _ type: String) -> CueMediaDuration {
        CueMediaDuration(
            cue: id, file: nil, trimmed: nil, duration: nil, channels: nil,
            picture: PictureFormat(
                type: type, codec: nil, format: nil, width: 10, height: 10, fps: nil, duration: nil))
    }

    /// An Audio cue copied with a patch the targets' workspace lacks (another workspace's, or one
    /// removed since): the output patch is not offered and not sent; one it has is.
    func testPastePropertiesLeavesOutAPatchTheWorkspaceLacks() throws {
        let snap = try Fixtures.json("server", "welcome")["snapshot"]
        let model = try JSONDecoder().decode(
            WorkspaceModel.self, from: Data(try XCTUnwrap(snap?["workspace"]).text().utf8))
        let known = try XCTUnwrap(model.patches.first?.id)
        func audio(_ id: String, patch: String) throws -> Cue {
            var kind = TestSnapshots.audioKind("a.wav")
            kind.set("patch", .string(patch))
            return try Cue(json: TestSnapshots.cueJSON(id: id, number: nil, name: "", kind: kind))
        }
        let target = try audio("t", patch: known.raw)
        let foreign = try audio("s", patch: "00000000-0000-0000-0000-0000000000ee")
        XCTAssertNil(PasteProperty.offered(from: foreign, to: [target], model: model).first { $0.category == .io })
        XCTAssertTrue(PasteProperty.commands([.outputPatch], from: foreign, to: [target], model: model).isEmpty)
        let own = try audio("o", patch: known.raw)
        XCTAssertEqual(
            PasteProperty.offered(from: own, to: [target], model: model).first { $0.category == .io }?.properties,
            [.outputPatch])
    }

    func testPastePropertiesOffersTheStageAndAStillsDurationButNotAMoviesDuration() throws {
        let still: ModelID = "s"
        let other: ModelID = "t"
        let movie: ModelID = "m"
        let source = try video(still, duration: nil)
        let media = [still: media(still, "still"), other: media(other, "still"), movie: media(movie, "video")]
        let toMovie = PasteProperty.offered(from: source, to: [try video(movie, duration: nil)], media: media)
        XCTAssertEqual(toMovie.first { $0.category == .io }?.properties, [.stage])
        XCTAssertFalse(toMovie.flatMap(\.properties).contains(.duration), "a movie's length is its file's")
        let target = try video(other, duration: 3)
        XCTAssertTrue(
            PasteProperty.offered(from: source, to: [target], media: media).flatMap(\.properties).contains(.duration))
        // "Until stopped" goes over as itself.
        XCTAssertEqual(
            PasteProperty.commands([.duration], from: source, to: [target], media: media).map(\.json),
            [Command.setProperty(other, .kindFields(["duration": .null])).json])
    }

    func testStagePropertyMatchesTheFixtures() throws {
        let cases: [(String, CueProperty)] = [
            ("command_set_cue_stage", .stage("00000000-0000-0000-0000-000000000002")),
            ("command_set_cue_stage_none", .stage(nil)),
        ]
        for (name, property) in cases {
            let encoded = JSONValue.parse(try Command.setProperty(cue, property).json.text())
            XCTAssertEqual(encoded, try Fixtures.json("client", name)["command"] ?? .null, name)
        }
    }

    func testDecodeFailedIsABrokenReason() throws {
        let text = try Fixtures.text("server", "runtime_state_broken_decode_failed")
        guard case .runtime(_, let changes) = try ServerMessage.decode(text),
            case .cueState(_, .broken, let reason)? = changes.first
        else { return XCTFail("not a broken cue state") }
        XCTAssertEqual(reason, .decodeFailed)
    }
}
