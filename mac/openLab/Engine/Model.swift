// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// A model id (cue, list, patch, workspace): the engine's UUID string, kept verbatim so a
/// re-encoded message equals what the engine sent.
struct ModelID: Hashable, Sendable, Codable, ExpressibleByStringLiteral, CustomStringConvertible {
    let raw: String
    init(_ raw: String) { self.raw = raw }
    init(stringLiteral value: String) { raw = value }
    init(from decoder: Decoder) throws { raw = try decoder.singleValueContainer().decode(String.self) }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(raw)
    }
    var description: String { raw }

    /// The id a Network cue's destination holds when the inspector's "no patch" is chosen. No patch
    /// has it, so the engine shows the cue broken (`network_patch_missing`) and it never sends.
    static let unpatched = ModelID("00000000-0000-0000-0000-000000000000")
}

/// A model object the UI only needs the id of for now (patches, network patches).
struct ModelObject: Hashable, Sendable, Decodable {
    let id: ModelID
    let raw: JSONValue
    init(id: ModelID, raw: JSONValue) {
        self.id = id
        self.raw = raw
    }
    init(from decoder: Decoder) throws {
        let v = try JSONValue(from: decoder)
        guard let id = v["id"]?.stringValue else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "no id"))
        }
        self.id = ModelID(id)
        raw = v
    }
}

/// A cue. `raw` is the engine's cue object without the group's `kind.children`, which are held as
/// `children` so the tree can be edited by id.
struct Cue: Hashable, Sendable, Decodable {
    let id: ModelID
    var raw: JSONValue
    var children: [Cue]

    var number: String? { raw["number"]?.stringValue }
    var name: String { raw["name"]?.stringValue ?? "" }
    var kindType: String { raw["kind"]?["type"]?.stringValue ?? "" }
    /// An Audio or Video cue: its target is a file (`media`), not a cue.
    var takesFile: Bool { kindType == "audio" || kindType == "video" }
    /// The cue has sound with levels and an output patch of its own: an Audio cue, or a Video cue
    /// whose movie, as probed (`media`), has an audio track. A still, a movie without sound and a
    /// file not probed yet have none.
    func sounds(_ media: CueMediaDuration?) -> Bool {
        kindType == "audio" || kindType == "video" && media?.hasSound == true
    }
    /// The post-wait the cue runs with: a Wait cue's is its duration (core's `effective_post_wait`).
    var postWait: Double {
        kindType == "wait" ? raw["kind"]?["duration"]?.doubleValue ?? 0 : raw["post_wait"]?.doubleValue ?? 0
    }

    /// The name to show (core's `display_name`): the user's name if set, else an Audio or Video
    /// cue's file name with its extension (last `/` component, as stored), else empty. Editing
    /// writes `name`.
    var displayName: String {
        let typed = name
        if !typed.isEmpty { return typed }
        guard kindType == "audio" || kindType == "video", let media = raw["kind"]?["media"]?.stringValue else {
            return ""
        }
        // Split on scalars, as core splits on the byte: "/" followed by a combining mark is still a separator.
        var scalars = Substring(media).unicodeScalars
        while scalars.last == "/" { scalars = scalars.dropLast() }
        let name = scalars.split(separator: "/", omittingEmptySubsequences: false).last ?? Substring.UnicodeScalarView()
        return String(name)
    }

    /// Kinds whose default name is the kind and the name of the cue they act on (QLab 5, manual pp.
    /// 93, 126: Fade, Start, Stop, Pause, Load, Reset, Devamp, Go To, Target, Arm, Disarm).
    static let namedAfterTarget: Set<String> = [
        "fade", "start", "stop", "pause", "load", "reset", "go_to", "target", "arm", "disarm", "devamp",
    ]

    /// The default name of an unnamed cue that acts on another: its kind and the target's shown name
    /// ("fade and stop ..." for a Fade that stops its target), empty when the cue has no such name
    /// or its target is missing. `lookup` finds a cue by id. Never stored: it follows the target.
    func targetDefaultName(lookup: (ModelID) -> Cue?) -> String {
        targetDefaultName(lookup: lookup, depth: 0)
    }

    private func targetDefaultName(lookup: (ModelID) -> Cue?, depth: Int) -> String {
        // A Wait reads its length and a Network cue its message (QLab 5, manual p. 93).
        if name.isEmpty, kindType == "wait" {
            return String(format: loc("cue.name.wait"), CueTime.running(raw["kind"]?["duration"]?.doubleValue ?? 0))
        }
        if name.isEmpty, kindType == "network" {
            let address = raw["kind"]?["address"]?.stringValue ?? ""
            return address.isEmpty
                ? "" : NetworkMessage.format(address: address, args: raw["kind"]?["args"]?.arrayValue ?? [])
        }
        guard name.isEmpty, Self.namedAfterTarget.contains(kindType), depth < 8,
            let id = raw["kind"]?["target"]?.stringValue.map({ ModelID($0) }), let target = lookup(id)
        else { return "" }
        var shown = target.displayName
        if shown.isEmpty { shown = target.targetDefaultName(lookup: lookup, depth: depth + 1) }
        if shown.isEmpty { shown = CueKindStyle.kindLabel(target.kindType) }
        let stops = kindType == "fade" && raw["kind"]?["stop_target_when_done"] == .bool(true)
        return String(
            format: loc(stops ? "cue.name.fadeAndStop" : "cue.name.afterTarget"),
            CueKindStyle.kindLabel(kindType).lowercased(), shown)
    }

    /// Whether the name this cue shows follows one of `ids`: one of them is further along its chain
    /// of targets (a default name is made from its target's, which may be a default name too).
    func followsName(of ids: Set<ModelID>, lookup: (ModelID) -> Cue?) -> Bool {
        var seen: Set<ModelID> = [id]
        var next = raw["kind"]?["target"]?.stringValue.map { ModelID($0) }
        while let t = next, seen.insert(t).inserted {
            if ids.contains(t) { return true }
            guard let target = lookup(t), Self.namedAfterTarget.contains(target.kindType) else { return false }
            next = target.raw["kind"]?["target"]?.stringValue.map { ModelID($0) }
        }
        return false
    }

    /// The name every surface shows for the cue (list, standby, Find, messages): its own name, an
    /// Audio cue's file name, else the default name of a cue that acts on another; empty for none.
    func shownName(lookup: (ModelID) -> Cue?) -> String {
        let own = displayName
        return own.isEmpty ? targetDefaultName(lookup: lookup) : own
    }

    /// `shownName`, or the kind's label when that is empty.
    func listName(lookup: (ModelID) -> Cue?) -> String {
        let shown = shownName(lookup: lookup)
        return shown.isEmpty ? CueKindStyle.kindLabel(kindType) : shown
    }

    init(from decoder: Decoder) throws {
        try self.init(json: try JSONValue(from: decoder))
    }

    init(json: JSONValue) throws {
        guard case .object(var o) = json, let id = o["id"]?.stringValue else {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "cue has no id"))
        }
        self.id = ModelID(id)
        children = []
        if case .object(var kind)? = o["kind"], let kids = kind.removeValue(forKey: "children") {
            children = try (kids.arrayValue ?? []).map { try Cue(json: $0) }
            o["kind"] = .object(kind)
        }
        raw = .object(o)
    }
}

/// An audio cue's media lengths in seconds (`media_duration`): each nil when not known (not
/// probed, missing, unplayable; `trimmed` and `duration` also when its slices cannot play).
/// `duration` counts the slices' play counts and is nil when one loops until devamped. Runtime data
/// from the engine's probes, not part of the cue.
struct CueMediaDuration: Hashable, Sendable, Decodable {
    let cue: ModelID
    let file: Double?
    let trimmed: Double?
    let duration: Double?
    /// How many channels the probed file has, as the engine sent it (nil: not known).
    let channels: Int?
    /// A Video cue's file as its probe read it (nil for an Audio cue, or not known).
    var picture: PictureFormat? = nil
    /// An Audio cue's file as its probe read it (nil for a Video cue, or not known).
    var audioFile: AudioFileFormat? = nil
    /// The audio tracks of a Video cue's movie, in file order (nil: it has no sound, or the cue is
    /// not a Video cue with a probed movie).
    var audioTracks: [AudioTrackFormat]? = nil
    /// Which of `audioTracks` the cue plays, from 0; `channels` is that track's.
    var audioTrack: Int? = nil

    enum CodingKeys: String, CodingKey {
        case cue, file, trimmed, duration, channels, picture
        case audioFile = "audio_file"
        case audioTracks = "audio_tracks"
        case audioTrack = "audio_track"
    }

    /// A Video cue's movie has an audio track.
    var hasSound: Bool { audioTracks?.isEmpty == false }

    /// The length the Duration column and field show: the cue's own `duration`, or for a cue that loops
    /// until devamped (none sent) one pass through its slices, so it never reads as an unprobed file.
    var shownDuration: Double? { duration ?? trimmed }

    /// The matrix rows this file gives: `channels` when it is a count a file can have, else nil. The
    /// engine only sends 1 to 64; anything else from a peer is treated as not known.
    var fileChannels: Int? {
        channels.flatMap { (1...LevelSet.maxInputs).contains($0) ? $0 : nil }
    }
}

/// A Video cue's file as the engine's probe read it: a video (`type` "video": codec `h264`, `hevc`,
/// `prores`, `hap`, `hap_alpha` or `hap_q`, frame rate and length) or a still (`type` "still":
/// format `png`, `jpeg` or `tiff`), each with its pixel size.
struct PictureFormat: Hashable, Sendable, Decodable {
    let type: String
    let codec: String?
    let format: String?
    let width: Int
    let height: Int
    let fps: Double?
    let duration: Double?
}

/// An Audio cue's file as the engine's probe read it: its codec's short name (`pcm`, `pcm_float`,
/// `flac`, `alac`, `aac`, `mp1`, `mp2`, `mp3`, `vorbis`, `opus`, `other`), channel count and sample rate.
struct AudioFileFormat: Hashable, Sendable, Decodable {
    let codec: String
    let channels: Int
    let sampleRate: Int

    enum CodingKeys: String, CodingKey {
        case codec, channels
        case sampleRate = "sample_rate"
    }
}

/// One audio track of a movie as the engine's probe read it: its sample format's four-character
/// code (`mp4a`, `lpcm`, ...), channel count and sample rate.
struct AudioTrackFormat: Hashable, Sendable, Decodable {
    let codec: String
    let channels: Int
    let sampleRate: Int

    enum CodingKeys: String, CodingKey {
        case codec, channels
        case sampleRate = "sample_rate"
    }
}

struct CueList: Hashable, Sendable, Decodable {
    let id: ModelID
    var name: String
    var cues: [Cue]
}

/// The mirrored show: what `snapshot.workspace` holds.
struct WorkspaceModel: Sendable, Decodable {
    var id: ModelID
    var name: String
    var settings: JSONValue
    var patches: [ModelObject]
    var networkPatches: [ModelObject]
    /// VID-01: the video output routes (`{id, name, width, height, rotation, scaling}`); absent
    /// from a workspace with none.
    var routes: [ModelObject] = []
    /// VID-01: the video stages (`{id, name, width, height, keep_black, layer, regions}`, each
    /// region `{id, route?, x, y, width, height}`); absent from a workspace with none.
    var stages: [ModelObject] = []
    var cueLists: [CueList]
    /// The names of stages removed since this model was read (not part of the wire model), so a
    /// cue left on one can say which stage it misses.
    var formerStageNames: [ModelID: String] = [:]

    enum CodingKeys: String, CodingKey {
        case id, name, settings, patches, routes, stages
        case networkPatches = "network_patches"
        case cueLists = "cue_lists"
    }

    /// The hotkey object of `key` in `settings.hotkeys` (docs/file-format.md section 4.2); nil
    /// when the key has none (null) or the engine's map predates it (format v4 had no
    /// selected-cue keys).
    func hotkey(_ key: WorkspaceKey) -> JSONValue? {
        guard let value = settings["hotkeys"]?[key.rawValue], value != .null else { return nil }
        return value
    }

    func shownName(_ cue: Cue) -> String { cue.shownName(lookup: { self.cue($0) }) }
    func listName(_ cue: Cue) -> String { cue.listName(lookup: { self.cue($0) }) }

    /// Every cue of every list, groups' children included (the footer's count).
    var cueCount: Int {
        func count(_ cues: [Cue]) -> Int { cues.reduce(0) { $0 + 1 + count($1.children) } }
        return cueLists.reduce(0) { $0 + count($1.cues) }
    }

    /// The cue with `id` anywhere in any list.
    func cue(_ id: ModelID) -> Cue? {
        func find(_ cues: [Cue]) -> Cue? {
            for c in cues {
                if c.id == id { return c }
                if let hit = find(c.children) { return hit }
            }
            return nil
        }
        for l in cueLists { if let hit = find(l.cues) { return hit } }
        return nil
    }
}

extension WorkspaceModel {
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(ModelID.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        settings = try c.decode(JSONValue.self, forKey: .settings)
        patches = try c.decode([ModelObject].self, forKey: .patches)
        networkPatches = try c.decode([ModelObject].self, forKey: .networkPatches)
        routes = try c.decodeIfPresent([ModelObject].self, forKey: .routes) ?? []
        stages = try c.decodeIfPresent([ModelObject].self, forKey: .stages) ?? []
        cueLists = try c.decode([CueList].self, forKey: .cueLists)
    }
}

struct DocumentState: Hashable, Sendable, Decodable {
    var path: String?
    var dirty: Bool
    var canUndo: Bool
    var canRedo: Bool
    /// What the step the next undo and redo apply is called (docs/protocol.md `document`): a
    /// code the Edit menu words (`undo.step.<code>`); nil, or a code it does not know: no name.
    var undoName: String?
    var redoName: String?
    /// The engine holds this workspace in Show Mode (SM-07 sleep hold); absent means false.
    var showMode: Bool
    enum CodingKeys: String, CodingKey {
        case path, dirty
        case canUndo = "can_undo"
        case canRedo = "can_redo"
        case undoName = "undo_name"
        case redoName = "redo_name"
        case showMode = "show_mode"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decodeIfPresent(String.self, forKey: .path)
        dirty = try c.decode(Bool.self, forKey: .dirty)
        canUndo = try c.decode(Bool.self, forKey: .canUndo)
        canRedo = try c.decode(Bool.self, forKey: .canRedo)
        undoName = try c.decodeIfPresent(String.self, forKey: .undoName)
        redoName = try c.decodeIfPresent(String.self, forKey: .redoName)
        showMode = try c.decodeIfPresent(Bool.self, forKey: .showMode) ?? false
    }
}

/// `{duration, curve}`; the curve stays a string so a new curve does not break decoding.
struct FadeSpec: Hashable, Sendable, Codable {
    var duration: Double
    var curve: String
}

enum Upsert: Sendable, Decodable {
    case cue(list: ModelID, parent: ModelID?, index: Int, cue: Cue)
    case cueList(id: ModelID, name: String, index: Int)
    case patch(index: Int, patch: ModelObject)
    case networkPatch(index: Int, patch: ModelObject)
    case stage(index: Int, stage: ModelObject)
    case route(index: Int, route: ModelObject)
    case settings(name: String, settings: JSONValue)
    /// A kind this client does not know: the model is now unreliable, so the caller resyncs.
    case unknown(String)

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        switch try c.decode(String.self, forKey: "type") {
        case "cue":
            self = .cue(
                list: try c.decode(ModelID.self, forKey: "list"),
                parent: try c.decodeIfPresent(ModelID.self, forKey: "parent"),
                index: try c.decode(Int.self, forKey: "index"), cue: try c.decode(Cue.self, forKey: "cue"))
        case "cue_list":
            self = .cueList(
                id: try c.decode(ModelID.self, forKey: "id"), name: try c.decode(String.self, forKey: "name"),
                index: try c.decode(Int.self, forKey: "index"))
        case "patch":
            self = .patch(
                index: try c.decode(Int.self, forKey: "index"), patch: try c.decode(ModelObject.self, forKey: "patch"))
        case "network_patch":
            self = .networkPatch(
                index: try c.decode(Int.self, forKey: "index"), patch: try c.decode(ModelObject.self, forKey: "patch"))
        case "stage":
            self = .stage(
                index: try c.decode(Int.self, forKey: "index"), stage: try c.decode(ModelObject.self, forKey: "stage"))
        case "route":
            self = .route(
                index: try c.decode(Int.self, forKey: "index"), route: try c.decode(ModelObject.self, forKey: "route"))
        case "settings":
            self = .settings(
                name: try c.decode(String.self, forKey: "name"),
                settings: try c.decode(JSONValue.self, forKey: "settings"))
        case let other:
            self = .unknown(other)
        }
    }
}

enum Delete: Sendable, Decodable {
    case cue(ModelID)
    case cueList(ModelID)
    case patch(ModelID)
    case networkPatch(ModelID)
    case stage(ModelID)
    case route(ModelID)
    case unknown(String)

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let id = try c.decode(ModelID.self, forKey: "id")
        switch try c.decode(String.self, forKey: "type") {
        case "cue": self = .cue(id)
        case "cue_list": self = .cueList(id)
        case "patch": self = .patch(id)
        case "network_patch": self = .networkPatch(id)
        case "stage": self = .stage(id)
        case "route": self = .route(id)
        case let other: self = .unknown(other)
        }
    }
}
