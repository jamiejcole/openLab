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

    // MARK: Sound

    private func sounding(_ id: ModelID, tracks: [AudioTrackFormat]) -> CueMediaDuration {
        var m = media(id, "video")
        m.audioTracks = tracks
        m.audioTrack = 0
        return m
    }

    private let stereo = AudioTrackFormat(codec: "lpcm", channels: 2, sampleRate: 48000)

    /// The sound-format line: codec by name (its four-character code when this build has no name
    /// for it), channel count, sample rate in kHz.
    func testTheSoundFormatReadsCodecChannelsAndSampleRate() {
        XCTAssertEqual(VideoIOControls.soundText(stereo), "PCM, 2 channels, 48 kHz")
        XCTAssertEqual(
            VideoIOControls.soundText(AudioTrackFormat(codec: "mp4a", channels: 1, sampleRate: 44100)),
            "AAC, 1 channel, 44.1 kHz")
        XCTAssertEqual(
            VideoIOControls.soundText(AudioTrackFormat(codec: "alac", channels: 6, sampleRate: 88200)),
            "Apple Lossless, 6 channels, 88.2 kHz")
        XCTAssertEqual(
            VideoIOControls.soundText(AudioTrackFormat(codec: "xyz9", channels: 2, sampleRate: 96000)),
            "xyz9, 2 channels, 96 kHz")
    }

    /// A new Video cue takes the patch it is given and that patch's default levels, exactly an
    /// Audio cue's; with no patch it has none and the main level alone.
    func testANewVideoCueTakesThePatchAndAnAudioCuesDefaultLevels() throws {
        let patch = ModelObject(
            id: "00000000-0000-0000-0000-000000000001",
            raw: .object(["outputs": .array([.object([:]), .object([:]), .object([:])])]))
        let video = try XCTUnwrap(NewCueKind.video.kindJSON(patch: patch))
        let audio = try XCTUnwrap(NewCueKind.audio.kindJSON(patch: patch))
        XCTAssertEqual(video["patch"], .string(patch.id.raw))
        XCTAssertEqual(video["levels"], audio["levels"])
        XCTAssertEqual(video["levels"]?["crosspoints"]?.arrayValue?.count, 3)
        XCTAssertEqual(video["audio_track"], .int(0))
        let none = try XCTUnwrap(NewCueKind.video.kindJSON(patch: nil))
        XCTAssertEqual(none["patch"], .null)
        XCTAssertEqual(none["levels"]?["outputs"], .array([]))
        XCTAssertEqual(none["levels"]?["master_db"], .double(0))
        // A patch of more outputs than a file can have channels: only routes the engine takes.
        let wide = ModelObject(id: "w", raw: .object(["outputs": .array(Array(repeating: .object([:]), count: 100))]))
        let levels = NewCueKind.defaultLevels(patch: wide)
        XCTAssertEqual(levels["outputs"]?.arrayValue?.count, 100)
        XCTAssertEqual(levels["crosspoints"]?.arrayValue?.count, LevelSet.maxInputs)
    }

    /// A Video cue's tabs: Levels last, only while its probed movie has sound. A Fade's Levels tab
    /// goes while its Video target has none. Several Video cues have Levels when all have sound.
    @MainActor func testTheLevelsTabFollowsTheMoviesSound() throws {
        let v: ModelID = "v"
        let cue = try video(v, duration: nil)
        var fadeKind = try XCTUnwrap(NewCueKind.fade.kindJSON(patch: nil))
        fadeKind.set("target", .string(v.raw))
        let fade = try Cue(json: TestSnapshots.cueJSON(id: "f", number: nil, name: "", kind: fadeKind))
        var snap = TestSnapshots.emptySnapshot(name: "x")
        var ws = try XCTUnwrap(snap["workspace"])
        var lists = try XCTUnwrap(ws["cue_lists"]?.arrayValue)
        lists[0].set("cues", .array([cue.raw, fade.raw]))
        ws.set("cue_lists", .array(lists))
        snap.set("workspace", ws)
        let model = try JSONDecoder().decode(WorkspaceModel.self, from: Data(try ws.text().utf8))
        let video = ["basics", "triggers", "io", "geometry", "timeLoops"]
        for silent in [[:], [v: media(v, "video")], [v: media(v, "still")], [v: sounding(v, tracks: [])]] {
            XCTAssertEqual(InspectorTabs.ids(for: cue, model: model, media: silent), video)
            XCTAssertEqual(InspectorTabs.ids(for: fade, model: model, media: silent), ["basics", "triggers", "curve"])
        }
        let heard = [v: sounding(v, tracks: [stereo])]
        XCTAssertEqual(InspectorTabs.ids(for: cue, model: model, media: heard), video + ["levels"])
        XCTAssertEqual(
            InspectorTabs.ids(for: fade, model: model, media: heard), ["basics", "triggers", "curve", "fadeLevels"])
        XCTAssertEqual(InspectorViewController.batchTabs(forKinds: ["video"]), ["basics", "triggers", "io"])
        XCTAssertEqual(
            InspectorViewController.batchTabs(forKinds: ["video"], allSound: true),
            ["basics", "triggers", "io", "levels"])
        XCTAssertEqual(
            InspectorViewController.batchTabs(forKinds: ["video", "audio"], allSound: true), ["basics", "triggers"])
    }

    /// Paste Properties: levels and the output patch go between Audio cues and Video cues with
    /// sound, either way; never from or to a Video cue without sound, whose levels are shown
    /// nowhere. A Video cue's patch of none goes to Video cues only.
    func testPastePropertiesTakesLevelsAndPatchBetweenCuesWithSound() throws {
        let a: ModelID = "a"
        let v: ModelID = "v"
        let q: ModelID = "q"
        let patch = "00000000-0000-0000-0000-000000000001"
        var audioKind = TestSnapshots.audioKind("a.wav")
        audioKind.set("patch", .string(patch))
        let audio = try Cue(json: TestSnapshots.cueJSON(id: a.raw, number: nil, name: "", kind: audioKind))
        let withSound = try video(v, duration: nil)
        let quiet = try video(q, duration: nil)
        let media = [v: sounding(v, tracks: [stereo]), q: media(q, "video")]
        func offered(_ from: Cue, _ to: Cue) -> [PasteProperty] {
            PasteProperty.offered(from: from, to: [to], media: media).flatMap(\.properties).filter {
                $0 == .levels || $0 == .outputPatch
            }
        }
        XCTAssertEqual(offered(audio, withSound), [.outputPatch, .levels])
        XCTAssertEqual(offered(withSound, audio), [.levels], "no patch to give: an Audio cue always has one")
        XCTAssertEqual(offered(withSound, withSound), [.outputPatch, .levels])
        XCTAssertEqual(offered(audio, quiet), [])
        XCTAssertEqual(offered(quiet, audio), [])
        XCTAssertEqual(offered(quiet, withSound), [])
        XCTAssertEqual(
            PasteProperty.commands([.outputPatch], from: audio, to: [withSound], media: media).map(\.json),
            [Command.setProperty(v, .kindFields(["patch": .string(patch)])).json])
        XCTAssertEqual(
            PasteProperty.commands([.outputPatch], from: withSound, to: [withSound], media: media).map(\.json),
            [Command.setProperty(v, .kindFields(["patch": .null])).json], "none goes over as none")
        // The levels are fitted to the target's own outputs, as between Audio cues.
        let sent = PasteProperty.commands([.levels], from: audio, to: [withSound], media: media)
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(
            sent.first?.json["property"]?["value"]?["levels"]?["master_db"], audio.raw["kind"]?["levels"]?["master_db"])
    }
}
