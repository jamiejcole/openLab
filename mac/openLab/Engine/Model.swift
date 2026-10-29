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
}

/// A model object the UI only needs the id of for now (patches, network patches).
struct ModelObject: Hashable, Sendable, Decodable {
    let id: ModelID
    let raw: JSONValue
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

    /// The name to show (core's `display_name`, M2.29c): the user's name if set, else an Audio cue's
    /// file name with its extension (last `/` component, as stored), else empty. Editing writes `name`.
    var displayName: String {
        let typed = name
        if !typed.isEmpty { return typed }
        guard kindType == "audio", let media = raw["kind"]?["media"]?.stringValue else { return "" }
        // Split on scalars, as core splits on the byte: "/" followed by a combining mark is still a separator.
        var scalars = Substring(media).unicodeScalars
        while scalars.last == "/" { scalars = scalars.dropLast() }
        let name = scalars.split(separator: "/", omittingEmptySubsequences: false).last ?? Substring.UnicodeScalarView()
        return String(name)
    }

    /// `displayName`, or the kind's label when that is empty (a cue with neither name nor file).
    var listName: String { displayName.isEmpty ? CueKindStyle.kindLabel(kindType) : displayName }

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

/// An audio cue's media lengths in seconds (`media_duration`, M5.4d): each nil when not known (not
/// probed, missing, unplayable; `trimmed` and `duration` also when its slices cannot play).
/// `duration` counts the slices' play counts and is nil when one loops until devamped. Runtime data
/// from the engine's probes, not part of the cue.
struct CueMediaDuration: Hashable, Sendable, Decodable {
    let cue: ModelID
    let file: Double?
    let trimmed: Double?
    let duration: Double?
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
    var cueLists: [CueList]

    enum CodingKeys: String, CodingKey {
        case id, name, settings, patches
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

struct DocumentState: Hashable, Sendable, Decodable {
    var path: String?
    var dirty: Bool
    var canUndo: Bool
    var canRedo: Bool
    /// The engine holds this workspace in Show Mode (SM-07 sleep hold); absent means false.
    var showMode: Bool
    enum CodingKeys: String, CodingKey {
        case path, dirty
        case canUndo = "can_undo"
        case canRedo = "can_redo"
        case showMode = "show_mode"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decodeIfPresent(String.self, forKey: .path)
        dirty = try c.decode(Bool.self, forKey: .dirty)
        canUndo = try c.decode(Bool.self, forKey: .canUndo)
        canRedo = try c.decode(Bool.self, forKey: .canRedo)
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
    case unknown(String)

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let id = try c.decode(ModelID.self, forKey: "id")
        switch try c.decode(String.self, forKey: "type") {
        case "cue": self = .cue(id)
        case "cue_list": self = .cueList(id)
        case "patch": self = .patch(id)
        case "network_patch": self = .networkPatch(id)
        case let other: self = .unknown(other)
        }
    }
}
